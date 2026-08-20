//! Lower `gen fn` / `yield` into ordinary functions over `std/iter`.
//!
//! A generator
//! ```text
//! gen fn count_up(n: Int) -> Iter(Int):
//!     var i = n
//!     while true:
//!         yield i
//!         i = i + 1
//! ```
//! becomes two plain functions. A single-yield loop gets an owned tuple frame
//! advanced once per `iter.unfold` pull. Irregular control flow retains the
//! compatibility helper below, which re-runs the body to the requested yield:
//! ```text
//! fn __gen_count_up(n: Int, __target: Int) -> Option(Int):
//!     var __i = 0
//!     var i = n
//!     while true:
//!         if __i == __target:
//!             return Some(i)
//!         __i = __i + 1
//!         i = i + 1
//!     None
//! fn count_up(n: Int) -> Iter(Int):
//!     iter.from_gen(fn(__t: Int): __gen_count_up(n, __t))
//! ```
//! Pulling element `k` re-runs the body to the `k`-th yield, so the body may use
//! any control flow (including unbounded loops and mutable state across yields).
//! Generators should be capability-free, since re-running repeats side effects.

use crate::ast::*;
use crate::source_check::{GeneratorsLoweredModule, SourceCheckedModule};

const COUNTER: &str = "__i";
const TARGET: &str = "__target";

/// Map each source item to the item positions it occupies after generator
/// lowering. Origin tables use this before the destructive rewrite so helper
/// and wrapper items retain the source item's ancestry.
pub(crate) fn lowered_item_mapping(module: &Module) -> Vec<Vec<usize>> {
    let mut next = 0usize;
    module
        .items
        .iter()
        .map(|item| {
            let count = match item {
                Item::Function(function) if function.is_gen => 2,
                Item::Impl(block) => {
                    block.methods.iter().filter(|method| method.is_gen).count() + 1
                }
                _ => 1,
            };
            let mapped = (next..next + count).collect();
            next += count;
            mapped
        })
        .collect()
}

/// Rewrite every `gen fn` in the module into a helper + wrapper, and ensure the
/// `iter`/`option` modules it relies on are imported. A no-op for modules
/// without generators.
pub fn lower(mut checked: SourceCheckedModule) -> Result<GeneratorsLoweredModule, String> {
    if !has_generator(checked.module()) {
        return Ok(GeneratorsLoweredModule::preserve(checked.into_module()));
    }
    let module = checked.module_mut();
    let mut items = Vec::with_capacity(module.items.len() + 1);
    let mut state_counter = 0usize;
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Function(f) if f.is_gen => {
                let (helper, wrapper) = lower_gen(f, None, &mut state_counter)?;
                items.push(Item::Function(wrapper));
                items.push(Item::Function(helper));
            }
            // A `gen fn` METHOD in an inherent `impl Type:` block lowers exactly
            // like a top-level one, but its wrapper STAYS a method (so
            // `value.method()` still resolves by receiver type and returns
            // `Iter(a)`); only the helper is hoisted to a top-level function, under
            // a type-qualified name so two types' identically-named generators can't
            // collide. Trait-impl `gen` methods are rejected at parse time, so every
            // impl reaching here is inherent. The enumeration is kept separate from
            // `lower_gen`'s body transform, so a later rewrite of the transform
            // (RFC-0059) leaves this traversal untouched.
            Item::Impl(mut im) if im.methods.iter().any(|m| m.is_gen) => {
                let ctx = MethodCtx {
                    self_ty: impl_self_ty(&im),
                    bounds: im.bounds.clone(),
                    type_name: im.type_name.clone(),
                };
                let mut methods = Vec::with_capacity(im.methods.len());
                for method in std::mem::take(&mut im.methods) {
                    if method.is_gen {
                        let (helper, wrapper) =
                            lower_gen(method, Some(&ctx), &mut state_counter)?;
                        items.push(Item::Function(helper));
                        methods.push(wrapper);
                    } else {
                        methods.push(method);
                    }
                }
                im.methods = methods;
                items.push(Item::Impl(im));
            }
            other => items.push(other),
        }
    }
    module.items = items;
    for dep in ["iter", "option"] {
        if !module.imports.iter().any(|m| m == dep) {
            module.imports.push(dep.to_string());
        }
    }
    // `import_lines` is parallel to `imports`; keep them the same length so the
    // formatter's comment placement stays consistent (0 = unknown line).
    while module.import_lines.len() < module.imports.len() {
        module.import_lines.push(0);
    }
    Ok(GeneratorsLoweredModule::preserve(checked.into_module()))
}

/// Validate the generator rules that lowering would otherwise erase.
pub(crate) fn validate_source(
    module: &Module,
) -> Result<(), crate::source_check::SourceValidationError> {
    for (item_index, item) in module.items.iter().enumerate() {
        match item {
            Item::Function(function) if function.is_gen => {
                validate_generator_return(function)
                    .map_err(|message| crate::source_check::SourceValidationError::new(
                        item_index, message,
                    ))?;
                validate_generator_reference_boundary(function)
                    .map_err(|message| crate::source_check::SourceValidationError::new(
                        item_index, message,
                    ))?;
                validate_generator_block(&function.body, &function.name, false)
                    .map_err(|message| crate::source_check::SourceValidationError::new(
                        item_index, message,
                    ))?;
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    if method.is_gen {
                        validate_generator_return(method)
                            .map_err(|message| crate::source_check::SourceValidationError::new(
                                item_index, message,
                            ))?;
                        validate_generator_reference_boundary(method)
                            .map_err(|message| crate::source_check::SourceValidationError::new(
                                item_index, message,
                            ))?;
                        validate_generator_block(&method.body, &method.name, false)
                            .map_err(|message| crate::source_check::SourceValidationError::new(
                                item_index, message,
                            ))?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_generator_return(function: &Function) -> Result<(), String> {
    if iter_elem(&function.ret).is_some() {
        return Ok(());
    }
    Err(format!(
        "generator `{}` must declare exactly one element type as `-> Iter(a)`",
        function.name
    ))
}

/// A generator frame can outlive the call that created it, so source references
/// may not enter through parameters or the yielded element type.
fn validate_generator_reference_boundary(function: &Function) -> Result<(), String> {
    let parameter_reference = function
        .params
        .iter()
        .filter_map(|parameter| parameter.ty.as_ref())
        .any(type_carries_reference);
    let result_reference = iter_elem(&function.ret)
        .as_ref()
        .is_some_and(type_carries_reference);
    if parameter_reference || result_reference {
        return Err(format!(
            "gen fn `{}` may not expose a borrowed view or explicit reference as a parameter or yielded element because its generator frame can outlive the caller's loan — pass or yield an owned value",
            function.name,
        ));
    }
    Ok(())
}

fn type_carries_reference(ty: &Type) -> bool {
    match ty {
        Type::Qualified(
            TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_),
            _,
        ) => true,
        Type::Qualified(_, inner) => type_carries_reference(inner),
        Type::Named(_, args) | Type::Dyn(_, args) => args.iter().any(type_carries_reference),
        Type::Tuple(items) => items.iter().any(type_carries_reference),
        Type::RecordCompose { base, fields } => {
            type_carries_reference(base)
                || fields.iter().any(|(_, field)| type_carries_reference(field))
        }
        Type::Fn(params, result, _) => {
            params.iter().any(type_carries_reference) || type_carries_reference(result)
        }
    }
}

fn validate_generator_block(block: &Block, name: &str, in_region: bool) -> Result<(), String> {
    let in_region = in_region || block.region.is_some();
    for statement in &block.stmts {
        match statement {
            Stmt::Yield(_) if in_region => {
                return Err(
                    "cannot `yield` inside `region:`: the generator frame outlives the region"
                        .to_string(),
                );
            }
            Stmt::Return(Some(_)) => {
                return Err(format!(
                    "`return <value>` is not allowed in generator `{name}`: a `gen fn` \
                     declares `-> Iter(a)` and produces its elements with `yield` — use a bare \
                     `return` to end the stream early"
                ));
            }
            Stmt::Let { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value) => validate_generator_expr(value, name, in_region)?,
            Stmt::Yield(_) | Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn validate_generator_expr(expr: &Expr, name: &str, in_region: bool) -> Result<(), String> {
    match expr {
        Expr::If { then_block, else_block, .. } => {
            validate_generator_block(then_block, name, in_region)?;
            if let Some(block) = else_block {
                validate_generator_block(block, name, in_region)?;
            }
        }
        Expr::While { body, .. } | Expr::For { body, .. } | Expr::Block(body) => {
            validate_generator_block(body, name, in_region)?;
        }
        Expr::Match { arms, .. } => {
            for arm in arms {
                validate_generator_expr(&arm.body, name, in_region)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether the module contains any `gen fn` — top level or as a method in an
/// `impl` block (both are lowered by [`lower`]).
fn has_generator(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.is_gen,
        Item::Impl(im) => im.methods.iter().any(|m| m.is_gen),
        _ => false,
    })
}

/// The impl context a `gen fn` method needs to lower into a top-level helper: the
/// receiver type (to type the helper's `self`), the impl's `where` bounds (so a
/// generic impl's helper monomorphizes per element), and the type name (to
/// disambiguate the helper's name across types).
struct MethodCtx {
    self_ty: Type,
    bounds: Vec<(String, String, Vec<Type>)>,
    type_name: String,
}

/// The type a method's `self` stands for in an `impl`: `List(a)` for a generic
/// target, a tuple for `impl … for (a, b)`, a bare head otherwise. Mirrors
/// `traits::method_fn`'s self-typing so a hoisted generator helper (an ordinary
/// top-level function) type-checks without having to infer its receiver.
fn impl_self_ty(im: &ImplDef) -> Type {
    if im.type_name.starts_with("Tuple") {
        Type::Tuple(im.target_args.clone())
    } else {
        Type::Named(im.type_name.clone(), im.target_args.clone())
    }
}

/// The element type `a` of a `-> Iter(a)` return annotation, if present.
fn iter_elem(ret: &Option<Type>) -> Option<Type> {
    match ret {
        Some(Type::Named(n, args)) if n == "Iter" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

/// Lower one `gen fn` into (helper, wrapper). `method` is `Some` when `f` is a
/// method of an inherent `impl`: the helper is then named per-type (so two types'
/// same-named generators don't collide), its `self` receiver is typed to the impl
/// type, and it carries the impl's bounds — while the wrapper is returned to be
/// re-inserted into `impl.methods`, preserving method identity.
fn lower_gen(
    f: Function,
    method: Option<&MethodCtx>,
    state_counter: &mut usize,
) -> Result<(Function, Function), String> {
    let elem = iter_elem(&f.ret);
    let entry_state = *state_counter;
    *state_counter += 1;
    let resume_state = *state_counter;
    *state_counter += 1;
    let helper_name = match method {
        Some(ctx) => format!("__gen_{}_{}", ctx.type_name, f.name),
        None => format!("__gen_{}", f.name),
    };
    if let Some(lowered) = lower_owned_loop_frame(
        &f,
        method,
        &helper_name,
        entry_state,
        resume_state,
    )? {
        return Ok(lowered);
    }

    // Helper params: the original params plus `__target: Int`.
    let mut helper_params = f.params.clone();
    helper_params.push(Param {
        name: TARGET.to_string(),
        ty: Some(Type::Named("Int".to_string(), vec![])),
        convention: Convention::Let,
        default: None,
    });
    // For an impl method the receiver `self` is unannotated after parsing (the
    // trait/impl pass types it later, but that pass never sees this hoisted
    // helper). Type it here to the implementing type so the helper checks.
    if let Some(ctx) = method {
        if let Some(first) = helper_params.first_mut() {
            if first.ty.is_none() {
                first.ty = Some(ctx.self_ty.clone());
            }
        }
    }

    // Helper body: `var __i = 0` + the body with yields rewritten + final `None`.
    let mut stmts = vec![Stmt::Let {
        name: COUNTER.to_string(),
        ty: None,
        mutable: true,
        value: Expr::Int(0),
    }];
    stmts.extend(rewrite_block(f.body.clone(), &f.name, false)?.stmts);
    stmts.push(Stmt::Expr(Expr::Ctor { name: "None".to_string(), args: vec![] }));
    let mut helper_attributes = f.attributes.clone();
    helper_attributes.push(crate::suspension::FRAME_FUNCTION_ATTRIBUTE.into());
    helper_attributes.push(crate::suspension::frame_state_attribute(resume_state));
    let helper = Function {
        line: f.line,
        public: false,
        comptime_only: false,
        attributes: helper_attributes,
        name: helper_name.clone(),
        params: helper_params,
        ret: elem.as_ref().map(|a| Type::Named("Option".to_string(), vec![a.clone()])),
        body: Block { stmts, lines: Vec::new(), region: None },
        // A generic impl's helper is monomorphized through its bounds, exactly
        // like the wrapper method (`traits::method_fn` re-applies `impl.bounds`).
        bounds: method.map_or_else(|| f.bounds.clone(), |ctx| ctx.bounds.clone()),
        is_gen: false,
        is_async: false,
    };

    // Wrapper: `f(params) -> Iter(a): iter.from_gen(fn(__t: Int): __gen_f(args, __t))`.
    let forwarded: Vec<Expr> = f
        .params
        .iter()
        .map(|p| Expr::Var(p.name.clone()))
        .chain(std::iter::once(Expr::Var(TARGET.to_string())))
        .collect();
    let thunk = Expr::Lambda {
        params: vec![Param {
            name: TARGET.to_string(),
            ty: Some(Type::Named("Int".to_string(), vec![])),
            convention: Convention::Let,
            default: None,
        }],
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call { name: helper_name, args: forwarded })],
            lines: vec![0],
            region: None,
        },
        ret: None,
    };
    let mut wrapper_attributes = f.attributes;
    wrapper_attributes.push(crate::suspension::FRAME_ENTRY_ATTRIBUTE.into());
    wrapper_attributes.push(crate::suspension::frame_state_attribute(entry_state));
    let wrapper = Function {
        line: f.line,
        public: f.public,
        comptime_only: false,
        attributes: wrapper_attributes,
        name: f.name,
        params: f.params,
        ret: f.ret,
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: "iter.from_gen".to_string(),
                args: vec![thunk],
            })],
            lines: vec![0],
            region: None,
        },
        bounds: f.bounds,
        is_gen: false,
        is_async: false,
    };

    Ok((helper, wrapper))
}

#[derive(Clone)]
struct GeneratorFrameBinding {
    name: String,
    ty: Type,
    mutable: bool,
}

/// Lower the common imperative generator shape to an actual one-pass owned
/// frame instead of replaying the body to the Nth yield:
///
/// ```text
/// <typed params>
/// <let/var initializers>
/// while condition:
///     <before>
///     yield value
///     <after>
/// ```
///
/// The frame is a fixed typed tuple of parameters and initialized locals. One
/// `iter.unfold` step restores those bindings, executes exactly one loop
/// iteration, and returns the yielded value plus the next owned frame. More
/// general CFGs retain the replay fallback until they are split into states.
fn lower_owned_loop_frame(
    f: &Function,
    method: Option<&MethodCtx>,
    helper_name: &str,
    entry_state: usize,
    resume_state: usize,
) -> Result<Option<(Function, Function)>, String> {
    let Some(elem) = iter_elem(&f.ret) else { return Ok(None) };
    let Some((last, prelude)) = f.body.stmts.split_last() else { return Ok(None) };
    let Stmt::Expr(Expr::While { cond, body }) = last else { return Ok(None) };
    let yields = body
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
        .collect::<Vec<_>>();
    if yields.len() != 1
        || block_has_nested_yield(body)
        || block_has_generator_control_transfer(body)
    {
        return Ok(None);
    }
    let yield_index = yields[0];
    let Stmt::Yield(yielded) = &body.stmts[yield_index] else { unreachable!() };

    let mut bindings = Vec::new();
    for parameter in &f.params {
        let ty = parameter
            .ty
            .clone()
            .or_else(|| {
                (parameter.name == "self")
                    .then(|| method.map(|context| context.self_ty.clone()))
                    .flatten()
            });
        let Some(ty) = ty else { return Ok(None) };
        bindings.push(GeneratorFrameBinding {
            name: parameter.name.clone(),
            ty,
            mutable: parameter.convention.binds_mutable(),
        });
    }
    let mut initializers = Vec::new();
    for statement in prelude {
        let Stmt::Let { name, ty, mutable, value } = statement else { return Ok(None) };
        let Some(ty) = ty.clone().or_else(|| generator_frame_type(value)) else {
            return Ok(None);
        };
        bindings.push(GeneratorFrameBinding {
            name: name.clone(),
            ty,
            mutable: *mutable,
        });
        initializers.push(statement.clone());
    }
    if bindings.is_empty() {
        return Ok(None);
    }

    let frame_ty = Type::Tuple(bindings.iter().map(|binding| binding.ty.clone()).collect());
    let frame_name = "__generator_frame".to_string();
    let yielded_name = "__generator_yielded".to_string();
    let mut step_statements = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| Stmt::Let {
            name: binding.name.clone(),
            ty: Some(binding.ty.clone()),
            mutable: binding.mutable,
            value: Expr::Field {
                base: Box::new(Expr::Var(frame_name.clone())),
                field: index.to_string(),
            },
        })
        .collect::<Vec<_>>();
    step_statements.extend(body.stmts[..yield_index].iter().cloned());

    let mut produce = vec![Stmt::Let {
        name: yielded_name.clone(),
        ty: Some(elem.clone()),
        mutable: false,
        value: yielded.clone(),
    }];
    produce.extend(body.stmts[yield_index + 1..].iter().cloned());
    let next_frame = Expr::Tuple(
        bindings
            .iter()
            .map(|binding| Expr::Var(binding.name.clone()))
            .collect(),
    );
    produce.push(Stmt::Expr(Expr::Ctor {
        name: "Some".into(),
        args: vec![Expr::Tuple(vec![Expr::Var(yielded_name), next_frame])],
    }));
    step_statements.push(Stmt::Expr(Expr::If {
        cond: Box::new(cond.as_ref().clone()),
        then_block: Block { stmts: produce, lines: Vec::new(), region: None },
        else_block: Some(Block {
            stmts: vec![Stmt::Expr(Expr::Ctor { name: "None".into(), args: Vec::new() })],
            lines: Vec::new(),
            region: None,
        }),
    }));

    let mut helper_attributes = f.attributes.clone();
    helper_attributes.push(crate::suspension::FRAME_FUNCTION_ATTRIBUTE.into());
    helper_attributes.push(crate::suspension::frame_state_attribute(resume_state));
    let helper = Function {
        line: f.line,
        public: false,
        comptime_only: false,
        attributes: helper_attributes,
        name: helper_name.to_string(),
        params: vec![Param {
            name: frame_name,
            ty: Some(frame_ty.clone()),
            convention: Convention::Own,
            default: None,
        }],
        ret: Some(Type::Named(
            "Option".into(),
            vec![Type::Tuple(vec![elem, frame_ty])],
        )),
        body: Block { stmts: step_statements, lines: Vec::new(), region: None },
        bounds: method.map_or_else(|| f.bounds.clone(), |context| context.bounds.clone()),
        is_gen: false,
        is_async: false,
    };

    let mut wrapper_statements = initializers;
    wrapper_statements.push(Stmt::Expr(Expr::Call {
        name: "iter.unfold".into(),
        args: vec![
            Expr::Tuple(
                bindings
                    .iter()
                    .map(|binding| Expr::Var(binding.name.clone()))
                    .collect(),
            ),
            Expr::Var(helper_name.to_string()),
        ],
    }));
    let mut wrapper_attributes = f.attributes.clone();
    wrapper_attributes.push(crate::suspension::FRAME_ENTRY_ATTRIBUTE.into());
    wrapper_attributes.push(crate::suspension::frame_state_attribute(entry_state));
    let wrapper = Function {
        line: f.line,
        public: f.public,
        comptime_only: false,
        attributes: wrapper_attributes,
        name: f.name.clone(),
        params: f.params.clone(),
        ret: f.ret.clone(),
        body: Block { stmts: wrapper_statements, lines: Vec::new(), region: None },
        bounds: f.bounds.clone(),
        is_gen: false,
        is_async: false,
    };
    Ok(Some((helper, wrapper)))
}

fn generator_frame_type(value: &Expr) -> Option<Type> {
    match value {
        Expr::Int(_) => Some(Type::Named("Int".into(), Vec::new())),
        Expr::Float(_) => Some(Type::Named("Float".into(), Vec::new())),
        Expr::Duration(_) => Some(Type::Named("Duration".into(), Vec::new())),
        Expr::Str(_) => Some(Type::Named("String".into(), Vec::new())),
        Expr::Bool(_) => Some(Type::Named("Bool".into(), Vec::new())),
        Expr::Tuple(values) => Some(Type::Tuple(
            values
                .iter()
                .map(generator_frame_type)
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::List(values) => {
            let first = generator_frame_type(values.first()?)?;
            values
                .iter()
                .skip(1)
                .all(|value| generator_frame_type(value).as_ref() == Some(&first))
                .then(|| Type::Named("List".into(), vec![first]))
        }
        _ => None,
    }
}

fn block_has_nested_yield(block: &Block) -> bool {
    let mut nested = false;
    let _: Result<(), ()> = crate::ast::visit::visit_block(block, &mut |expression| {
        let blocks = match expression {
            Expr::If { then_block, else_block, .. } => {
                let mut blocks = vec![then_block];
                blocks.extend(else_block.iter());
                blocks
            }
            Expr::Match { arms, .. } => {
                if arms.iter().any(|arm| matches!(&arm.body, Expr::Block(block) if block.stmts.iter().any(|statement| matches!(statement, Stmt::Yield(_))))) {
                    nested = true;
                }
                Vec::new()
            }
            Expr::Block(block)
            | Expr::Lambda { body: block, .. }
            | Expr::While { body: block, .. }
            | Expr::For { body: block, .. }
            | Expr::WhileLet { body: block, .. } => vec![block],
            _ => Vec::new(),
        };
        if blocks.into_iter().any(|block| {
            block.stmts.iter().any(|statement| matches!(statement, Stmt::Yield(_)))
        }) {
            nested = true;
        }
        Ok(())
    });
    nested
}

fn block_has_generator_control_transfer(block: &Block) -> bool {
    fn directly_transfers(block: &Block) -> bool {
        block.stmts.iter().any(|statement| {
            matches!(statement, Stmt::Return(_) | Stmt::Break | Stmt::Continue)
        })
    }

    if directly_transfers(block) {
        return true;
    }
    let mut transfers = false;
    let _: Result<(), ()> = crate::ast::visit::visit_block(block, &mut |expression| {
        let blocks = match expression {
            Expr::If { then_block, else_block, .. } => {
                let mut blocks = vec![then_block];
                blocks.extend(else_block.iter());
                blocks
            }
            Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|arm| match &arm.body {
                    Expr::Block(block) => Some(block),
                    _ => None,
                })
                .collect(),
            Expr::Block(block)
            | Expr::Lambda { body: block, .. }
            | Expr::While { body: block, .. }
            | Expr::For { body: block, .. }
            | Expr::WhileLet { body: block, .. } => vec![block],
            _ => Vec::new(),
        };
        if blocks.into_iter().any(directly_transfers) {
            transfers = true;
        }
        Ok(())
    });
    transfers
}

/// Replace each `yield e` with `if __i == __target: return Some(e)` followed by
/// `__i = __i + 1`, recursing through the nested blocks of control-flow forms
/// (but not into lambdas — `yield` belongs to the generator, not a closure).
///
/// User `return`s are re-expressed in terms of the generator's stream contract,
/// NOT passed untranslated into the synthesized `-> Option(a)` helper:
/// a bare `return` becomes the stream's end (`return None`), and `return <value>`
/// is rejected against the declared `-> Iter(a)` signature (a generator produces
/// its elements with `yield`, not by returning a scalar) — so the internal
/// `Option(a)` protocol never leaks into a user diagnostic. `gen_name` is the
/// generator's name, used only to phrase that rejection.
fn rewrite_block(b: Block, gen_name: &str, in_region: bool) -> Result<Block, String> {
    let in_region = in_region || b.region.is_some();
    let mut out = Vec::with_capacity(b.stmts.len());
    for stmt in b.stmts {
        match stmt {
            Stmt::Yield(e) => {
                if in_region {
                    return Err(
                        "cannot `yield` inside `region:`: the generator frame outlives the region"
                            .to_string(),
                    );
                }
                let check = Expr::If {
                    cond: Box::new(Expr::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(Expr::Var(COUNTER.to_string())),
                        rhs: Box::new(Expr::Var(TARGET.to_string())),
                    }),
                    then_block: Block {
                        stmts: vec![Stmt::Return(Some(Expr::Ctor {
                            name: "Some".to_string(),
                            args: vec![e],
                        }))],
                        lines: vec![0],
                        region: None,
                    },
                    else_block: None,
                };
                out.push(Stmt::Expr(check));
                out.push(Stmt::Assign {
                    name: COUNTER.to_string(),
                    value: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Var(COUNTER.to_string())),
                        rhs: Box::new(Expr::Int(1)),
                    },
                });
            }
            Stmt::Let { name, ty, mutable, value } => {
                out.push(Stmt::Let {
                    name,
                    ty,
                    mutable,
                    value: rewrite_expr(value, gen_name, in_region)?,
                })
            }
            Stmt::Assign { name, value } => {
                out.push(Stmt::Assign { name, value: rewrite_expr(value, gen_name, in_region)? })
            }
            Stmt::LetPattern { pattern, value } => {
                out.push(Stmt::LetPattern {
                    pattern,
                    value: rewrite_expr(value, gen_name, in_region)?,
                })
            }
            // A bare `return` ends the stream: it becomes the helper's `return None`.
            Stmt::Return(None) => out.push(Stmt::Return(Some(Expr::Ctor {
                name: "None".to_string(),
                args: vec![],
            }))),
            // `return <value>` is not a valid generator statement: a `gen fn`
            // declares `-> Iter(a)` and produces its elements with `yield`, so a
            // returned scalar has nowhere to go. Reject it in the generator's own
            // terms — never let it be typed against the synthesized `Option(a)`.
            Stmt::Return(Some(_)) => {
                return Err(format!(
                    "`return <value>` is not allowed in generator `{gen_name}`: a `gen fn` \
                     declares `-> Iter(a)` and produces its elements with `yield` — use a bare \
                     `return` to end the stream early"
                ));
            }
            Stmt::Expr(e) => out.push(Stmt::Expr(rewrite_expr(e, gen_name, in_region)?)),
            other => out.push(other),
        }
    }
    Ok(Block { stmts: out, lines: b.lines, region: b.region })
}

/// Rewrite the nested blocks of an expression's control-flow forms so yields
/// inside `if`/`while`/`for`/`match`/block bodies are transformed too.
fn rewrite_expr(e: Expr, gen_name: &str, in_region: bool) -> Result<Expr, String> {
    Ok(match e {
        Expr::If { cond, then_block, else_block } => Expr::If {
            cond,
            then_block: rewrite_block(then_block, gen_name, in_region)?,
            else_block: match else_block {
                Some(b) => Some(rewrite_block(b, gen_name, in_region)?),
                None => None,
            },
        },
        Expr::While { cond, body } => {
            Expr::While { cond, body: rewrite_block(body, gen_name, in_region)? }
        }
        Expr::For { var, iter, body } => {
            Expr::For { var, iter, body: rewrite_block(body, gen_name, in_region)? }
        }
        Expr::Match { scrutinee, arms } => {
            let mut new_arms = Vec::with_capacity(arms.len());
            for a in arms {
                new_arms.push(MatchArm {
                    line: a.line,
                    pattern: a.pattern,
                    guard: a.guard,
                    body: rewrite_expr(a.body, gen_name, in_region)?,
                });
            }
            Expr::Match { scrutinee, arms: new_arms }
        }
        Expr::Block(b) => Expr::Block(rewrite_block(b, gen_name, in_region)?),
        other => other,
    })
}

#[cfg(test)]
mod target_availability_tests {
    use super::*;

    #[test]
    fn generator_helpers_preserve_target_availability() {
        let module = crate::parser::parse_module(
            "@browser\ngen fn browser_values() -> Iter(Int):\n    yield 1\n",
        )
        .expect("parse targeted generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        let generated: Vec<&Function> = lowered
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function)
                    if function.name == "browser_values"
                        || function.name == "__gen_browser_values" =>
                {
                    Some(function)
                }
                _ => None,
            })
            .collect();
        assert_eq!(generated.len(), 2);
        assert!(generated.iter().all(|function| {
            function.attributes.iter().any(|attribute| attribute == "browser")
                && crate::suspension::frame_state(function).is_some()
        }));
        let wrapper = generated
            .iter()
            .find(|function| function.name == "browser_values")
            .expect("generator wrapper");
        assert!(wrapper
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_ENTRY_ATTRIBUTE));
        let helper = generated
            .iter()
            .find(|function| function.name == "__gen_browser_values")
            .expect("generator resume helper");
        assert!(helper
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_FUNCTION_ATTRIBUTE));
        assert_eq!(crate::suspension::frame_state(wrapper), Some(0));
        assert_eq!(crate::suspension::frame_state(helper), Some(1));
    }

    #[test]
    fn single_yield_loop_lowers_to_one_pass_owned_unfold_frame() {
        let module = crate::parser::parse_module(
            "gen fn fibs() -> Iter(Int):\n    var a = 0\n    var b = 1\n    while true:\n        yield a\n        let next = a + b\n        a = b\n        b = next\n",
        )
        .expect("parse owned generator frame");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        let wrapper = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "fibs" => Some(function),
                _ => None,
            })
            .expect("generator wrapper");
        assert!(matches!(
            wrapper.body.stmts.last(),
            Some(Stmt::Expr(Expr::Call { name, .. })) if name == "iter.unfold"
        ));
        let helper = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "__gen_fibs" => Some(function),
                _ => None,
            })
            .expect("generator resume helper");
        assert_eq!(helper.params.len(), 1);
        assert_eq!(helper.params[0].convention, Convention::Own);
        assert!(matches!(
            helper.params[0].ty,
            Some(Type::Tuple(ref fields)) if fields.len() == 2
        ));
        assert!(!format!("{:?}", lowered).contains("iter.from_gen"));
    }

    #[test]
    fn generator_control_transfer_retains_replay_fallback() {
        let module = crate::parser::parse_module(
            "gen fn firstn(n: Int) -> Iter(Int):\n    var i = 0\n    while true:\n        if i >= n:\n            return\n        yield i\n        i = i + 1\n",
        )
        .expect("parse generator return");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        assert!(format!("{:?}", lowered).contains("iter.from_gen"));
    }
}
