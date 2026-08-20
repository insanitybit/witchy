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
//! becomes two plain functions. Each supported generator gets an owned tuple
//! frame advanced once per `iter.unfold` pull:
//! ```text
//! fn __gen_count_up(own frame: (Int, Int, Int)) -> Option((Int, (Int, Int, Int))):
//!     let n = frame.0
//!     var i = frame.1
//!     let resume_phase = frame.2
//!     if resume_phase == 1:
//!         i = i + 1
//!     if true:
//!         Some((i, (n, i, 1)))
//!     else:
//!         None
//! fn count_up(n: Int) -> Iter(Int):
//!     var i = n
//!     iter.unfold((n, i, 0), __gen_count_up)
//! ```
//! Each migrated shape resumes from its recorded phase. The isolated replay
//! compatibility path remains only for accepted CFGs whose owned transitions
//! are still being implemented; those shapes are not part of the promoted
//! effect or complexity contract.

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
    let function_returns = checked
        .module()
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => function
                .ret
                .clone()
                .map(|ret| (function.name.clone(), ret)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let module = checked.module_mut();
    let mut items = Vec::with_capacity(module.items.len() + 1);
    let mut state_counter = 0usize;
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Function(f) if f.is_gen => {
                let (helper, wrapper) =
                    lower_gen(f, None, &function_returns, &mut state_counter)?;
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
                            lower_gen(method, Some(&ctx), &function_returns, &mut state_counter)?;
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
        Expr::While { body, .. }
        | Expr::WhileLet { body, .. }
        | Expr::For { body, .. }
        | Expr::Block(body) => {
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

/// One collision-free namespace for compiler-owned bindings in a generator.
///
/// AST bindings are still string-backed, so generated names must be hygienic by
/// construction. Pick a prefix absent from every source binding and variable
/// reference; all names derived from it are then unreachable from source text in
/// this function, even when the user deliberately chooses an internal-looking
/// identifier.
#[derive(Clone)]
struct GeneratorNames {
    prefix: String,
}

impl GeneratorNames {
    fn new(function: &Function, seed: usize) -> Self {
        let mut used = std::collections::HashSet::new();
        used.extend(function.params.iter().map(|parameter| parameter.name.clone()));
        collect_direct_generator_names(&function.body, &mut used);
        let _: Result<(), ()> = crate::ast::visit::visit_block(
            &function.body,
            &mut |expression| {
                match expression {
                    Expr::Var(name) => {
                        used.insert(name.clone());
                    }
                    Expr::Lambda { params, .. } => {
                        used.extend(params.iter().map(|parameter| parameter.name.clone()));
                    }
                    Expr::For { var, body, .. } => {
                        used.insert(var.clone());
                        collect_direct_generator_names(body, &mut used);
                    }
                    Expr::While { body, .. }
                    | Expr::WhileLet { body, .. }
                    | Expr::Block(body) => collect_direct_generator_names(body, &mut used),
                    Expr::If { then_block, else_block, .. } => {
                        collect_direct_generator_names(then_block, &mut used);
                        if let Some(block) = else_block {
                            collect_direct_generator_names(block, &mut used);
                        }
                    }
                    Expr::Match { arms, .. } => {
                        for arm in arms {
                            let mut names = Vec::new();
                            pattern_binds(&arm.pattern, &mut names);
                            used.extend(names);
                            if let Expr::Block(block) = &arm.body {
                                collect_direct_generator_names(block, &mut used);
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            },
        );
        let mut serial = seed;
        loop {
            let prefix = format!("__generator_internal_{serial}_");
            if used.iter().all(|name| !name.starts_with(&prefix)) {
                return Self { prefix };
            }
            serial += 1;
        }
    }

    fn name(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.prefix)
    }
}

fn collect_direct_generator_names(
    block: &Block,
    names: &mut std::collections::HashSet<String>,
) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { name, .. } | Stmt::Assign { name, .. } => {
                names.insert(name.clone());
            }
            Stmt::LetPattern { pattern, .. } => {
                let mut bound = Vec::new();
                pattern_binds(pattern, &mut bound);
                names.extend(bound);
            }
            Stmt::Yield(_)
            | Stmt::Return(_)
            | Stmt::Expr(_)
            | Stmt::Break
            | Stmt::Continue => {}
        }
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
    function_returns: &std::collections::HashMap<String, Type>,
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
    let names = GeneratorNames::new(&f, entry_state);
    let normalized = normalize_owned_generator(&f, &names, function_returns);
    if let Some(lowered) = lower_owned_loop_frame(
        &normalized,
        method,
        &helper_name,
        entry_state,
        resume_state,
        &names,
    )? {
        return Ok(lowered);
    }

    lower_replay_fallback(f, method, helper_name, entry_state, resume_state, elem)
}

/// Preserve the historical surface while an irregular CFG is migrated to the
/// owned-frame state machine. This compatibility path is deliberately isolated
/// so each new owned lowering removes callers from one place, and the final
/// no-replay slice can delete this function without changing accepted syntax.
fn lower_replay_fallback(
    f: Function,
    method: Option<&MethodCtx>,
    helper_name: String,
    entry_state: usize,
    resume_state: usize,
    elem: Option<Type>,
) -> Result<(Function, Function), String> {
    let mut helper_params = f.params.clone();
    helper_params.push(Param {
        name: TARGET.to_string(),
        ty: Some(Type::Named("Int".to_string(), vec![])),
        convention: Convention::Let,
        default: None,
    });
    if let Some(context) = method {
        if let Some(first) = helper_params.first_mut() {
            if first.ty.is_none() {
                first.ty = Some(context.self_ty.clone());
            }
        }
    }

    let mut statements = vec![Stmt::Let {
        name: COUNTER.to_string(),
        ty: None,
        mutable: true,
        value: Expr::Int(0),
    }];
    statements.extend(rewrite_block(f.body.clone(), &f.name, false)?.stmts);
    statements.push(Stmt::Expr(Expr::Ctor {
        name: "None".to_string(),
        args: vec![],
    }));
    let mut helper_attributes = f.attributes.clone();
    helper_attributes.push(crate::suspension::FRAME_FUNCTION_ATTRIBUTE.into());
    helper_attributes.push(crate::suspension::FRAME_BOXED_ATTRIBUTE.into());
    helper_attributes.push(crate::suspension::frame_state_attribute(resume_state));
    let helper = Function {
        line: f.line,
        public: false,
        comptime_only: false,
        attributes: helper_attributes,
        name: helper_name.clone(),
        params: helper_params,
        ret: elem
            .as_ref()
            .map(|item| Type::Named("Option".to_string(), vec![item.clone()])),
        body: Block {
            stmts: statements,
            lines: Vec::new(),
            region: None,
        },
        bounds: method.map_or_else(|| f.bounds.clone(), |context| context.bounds.clone()),
        is_gen: false,
        is_async: false,
    };

    let forwarded = f
        .params
        .iter()
        .map(|parameter| Expr::Var(parameter.name.clone()))
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
            stmts: vec![Stmt::Expr(Expr::Call {
                name: helper_name,
                args: forwarded,
            })],
            lines: vec![0],
            region: None,
        },
        ret: None,
    };
    let mut wrapper_attributes = f.attributes;
    wrapper_attributes.push(crate::suspension::FRAME_ENTRY_ATTRIBUTE.into());
    wrapper_attributes.push(crate::suspension::FRAME_BOXED_ATTRIBUTE.into());
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

/// Normalize accepted finite forms and the flagship seed-yield loop into the
/// terminal-loop shapes handled by the owned frame lowerer. These synthetic
/// bindings are part of the frame, so finishing a finite body cannot restart it.
fn normalize_owned_generator(
    f: &Function,
    names: &GeneratorNames,
    function_returns: &std::collections::HashMap<String, Type>,
) -> Function {
    if let Some(normalized) = normalize_terminal_for(f, names, function_returns) {
        return normalized;
    }
    if let Some(normalized) = normalize_prefix_yield_loop(f, names) {
        return normalized;
    }
    if matches!(f.body.stmts.last(), Some(Stmt::Expr(Expr::While { .. } | Expr::WhileLet { .. }))) {
        return f.clone();
    }
    if let Some(normalized) = normalize_finite_conditional_then_direct(f, names) {
        return normalized;
    }
    if finite_body_uses_supported_yields(f) {
        return normalize_finite_one_shot(f, names);
    }
    f.clone()
}

/// Normalize a terminal list `for` into the same indexed owned-loop shape used
/// by the backend. The list expression is an entry initializer, so constructing
/// the generator stays lazy and later pulls resume from the captured index.
fn normalize_terminal_for(
    f: &Function,
    names: &GeneratorNames,
    function_returns: &std::collections::HashMap<String, Type>,
) -> Option<Function> {
    let (last, prelude) = f.body.stmts.split_last()?;
    let Stmt::Expr(Expr::For { var, iter, body }) = last else { return None };
    if !prelude.iter().all(|statement| matches!(statement, Stmt::Let { .. }))
        || !block_has_any_yield(body)
        || block_has_generator_loop_control_transfer(body)
    {
        return None;
    }

    let mut bindings = Vec::new();
    for parameter in &f.params {
        let ty = parameter.ty.clone()?;
        bindings.push(GeneratorFrameBinding {
            name: parameter.name.clone(),
            ty,
            mutable: parameter.convention.binds_mutable(),
        });
    }
    for statement in prelude {
        let Stmt::Let { name, ty, mutable, value } = statement else { unreachable!() };
        let ty = ty
            .clone()
            .or_else(|| generator_frame_type_from_bindings(value, &bindings))?;
        bindings.push(GeneratorFrameBinding {
            name: name.clone(),
            ty,
            mutable: *mutable,
        });
    }
    let list_ty = match iter.as_ref() {
        Expr::Call { name, .. } => function_returns.get(name).cloned(),
        value => generator_frame_type_from_bindings(value, &bindings),
    }?;
    let Type::Named(list_name, list_args) = &list_ty else { return None };
    let [elem] = list_args.as_slice() else { return None };
    if list_name != "List" {
        return None;
    }
    let elem = elem.clone();

    let list = names.name("for_list");
    let index = names.name("for_index");
    let mut statements = prelude.to_vec();
    statements.push(Stmt::Let {
        name: list.clone(),
        ty: Some(list_ty),
        mutable: false,
        value: iter.as_ref().clone(),
    });
    statements.push(Stmt::Let {
        name: index.clone(),
        ty: Some(Type::Named("Int".into(), Vec::new())),
        mutable: true,
        value: Expr::Int(0),
    });
    let mut loop_statements = vec![
        Stmt::Let {
            name: var.clone(),
            ty: Some(elem),
            mutable: false,
            value: Expr::Call {
                name: crate::intrinsics::LIST_AT.into(),
                args: vec![Expr::Var(list.clone()), Expr::Var(index.clone())],
            },
        },
        Stmt::Assign {
            name: index.clone(),
            value: Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Var(index.clone())),
                rhs: Box::new(Expr::Int(1)),
            },
        },
    ];
    loop_statements.extend(body.stmts.clone());
    statements.push(Stmt::Expr(Expr::While {
        cond: Box::new(Expr::Binary {
            op: BinOp::Lt,
            lhs: Box::new(Expr::Var(index)),
            rhs: Box::new(Expr::Call {
                name: crate::intrinsics::LIST_LENGTH.into(),
                args: vec![Expr::Var(list)],
            }),
        }),
        body: Block {
            stmts: loop_statements,
            lines: body.lines.clone(),
            region: body.region.clone(),
        },
    }));
    let mut normalized = f.clone();
    normalized.body = Block {
        stmts: statements,
        lines: f.body.lines.clone(),
        region: f.body.region.clone(),
    };
    Some(normalized)
}

fn leading_initializers(statements: &[Stmt]) -> usize {
    statements
        .iter()
        .take_while(|statement| matches!(statement, Stmt::Let { .. }))
        .count()
}

fn finite_body_uses_supported_yields(f: &Function) -> bool {
    let prelude_len = leading_initializers(&f.body.stmts);
    let body = Block {
        stmts: f.body.stmts[prelude_len..].to_vec(),
        lines: Vec::new(),
        region: None,
    };
    let direct_yields = body
        .stmts
        .iter()
        .any(|statement| matches!(statement, Stmt::Yield(_)));
    (direct_yields && !block_has_nested_yield(&body))
        || (!direct_yields
            && (single_nested_conditional_yield(&body).is_some()
                || nested_branch_yields(&body).is_some()
                || nested_match_yields(&body).is_some()))
}

fn normalize_finite_one_shot(f: &Function, names: &GeneratorNames) -> Function {
    let once = names.name("once");
    let prelude_len = leading_initializers(&f.body.stmts);
    let mut statements = f.body.stmts[..prelude_len].to_vec();
    statements.push(Stmt::Let {
        name: once.clone(),
        ty: Some(Type::Named("Bool".into(), Vec::new())),
        mutable: true,
        value: Expr::Bool(true),
    });
    let mut body = f.body.stmts[prelude_len..].to_vec();
    body.push(Stmt::Assign {
        name: once.clone(),
        value: Expr::Bool(false),
    });
    statements.push(Stmt::Expr(Expr::While {
        cond: Box::new(Expr::Var(once)),
        body: Block { stmts: body, lines: Vec::new(), region: None },
    }));
    let mut normalized = f.clone();
    normalized.body = Block { stmts: statements, lines: Vec::new(), region: None };
    normalized
}

/// Normalize `if condition: yield first; yield second` into a two-branch loop.
/// The stage assignment occurs before suspension, so resumption neither repeats
/// the condition nor skips the direct tail yield.
fn normalize_finite_conditional_then_direct(
    f: &Function,
    names: &GeneratorNames,
) -> Option<Function> {
    let finite_stage = names.name("finite_stage");
    let prelude_len = leading_initializers(&f.body.stmts);
    let executable = &f.body.stmts[prelude_len..];
    let if_index = executable.iter().position(|statement| {
        matches!(statement, Stmt::Expr(Expr::If { then_block, .. }) if block_has_any_yield(then_block))
    })?;
    if if_index != 0 {
        return None;
    }
    let Stmt::Expr(Expr::If { cond, then_block, else_block }) = &executable[if_index] else {
        return None;
    };
    let (then_prefix, then_yielded, then_suffix) = direct_yield_parts(then_block)?;
    if else_block.as_ref().is_some_and(block_has_any_yield) {
        return None;
    }
    let tail = &executable[if_index + 1..];
    let tail_yields = tail
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
        .collect::<Vec<_>>();
    let [tail_yield_index] = tail_yields.as_slice() else { return None };
    let tail_block = Block { stmts: tail.to_vec(), lines: Vec::new(), region: None };
    if block_has_nested_yield(&tail_block) {
        return None;
    }
    let Stmt::Yield(tail_yielded) = &tail[*tail_yield_index] else { unreachable!() };

    let mut then_statements = then_prefix;
    then_statements.push(Stmt::Assign {
        name: finite_stage.clone(),
        value: Expr::Int(1),
    });
    then_statements.push(Stmt::Yield(then_yielded));
    then_statements.extend(then_suffix);

    let mut else_statements = Vec::new();
    if let Some(first_else) = else_block {
        else_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Var(finite_stage.clone())),
                rhs: Box::new(Expr::Int(0)),
            }),
            then_block: first_else.clone(),
            else_block: None,
        }));
    }
    else_statements.push(Stmt::Assign {
        name: finite_stage.clone(),
        value: Expr::Int(2),
    });
    else_statements.extend(tail[..*tail_yield_index].iter().cloned());
    else_statements.push(Stmt::Yield(tail_yielded.clone()));
    else_statements.extend(tail[*tail_yield_index + 1..].iter().cloned());

    let mut statements = f.body.stmts[..prelude_len].to_vec();
    statements.push(Stmt::Let {
        name: finite_stage.clone(),
        ty: Some(Type::Named("Int".into(), Vec::new())),
        mutable: true,
        value: Expr::Int(0),
    });
    statements.push(Stmt::Expr(Expr::While {
        cond: Box::new(Expr::Binary {
            op: BinOp::Lt,
            lhs: Box::new(Expr::Var(finite_stage.clone())),
            rhs: Box::new(Expr::Int(2)),
        }),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::And,
                    lhs: Box::new(Expr::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(Expr::Var(finite_stage)),
                        rhs: Box::new(Expr::Int(0)),
                    }),
                    rhs: cond.clone(),
                }),
                then_block: Block {
                    stmts: then_statements,
                    lines: Vec::new(),
                    region: None,
                },
                else_block: Some(Block {
                    stmts: else_statements,
                    lines: Vec::new(),
                    region: None,
                }),
            })],
            lines: Vec::new(),
            region: None,
        },
    }));
    let mut normalized = f.clone();
    normalized.body = Block { stmts: statements, lines: Vec::new(), region: None };
    Some(normalized)
}

/// Normalize `yield seed; while condition: ... yield next` into a terminal loop
/// with two directly yielding branches. This preserves Collatz's initial value
/// while recording whether the seed branch has already run in the owned frame.
fn normalize_prefix_yield_loop(f: &Function, names: &GeneratorNames) -> Option<Function> {
    let once = names.name("once");
    let (last, prefix) = f.body.stmts.split_last()?;
    let Stmt::Expr(Expr::While { cond, body }) = last else { return None };
    let (seed, initializers) = prefix.split_last()?;
    let Stmt::Yield(seed) = seed else { return None };
    if !initializers.iter().all(|statement| matches!(statement, Stmt::Let { .. }))
        || direct_yield_parts(body).is_none()
    {
        return None;
    }
    let mut statements = initializers.to_vec();
    statements.push(Stmt::Let {
        name: once.clone(),
        ty: Some(Type::Named("Bool".into(), Vec::new())),
        mutable: true,
        value: Expr::Bool(true),
    });
    statements.push(Stmt::Expr(Expr::While {
        cond: Box::new(Expr::Binary {
            op: BinOp::Or,
            lhs: Box::new(Expr::Var(once.clone())),
            rhs: cond.clone(),
        }),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Var(once.clone())),
                then_block: Block {
                    stmts: vec![
                        Stmt::Assign {
                            name: once,
                            value: Expr::Bool(false),
                        },
                        Stmt::Yield(seed.clone()),
                    ],
                    lines: Vec::new(),
                    region: None,
                },
                else_block: Some(body.clone()),
            })],
            lines: Vec::new(),
            region: None,
        },
    }));
    let mut normalized = f.clone();
    normalized.body = Block { stmts: statements, lines: Vec::new(), region: None };
    Some(normalized)
}

#[derive(Clone)]
struct GeneratorFrameBinding {
    name: String,
    ty: Type,
    mutable: bool,
}

#[derive(Clone)]
struct DirectLoopLocal {
    binding: GeneratorFrameBinding,
    declaration_index: usize,
}

fn capture_frame_bindings(
    bindings: &[GeneratorFrameBinding],
    parameter_count: usize,
) -> Vec<Expr> {
    bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            let value = Expr::Var(binding.name.clone());
            if index < parameter_count {
                value
            } else {
                Expr::Ctor {
                    name: "Some".into(),
                    args: vec![value],
                }
            }
        })
        .collect()
}

fn initial_frame_bindings(
    bindings: &[GeneratorFrameBinding],
    parameter_count: usize,
) -> Vec<Expr> {
    bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            if index < parameter_count {
                Expr::Var(binding.name.clone())
            } else {
                Expr::Ctor {
                    name: "None".into(),
                    args: Vec::new(),
                }
            }
        })
        .collect()
}

struct NestedConditionalYield {
    outer_prefix: Vec<Stmt>,
    outer_suffix: Vec<Stmt>,
    branch_condition: Expr,
    then_prefix: Vec<Stmt>,
    then_suffix: Vec<Stmt>,
    yielded: Expr,
    else_block: Option<Block>,
}

struct NestedBranchYields {
    outer_prefix: Vec<Stmt>,
    outer_suffix: Vec<Stmt>,
    branch_condition: Expr,
    then_prefix: Vec<Stmt>,
    then_suffix: Vec<Stmt>,
    then_yielded: Expr,
    else_prefix: Vec<Stmt>,
    else_suffix: Vec<Stmt>,
    else_yielded: Expr,
}

struct NestedMatchArmYield {
    line: u32,
    pattern: Pattern,
    guard: Option<Expr>,
    prefix: Vec<Stmt>,
    suffix: Vec<Stmt>,
    yielded: Option<Expr>,
    rebind_on_resume: bool,
}

struct NestedMatchYields {
    outer_prefix: Vec<Stmt>,
    outer_suffix: Vec<Stmt>,
    scrutinee: Expr,
    arms: Vec<NestedMatchArmYield>,
}

fn direct_yield_parts(block: &Block) -> Option<(Vec<Stmt>, Expr, Vec<Stmt>)> {
    if block_has_nested_yield(block) {
        return None;
    }
    let yields = block
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
        .collect::<Vec<_>>();
    let [yield_index] = yields.as_slice() else { return None };
    let Stmt::Yield(yielded) = &block.stmts[*yield_index] else { unreachable!() };
    let prefix = block.stmts[..*yield_index].to_vec();
    let suffix = block.stmts[*yield_index + 1..].to_vec();
    if !live_loop_local_bindings(&prefix, &suffix)?.is_empty() {
        return None;
    }
    Some((prefix, yielded.clone(), suffix))
}

fn optional_direct_yield_parts(block: &Block) -> Option<(Vec<Stmt>, Option<Expr>, Vec<Stmt>)> {
    if block_has_nested_yield(block) {
        return None;
    }
    let yields = block
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
        .collect::<Vec<_>>();
    match yields.as_slice() {
        [] => Some((block.stmts.clone(), None, Vec::new())),
        [yield_index] => {
            let Stmt::Yield(yielded) = &block.stmts[*yield_index] else { unreachable!() };
            let prefix = block.stmts[..*yield_index].to_vec();
            let suffix = block.stmts[*yield_index + 1..].to_vec();
            if !live_loop_local_bindings(&prefix, &suffix)?.is_empty() {
                return None;
            }
            Some((prefix, Some(yielded.clone()), suffix))
        }
        _ => None,
    }
}

/// Recognize an `if` whose two branches each suspend once. The frame records
/// which branch suspended, so resumption enters only that branch's suffix and
/// does not repeat the condition or either prefix.
fn nested_branch_yields(body: &Block) -> Option<NestedBranchYields> {
    let mut found = None;
    for (outer_index, statement) in body.stmts.iter().enumerate() {
        let Stmt::Expr(Expr::If {
            cond,
            then_block,
            else_block: Some(else_block),
        }) = statement
        else {
            continue;
        };
        let Some((then_prefix, then_yielded, then_suffix)) = direct_yield_parts(then_block) else {
            continue;
        };
        let Some((else_prefix, else_yielded, else_suffix)) = direct_yield_parts(else_block) else {
            continue;
        };
        if found.is_some()
            || block_has_any_yield(&Block {
                stmts: body.stmts[..outer_index].to_vec(),
                lines: Vec::new(),
                region: None,
            })
            || block_has_any_yield(&Block {
                stmts: body.stmts[outer_index + 1..].to_vec(),
                lines: Vec::new(),
                region: None,
            })
        {
            return None;
        }
        found = Some(NestedBranchYields {
            outer_prefix: body.stmts[..outer_index].to_vec(),
            outer_suffix: body.stmts[outer_index + 1..].to_vec(),
            branch_condition: cond.as_ref().clone(),
            then_prefix,
            then_suffix,
            then_yielded,
            else_prefix,
            else_suffix,
            else_yielded,
        });
    }
    found
}

/// Recognize a `match` with one or more directly suspending arms. Ordinary arms
/// run to completion at phase zero; a yielding arm records its own resume phase,
/// preserving pattern/guard behavior without replaying its prefix.
fn nested_match_yields(body: &Block) -> Option<NestedMatchYields> {
    let mut found = None;
    for (outer_index, statement) in body.stmts.iter().enumerate() {
        let Stmt::Expr(Expr::Match { scrutinee, arms }) = statement else { continue };
        if arms.is_empty() {
            continue;
        }
        let lowered_arms = arms
            .iter()
            .map(|arm| {
                let Expr::Block(block) = &arm.body else { return None };
                let (prefix, yielded, suffix) = optional_direct_yield_parts(block)?;
                let suffix_block = Block {
                    stmts: suffix.clone(),
                    lines: Vec::new(),
                    region: None,
                };
                let mut pattern_bindings = Vec::new();
                crate::ast::pattern_binds(&arm.pattern, &mut pattern_bindings);
                let rebind_on_resume = yielded.is_some()
                    && pattern_bindings
                    .iter()
                    .any(|binding| block_references_binding(&suffix_block, binding));
                // Rebinding a selected arm can safely read a restored frame
                // variable. An arbitrary scrutinee expression could repeat an
                // effect, so those shapes remain outside the supported owned
                // frame boundary and receive a lowering diagnostic.
                if rebind_on_resume && !matches!(scrutinee.as_ref(), Expr::Var(_)) {
                    return None;
                }
                if rebind_on_resume
                    && matches!(scrutinee.as_ref(), Expr::Var(name) if block_assigns_binding(
                        &Block {
                            stmts: prefix.clone(),
                            lines: Vec::new(),
                            region: None,
                        },
                        name,
                    ))
                {
                    return None;
                }
                Some(NestedMatchArmYield {
                    line: arm.line,
                    pattern: arm.pattern.clone(),
                    guard: arm.guard.clone(),
                    prefix,
                    suffix,
                    yielded,
                    rebind_on_resume,
                })
            })
            .collect::<Option<Vec<_>>>();
        let Some(arms) = lowered_arms else { continue };
        if !arms.iter().any(|arm| arm.yielded.is_some()) {
            continue;
        }
        if found.is_some()
            || block_has_any_yield(&Block {
                stmts: body.stmts[..outer_index].to_vec(),
                lines: Vec::new(),
                region: None,
            })
            || block_has_any_yield(&Block {
                stmts: body.stmts[outer_index + 1..].to_vec(),
                lines: Vec::new(),
                region: None,
            })
        {
            return None;
        }
        found = Some(NestedMatchYields {
            outer_prefix: body.stmts[..outer_index].to_vec(),
            outer_suffix: body.stmts[outer_index + 1..].to_vec(),
            scrutinee: scrutinee.as_ref().clone(),
            arms,
        });
    }
    found
}

/// Recognize one conditional suspension point inside the generator's outer
/// loop. This is deliberately structural: the resume helper enters the suffix
/// of the taken branch directly, so it neither re-evaluates the condition nor
/// repeats effects that ran before the yield.
fn single_nested_conditional_yield(body: &Block) -> Option<NestedConditionalYield> {
    let mut found = None;
    for (outer_index, statement) in body.stmts.iter().enumerate() {
        let Stmt::Expr(Expr::If {
            cond,
            then_block,
            else_block,
        }) = statement
        else {
            continue;
        };
        let direct_yields = then_block
            .stmts
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
            .collect::<Vec<_>>();
        if direct_yields.len() != 1
            || block_has_nested_yield(then_block)
            || else_block.as_ref().is_some_and(block_has_any_yield)
        {
            continue;
        }
        let yield_index = direct_yields[0];
        let Stmt::Yield(yielded) = &then_block.stmts[yield_index] else { unreachable!() };
        let then_prefix = then_block.stmts[..yield_index].to_vec();
        let then_suffix = then_block.stmts[yield_index + 1..].to_vec();
        if then_prefix.iter().any(|statement| matches!(statement, Stmt::LetPattern { .. }))
            || !live_loop_local_bindings(&then_prefix, &then_suffix)?.is_empty()
        {
            continue;
        }
        if found.is_some()
            || block_has_any_yield(&Block {
                stmts: body.stmts[..outer_index].to_vec(),
                lines: Vec::new(),
                region: None,
            })
            || block_has_any_yield(&Block {
                stmts: body.stmts[outer_index + 1..].to_vec(),
                lines: Vec::new(),
                region: None,
            })
        {
            return None;
        }
        found = Some(NestedConditionalYield {
            outer_prefix: body.stmts[..outer_index].to_vec(),
            outer_suffix: body.stmts[outer_index + 1..].to_vec(),
            branch_condition: cond.as_ref().clone(),
            then_prefix,
            then_suffix,
            yielded: yielded.clone(),
            else_block: else_block.clone(),
        });
    }
    found
}

fn block_has_any_yield(block: &Block) -> bool {
    block.stmts.iter().any(|statement| matches!(statement, Stmt::Yield(_)))
        || block_has_nested_yield(block)
}

/// Direct loop locals that are read or assigned after the yield are live across
/// suspension. They are distinct from the generator's entry bindings: there is
/// no value for them until the loop prefix has run, so the frame carries each as
/// `Option(T)` and fills it only when suspending.
fn live_loop_local_bindings(before: &[Stmt], after: &[Stmt]) -> Option<Vec<GeneratorFrameBinding>> {
    let after = Block { stmts: after.to_vec(), lines: Vec::new(), region: None };
    let mut live = Vec::new();
    for statement in before {
        let Stmt::Let { name, ty, mutable, value } = statement else { continue };
        if !block_references_binding(&after, name) {
            continue;
        }
        live.push(GeneratorFrameBinding {
            name: name.clone(),
            ty: ty.clone().or_else(|| generator_frame_type(value))?,
            mutable: *mutable,
        });
    }
    Some(live)
}

/// Direct-body locals whose value crosses at least one suspension. The source
/// position is retained so each phase restores and captures only locals that
/// have actually been initialized at that point.
fn direct_loop_live_locals(
    body: &Block,
    yields: &[usize],
    entry_bindings: &[GeneratorFrameBinding],
) -> Option<Vec<DirectLoopLocal>> {
    let mut type_environment = entry_bindings.to_vec();
    let mut live = Vec::new();
    for (index, statement) in body.stmts.iter().enumerate() {
        let declared = match statement {
            Stmt::Let {
                name,
                ty,
                mutable,
                value,
            } => {
                let inferred = ty
                    .clone()
                    .or_else(|| generator_frame_type_from_bindings(value, &type_environment))?;
                vec![GeneratorFrameBinding {
                    name: name.clone(),
                    ty: inferred,
                    mutable: *mutable,
                }]
            }
            Stmt::LetPattern { pattern, value } => {
                let value_ty = generator_frame_type_from_bindings(value, &type_environment)?;
                generator_pattern_bindings(pattern, &value_ty)?
            }
            _ => continue,
        };
        for binding in &declared {
            if let Some(next_yield) = yields.iter().copied().find(|yield_index| *yield_index > index)
            {
                let after = Block {
                    stmts: body.stmts[next_yield + 1..].to_vec(),
                    lines: Vec::new(),
                    region: None,
                };
                if block_references_binding(&after, &binding.name) {
                    live.push(DirectLoopLocal {
                        binding: binding.clone(),
                        declaration_index: index,
                    });
                }
            }
        }
        type_environment.extend(declared);
    }
    Some(live)
}

fn generator_pattern_bindings(
    pattern: &Pattern,
    ty: &Type,
) -> Option<Vec<GeneratorFrameBinding>> {
    match pattern {
        Pattern::Wildcard
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => Some(Vec::new()),
        Pattern::Var(name) => Some(vec![GeneratorFrameBinding {
            name: name.clone(),
            ty: ty.clone(),
            mutable: false,
        }]),
        Pattern::Tuple(patterns) => {
            let Type::Tuple(types) = ty else { return None };
            if patterns.len() != types.len() {
                return None;
            }
            let mut bindings = Vec::new();
            for (pattern, ty) in patterns.iter().zip(types) {
                bindings.extend(generator_pattern_bindings(pattern, ty)?);
            }
            Some(bindings)
        }
        Pattern::List { elems, rest } => {
            let Type::Named(name, args) = ty else { return None };
            let [elem_ty] = args.as_slice() else { return None };
            if name != "List" {
                return None;
            }
            let mut bindings = Vec::new();
            for pattern in elems {
                bindings.extend(generator_pattern_bindings(pattern, elem_ty)?);
            }
            if let Some(Some(name)) = rest {
                bindings.push(GeneratorFrameBinding {
                    name: name.clone(),
                    ty: ty.clone(),
                    mutable: false,
                });
            }
            Some(bindings)
        }
        Pattern::Ctor { .. } | Pattern::AnonCtor { .. } | Pattern::Or(_) => None,
    }
}

fn block_references_binding(block: &Block, name: &str) -> bool {
    if block_assigns_binding(block, name) {
        return true;
    }
    let mut referenced = false;
    let _: Result<(), ()> = crate::ast::visit::visit_block(block, &mut |expression| {
        if matches!(expression, Expr::Var(variable) if variable == name) {
            referenced = true;
        }
        Ok(())
    });
    referenced
}

fn block_assigns_binding(block: &Block, name: &str) -> bool {
    if block.stmts.iter().any(|statement| {
        matches!(statement, Stmt::Assign { name: assigned, .. } if assigned == name)
    }) {
        return true;
    }
    let mut assigned = false;
    let _: Result<(), ()> = crate::ast::visit::visit_block(block, &mut |expression| {
        let nested = match expression {
            Expr::If { then_block, else_block, .. } => {
                let mut nested = vec![then_block];
                nested.extend(else_block.iter());
                nested
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
        if nested.into_iter().any(|block| {
            block.stmts.iter().any(|statement| {
                matches!(statement, Stmt::Assign { name: nested_name, .. } if nested_name == name)
            })
        }) {
            assigned = true;
        }
        Ok(())
    });
    assigned
}

fn restore_optional_bindings(
    frame_name: &str,
    frame_offset: usize,
    bindings: &[GeneratorFrameBinding],
    names: &GeneratorNames,
    namespace: &str,
    mut body: Block,
) -> Block {
    for (offset, binding) in bindings.iter().enumerate().rev() {
        let restored = names.name(&format!("{namespace}_{offset}"));
        let mut resumed = vec![Stmt::Let {
            name: binding.name.clone(),
            ty: Some(binding.ty.clone()),
            mutable: binding.mutable,
            value: Expr::Var(restored.clone()),
        }];
        resumed.append(&mut body.stmts);
        body = Block {
            stmts: vec![Stmt::Expr(Expr::Match {
                scrutinee: Box::new(Expr::Field {
                    base: Box::new(Expr::Var(frame_name.to_string())),
                    field: (frame_offset + offset).to_string(),
                }),
                arms: vec![
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor {
                            name: "Some".into(),
                            args: vec![Pattern::Var(restored)],
                        },
                        guard: None,
                        body: Expr::Block(Block {
                            stmts: resumed,
                            lines: Vec::new(),
                            region: None,
                        }),
                    },
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor { name: "None".into(), args: Vec::new() },
                        guard: None,
                        body: Expr::Block(Block {
                            stmts: vec![Stmt::Return(Some(Expr::Ctor {
                                name: "None".into(),
                                args: Vec::new(),
                            }))],
                            lines: Vec::new(),
                            region: None,
                        }),
                    },
                ],
            })],
            lines: Vec::new(),
            region: None,
        };
    }
    body
}

fn restore_direct_loop_locals(
    frame_name: &str,
    frame_offset: usize,
    locals: &[DirectLoopLocal],
    initialized_before: usize,
    names: &GeneratorNames,
    mut body: Block,
) -> Block {
    for (offset, local) in locals.iter().enumerate().rev() {
        if local.declaration_index >= initialized_before {
            continue;
        }
        let binding = &local.binding;
        let restored = names.name(&format!("direct_live_{offset}"));
        let mut resumed = vec![Stmt::Let {
            name: binding.name.clone(),
            ty: Some(binding.ty.clone()),
            mutable: binding.mutable,
            value: Expr::Var(restored.clone()),
        }];
        resumed.append(&mut body.stmts);
        body = Block {
            stmts: vec![Stmt::Expr(Expr::Match {
                scrutinee: Box::new(Expr::Field {
                    base: Box::new(Expr::Var(frame_name.to_string())),
                    field: (frame_offset + offset).to_string(),
                }),
                arms: vec![
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor {
                            name: "Some".into(),
                            args: vec![Pattern::Var(restored)],
                        },
                        guard: None,
                        body: Expr::Block(Block {
                            stmts: resumed,
                            lines: Vec::new(),
                            region: None,
                        }),
                    },
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor {
                            name: "None".into(),
                            args: Vec::new(),
                        },
                        guard: None,
                        body: Expr::Block(Block {
                            stmts: vec![Stmt::Return(Some(Expr::Ctor {
                                name: "None".into(),
                                args: Vec::new(),
                            }))],
                            lines: Vec::new(),
                            region: None,
                        }),
                    },
                ],
            })],
            lines: Vec::new(),
            region: None,
        };
    }
    body
}

fn capture_direct_loop_locals(
    frame_name: &str,
    frame_offset: usize,
    locals: &[DirectLoopLocal],
    initialized_before: usize,
) -> Vec<Expr> {
    locals
        .iter()
        .enumerate()
        .map(|(offset, local)| {
            if local.declaration_index < initialized_before {
                Expr::Ctor {
                    name: "Some".into(),
                    args: vec![Expr::Var(local.binding.name.clone())],
                }
            } else {
                Expr::Field {
                    base: Box::new(Expr::Var(frame_name.to_string())),
                    field: (frame_offset + offset).to_string(),
                }
            }
        })
        .collect()
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
/// The frame is a fixed typed tuple of parameters, initialized locals, and an
/// integer resume phase. One `iter.unfold` step restores those bindings,
/// executes the statements after the previous yield only when resuming, and
/// stops exactly at the next direct yield. Residual CFGs keep their existing
/// surface through [`lower_replay_fallback`] until a later slice gives them the
/// same owned transition contract.
fn lower_owned_loop_frame(
    f: &Function,
    method: Option<&MethodCtx>,
    helper_name: &str,
    entry_state: usize,
    resume_state: usize,
    names: &GeneratorNames,
) -> Result<Option<(Function, Function)>, String> {
    let Some(elem) = iter_elem(&f.ret) else { return Ok(None) };
    let Some((last, prelude)) = f.body.stmts.split_last() else { return Ok(None) };
    let (cond, owned_body) = match last {
        Stmt::Expr(Expr::While { cond, body }) => (cond.clone(), body.clone()),
        Stmt::Expr(Expr::WhileLet {
            pattern,
            scrutinee,
            body,
        }) => {
            // Preserve `while let` until generator lowering so a suspension in
            // its body can use the same selected-arm phase and binding restore
            // machinery as an ordinary `match`. The wildcard arm ends the
            // stream because this terminal loop has no source suffix.
            let dispatch = Expr::Match {
                scrutinee: scrutinee.clone(),
                arms: vec![
                    MatchArm {
                        line: 0,
                        pattern: pattern.clone(),
                        guard: None,
                        body: Expr::Block(body.clone()),
                    },
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Wildcard,
                        guard: None,
                        body: Expr::Block(Block {
                            stmts: vec![Stmt::Return(None)],
                            lines: vec![0],
                            region: None,
                        }),
                    },
                ],
            };
            (
                Box::new(Expr::Bool(true)),
                Block {
                    stmts: vec![Stmt::Expr(dispatch)],
                    lines: vec![0],
                    region: None,
                },
            )
        }
        _ => return Ok(None),
    };
    let body = &owned_body;
    let yields = body
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| matches!(statement, Stmt::Yield(_)).then_some(index))
        .collect::<Vec<_>>();
    let nested_conditional = if yields.is_empty() {
        single_nested_conditional_yield(body)
    } else {
        None
    };
    let nested_branches = if yields.is_empty() && nested_conditional.is_none() {
        nested_branch_yields(body)
    } else {
        None
    };
    let nested_match = if yields.is_empty()
        && nested_conditional.is_none()
        && nested_branches.is_none()
    {
        nested_match_yields(body)
    } else {
        None
    };
    if (yields.is_empty()
        && nested_conditional.is_none()
        && nested_branches.is_none()
        && nested_match.is_none())
        || (!yields.is_empty() && block_has_nested_yield(body))
        || block_has_generator_loop_control_transfer(body)
    {
        return Ok(None);
    }

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
    let parameter_count = bindings.len();
    let mut initializers = Vec::new();
    for statement in prelude {
        let Stmt::Let { name, ty, mutable, value } = statement else { return Ok(None) };
        let Some(ty) = ty
            .clone()
            .or_else(|| generator_frame_type_from_bindings(value, &bindings))
        else {
            return Ok(None);
        };
        bindings.push(GeneratorFrameBinding {
            name: name.clone(),
            ty,
            mutable: *mutable,
        });
        initializers.push(statement.clone());
    }
    if let Some(nested) = &nested_match
        && nested.arms.iter().any(|arm| arm.rebind_on_resume)
        && !matches!(&nested.scrutinee, Expr::Var(name) if bindings.iter().any(|binding| binding.name == *name))
    {
        return Ok(None);
    }

    let direct_live_locals = if yields.is_empty() {
        Vec::new()
    } else {
        let Some(live) = direct_loop_live_locals(body, &yields, &bindings) else {
            return Ok(None);
        };
        live
    };
    let live_locals = direct_live_locals
        .iter()
        .map(|local| local.binding.clone())
        .collect::<Vec<_>>();
    let mut frame_fields = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            if index < parameter_count {
                binding.ty.clone()
            } else {
                Type::Named("Option".into(), vec![binding.ty.clone()])
            }
        })
        .collect::<Vec<_>>();
    frame_fields.extend(
        live_locals
            .iter()
            .map(|binding| Type::Named("Option".into(), vec![binding.ty.clone()])),
    );
    frame_fields.push(Type::Named("Int".into(), Vec::new()));
    let frame_ty = Type::Tuple(frame_fields);
    let frame_name = names.name("frame");
    let resume_name = names.name("resume_after_yield");
    let yielded_name = names.name("yielded");
    let suspended_name = names.name("suspended");
    let mut step_statements = bindings[..parameter_count]
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
    step_statements.push(Stmt::Let {
        name: resume_name.clone(),
        ty: Some(Type::Named("Int".into(), Vec::new())),
        mutable: false,
        value: Expr::Field {
            base: Box::new(Expr::Var(frame_name.clone())),
            field: (bindings.len() + live_locals.len()).to_string(),
        },
    });
    let core_start = step_statements.len();
    if let Some(nested) = nested_conditional {
        step_statements.push(Stmt::Let {
            name: suspended_name.clone(),
            ty: Some(Type::Named("Bool".into(), Vec::new())),
            mutable: true,
            value: Expr::Bool(false),
        });
        let mut resume = nested.then_suffix;
        resume.extend(nested.outer_suffix.clone());
        let resume = rewrite_owned_frame_returns(Block {
            stmts: resume,
            lines: Vec::new(),
            region: None,
        })?;
        step_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Var(resume_name.clone())),
                rhs: Box::new(Expr::Int(1)),
            }),
            then_block: resume,
            else_block: None,
        }));

        let mut yielding_branch = rewrite_owned_frame_returns(Block {
            stmts: nested.then_prefix,
            lines: Vec::new(),
            region: None,
        })?
        .stmts;
        yielding_branch.push(Stmt::Assign {
            name: suspended_name.clone(),
            value: Expr::Bool(true),
        });

        let mut loop_statements = rewrite_owned_frame_returns(Block {
            stmts: nested.outer_prefix,
            lines: Vec::new(),
            region: None,
        })?
        .stmts;
        loop_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(nested.branch_condition),
            then_block: Block {
                stmts: yielding_branch,
                lines: Vec::new(),
                region: None,
            },
            else_block: nested
                .else_block
                .map(rewrite_owned_frame_returns)
                .transpose()?,
        }));
        let suffix = rewrite_owned_frame_returns(Block {
            stmts: nested.outer_suffix,
            lines: Vec::new(),
            region: None,
        })?;
        loop_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(Expr::Var(suspended_name.clone())),
            }),
            then_block: suffix,
            else_block: None,
        }));
        step_statements.push(Stmt::Expr(Expr::While {
            cond: Box::new(Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(cond.as_ref().clone()),
                rhs: Box::new(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(Expr::Var(suspended_name.clone())),
                }),
            }),
            body: Block {
                stmts: loop_statements,
                lines: Vec::new(),
                region: None,
            },
        }));
        let mut next_frame_fields = capture_frame_bindings(&bindings, parameter_count);
        next_frame_fields.push(Expr::Int(1));
        step_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Var(suspended_name)),
            then_block: Block {
                stmts: vec![Stmt::Expr(Expr::Ctor {
                    name: "Some".into(),
                    args: vec![Expr::Tuple(vec![
                        nested.yielded,
                        Expr::Tuple(next_frame_fields),
                    ])],
                })],
                lines: Vec::new(),
                region: None,
            },
            else_block: Some(Block {
                stmts: vec![Stmt::Expr(Expr::Ctor {
                    name: "None".into(),
                    args: Vec::new(),
                })],
                lines: Vec::new(),
                region: None,
            }),
        }));
    } else if let Some(nested) = nested_branches {
        let yielded_option_name = names.name("yielded_option");
        step_statements.push(Stmt::Let {
            name: suspended_name.clone(),
            ty: Some(Type::Named("Int".into(), Vec::new())),
            mutable: true,
            value: Expr::Int(0),
        });
        step_statements.push(Stmt::Let {
            name: yielded_option_name.clone(),
            ty: Some(Type::Named("Option".into(), vec![elem.clone()])),
            mutable: true,
            value: Expr::Ctor { name: "None".into(), args: Vec::new() },
        });
        for (phase, suffix) in [(1_i64, nested.then_suffix), (2_i64, nested.else_suffix)] {
            let mut resume = suffix;
            resume.extend(nested.outer_suffix.clone());
            step_statements.push(Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(resume_name.clone())),
                    rhs: Box::new(Expr::Int(phase)),
                }),
                then_block: rewrite_owned_frame_returns(Block {
                    stmts: resume,
                    lines: Vec::new(),
                    region: None,
                })?,
                else_block: None,
            }));
        }

        let make_yielding_branch = |prefix: Vec<Stmt>, yielded: Expr, phase: i64| {
            let mut statements = rewrite_owned_frame_returns(Block {
                stmts: prefix,
                lines: Vec::new(),
                region: None,
            })?
            .stmts;
            statements.push(Stmt::Assign {
                name: yielded_option_name.clone(),
                value: Expr::Ctor { name: "Some".into(), args: vec![yielded] },
            });
            statements.push(Stmt::Assign {
                name: suspended_name.clone(),
                value: Expr::Int(phase),
            });
            Ok::<Block, String>(Block { stmts: statements, lines: Vec::new(), region: None })
        };
        let mut loop_statements = rewrite_owned_frame_returns(Block {
            stmts: nested.outer_prefix,
            lines: Vec::new(),
            region: None,
        })?
        .stmts;
        loop_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(nested.branch_condition),
            then_block: make_yielding_branch(nested.then_prefix, nested.then_yielded, 1)?,
            else_block: Some(make_yielding_branch(
                nested.else_prefix,
                nested.else_yielded,
                2,
            )?),
        }));
        step_statements.push(Stmt::Expr(Expr::While {
            cond: Box::new(Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(cond.as_ref().clone()),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(suspended_name.clone())),
                    rhs: Box::new(Expr::Int(0)),
                }),
            }),
            body: Block { stmts: loop_statements, lines: Vec::new(), region: None },
        }));
        let mut next_frame_fields = capture_frame_bindings(&bindings, parameter_count);
        next_frame_fields.push(Expr::Var(suspended_name));
        step_statements.push(Stmt::Expr(Expr::Match {
            scrutinee: Box::new(Expr::Var(yielded_option_name)),
            arms: vec![
                MatchArm {
                    line: 0,
                    pattern: Pattern::Ctor {
                        name: "Some".into(),
                        args: vec![Pattern::Var(yielded_name.clone())],
                    },
                    guard: None,
                    body: Expr::Ctor {
                        name: "Some".into(),
                        args: vec![Expr::Tuple(vec![
                            Expr::Var(yielded_name),
                            Expr::Tuple(next_frame_fields),
                        ])],
                    },
                },
                MatchArm {
                    line: 0,
                    pattern: Pattern::Ctor { name: "None".into(), args: Vec::new() },
                    guard: None,
                    body: Expr::Ctor { name: "None".into(), args: Vec::new() },
                },
            ],
        }));
    } else if let Some(nested) = nested_match {
        let yielded_option_name = names.name("yielded_option");
        step_statements.push(Stmt::Let {
            name: suspended_name.clone(),
            ty: Some(Type::Named("Int".into(), Vec::new())),
            mutable: true,
            value: Expr::Int(0),
        });
        step_statements.push(Stmt::Let {
            name: yielded_option_name.clone(),
            ty: Some(Type::Named("Option".into(), vec![elem.clone()])),
            mutable: true,
            value: Expr::Ctor { name: "None".into(), args: Vec::new() },
        });
        for (index, arm) in nested.arms.iter().enumerate() {
            if arm.yielded.is_none() {
                continue;
            }
            let mut resume = arm.suffix.clone();
            resume.extend(nested.outer_suffix.clone());
            let resume = rewrite_owned_frame_returns(Block {
                stmts: resume,
                lines: Vec::new(),
                region: None,
            })?;
            let resume = if arm.rebind_on_resume {
                Block {
                    stmts: vec![Stmt::Expr(Expr::Match {
                        scrutinee: Box::new(nested.scrutinee.clone()),
                        arms: vec![
                            MatchArm {
                                line: arm.line,
                                pattern: arm.pattern.clone(),
                                // The chosen phase already proves that the
                                // source guard succeeded. Re-running it could
                                // duplicate effects.
                                guard: None,
                                body: Expr::Block(resume),
                            },
                            MatchArm {
                                line: 0,
                                pattern: Pattern::Wildcard,
                                guard: None,
                                body: Expr::Block(Block {
                                    stmts: Vec::new(),
                                    lines: Vec::new(),
                                    region: None,
                                }),
                            },
                        ],
                    })],
                    lines: Vec::new(),
                    region: None,
                }
            } else {
                resume
            };
            step_statements.push(Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(resume_name.clone())),
                    rhs: Box::new(Expr::Int((index + 1) as i64)),
                }),
                then_block: resume,
                else_block: None,
            }));
        }

        let mut loop_statements = rewrite_owned_frame_returns(Block {
            stmts: nested.outer_prefix,
            lines: Vec::new(),
            region: None,
        })?
        .stmts;
        let arms = nested
            .arms
            .into_iter()
            .enumerate()
            .map(|(index, arm)| {
                let mut statements = rewrite_owned_frame_returns(Block {
                    stmts: arm.prefix,
                    lines: Vec::new(),
                    region: None,
                })?
                .stmts;
                if let Some(yielded) = arm.yielded {
                    statements.push(Stmt::Assign {
                        name: yielded_option_name.clone(),
                        value: Expr::Ctor { name: "Some".into(), args: vec![yielded] },
                    });
                    statements.push(Stmt::Assign {
                        name: suspended_name.clone(),
                        value: Expr::Int((index + 1) as i64),
                    });
                }
                Ok::<MatchArm, String>(MatchArm {
                    line: arm.line,
                    pattern: arm.pattern,
                    guard: arm.guard,
                    body: Expr::Block(Block {
                        stmts: statements,
                        lines: Vec::new(),
                        region: None,
                    }),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        loop_statements.push(Stmt::Expr(Expr::Match {
            scrutinee: Box::new(nested.scrutinee),
            arms,
        }));
        step_statements.push(Stmt::Expr(Expr::While {
            cond: Box::new(Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(cond.as_ref().clone()),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(suspended_name.clone())),
                    rhs: Box::new(Expr::Int(0)),
                }),
            }),
            body: Block { stmts: loop_statements, lines: Vec::new(), region: None },
        }));
        let mut next_frame_fields = capture_frame_bindings(&bindings, parameter_count);
        next_frame_fields.push(Expr::Var(suspended_name));
        step_statements.push(Stmt::Expr(Expr::Match {
            scrutinee: Box::new(Expr::Var(yielded_option_name)),
            arms: vec![
                MatchArm {
                    line: 0,
                    pattern: Pattern::Ctor {
                        name: "Some".into(),
                        args: vec![Pattern::Var(yielded_name.clone())],
                    },
                    guard: None,
                    body: Expr::Ctor {
                        name: "Some".into(),
                        args: vec![Expr::Tuple(vec![
                            Expr::Var(yielded_name),
                            Expr::Tuple(next_frame_fields),
                        ])],
                    },
                },
                MatchArm {
                    line: 0,
                    pattern: Pattern::Ctor { name: "None".into(), args: Vec::new() },
                    guard: None,
                    body: Expr::Ctor { name: "None".into(), args: Vec::new() },
                },
            ],
        }));
    } else {
        let first_yield_index = yields[0];
        let Stmt::Yield(first_yielded) = &body.stmts[first_yield_index] else { unreachable!() };
        step_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(Expr::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(Expr::Var(resume_name.clone())),
                    rhs: Box::new(Expr::Int(0)),
                }),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Gt,
                    lhs: Box::new(Expr::Var(resume_name.clone())),
                    rhs: Box::new(Expr::Int(yields.len() as i64)),
                }),
            }),
            then_block: Block {
                stmts: vec![Stmt::Return(Some(Expr::Ctor {
                    name: "None".into(),
                    args: Vec::new(),
                }))],
                lines: Vec::new(),
                region: None,
            },
            else_block: None,
        }));
        for (phase, window) in yields.windows(2).enumerate() {
            let previous_yield = window[0];
            let next_yield = window[1];
            let Stmt::Yield(yielded) = &body.stmts[next_yield] else { unreachable!() };
            let resume = rewrite_owned_frame_returns(Block {
                stmts: body.stmts[previous_yield + 1..next_yield].to_vec(),
                lines: Vec::new(),
                region: None,
            })?;
            let mut resume = resume.stmts;
            resume.push(Stmt::Let {
                name: yielded_name.clone(),
                ty: Some(elem.clone()),
                mutable: false,
                value: yielded.clone(),
            });
            let mut next_frame_fields = capture_frame_bindings(&bindings, parameter_count);
            next_frame_fields.extend(capture_direct_loop_locals(
                &frame_name,
                bindings.len(),
                &direct_live_locals,
                next_yield,
            ));
            next_frame_fields.push(Expr::Int((phase + 2) as i64));
            resume.push(Stmt::Return(Some(Expr::Ctor {
                name: "Some".into(),
                args: vec![Expr::Tuple(vec![
                    Expr::Var(yielded_name.clone()),
                    Expr::Tuple(next_frame_fields),
                ])],
            })));
            let resume = restore_direct_loop_locals(
                &frame_name,
                bindings.len(),
                &direct_live_locals,
                previous_yield,
                names,
                Block { stmts: resume, lines: Vec::new(), region: None },
            );
            step_statements.push(Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(resume_name.clone())),
                    rhs: Box::new(Expr::Int((phase + 1) as i64)),
                }),
                then_block: resume,
                else_block: None,
            }));
        }

        let last_yield_index = *yields.last().expect("non-empty direct yield set");
        let after = rewrite_owned_frame_returns(Block {
            stmts: body.stmts[last_yield_index + 1..].to_vec(),
            lines: Vec::new(),
            region: None,
        })?;
        let after = restore_direct_loop_locals(
            &frame_name,
            bindings.len(),
            &direct_live_locals,
            last_yield_index,
            names,
            after,
        );
        step_statements.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Var(resume_name.clone())),
                rhs: Box::new(Expr::Int(yields.len() as i64)),
            }),
            then_block: after,
            else_block: None,
        }));

        let mut produce = rewrite_owned_frame_returns(Block {
            stmts: body.stmts[..first_yield_index].to_vec(),
            lines: Vec::new(),
            region: None,
        })?
        .stmts;
        produce.push(Stmt::Let {
            name: yielded_name.clone(),
            ty: Some(elem.clone()),
            mutable: false,
            value: first_yielded.clone(),
        });
        let mut next_frame_fields = capture_frame_bindings(&bindings, parameter_count);
        next_frame_fields.extend(capture_direct_loop_locals(
            &frame_name,
            bindings.len(),
            &direct_live_locals,
            first_yield_index,
        ));
        next_frame_fields.push(Expr::Int(1));
        let next_frame = Expr::Tuple(next_frame_fields);
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
    }

    let core = Block {
        stmts: step_statements.split_off(core_start),
        lines: Vec::new(),
        region: None,
    };
    let mut entry = initializers.clone();
    entry.extend(core.stmts.clone());
    let resumed = restore_optional_bindings(
        &frame_name,
        parameter_count,
        &bindings[parameter_count..],
        names,
        "prelude",
        core,
    );
    step_statements.push(Stmt::Expr(Expr::If {
        cond: Box::new(Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(Expr::Var(resume_name)),
            rhs: Box::new(Expr::Int(0)),
        }),
        then_block: Block {
            stmts: entry,
            lines: Vec::new(),
            region: None,
        },
        else_block: Some(resumed),
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

    let wrapper_statements = vec![Stmt::Expr(Expr::Call {
        name: "iter.unfold".into(),
        args: vec![
            Expr::Tuple(
                initial_frame_bindings(&bindings, parameter_count)
                    .into_iter()
                    .chain(live_locals.iter().map(|_| Expr::Ctor {
                        name: "None".into(),
                        args: Vec::new(),
                    }))
                    .chain(std::iter::once(Expr::Int(0)))
                    .collect(),
            ),
            Expr::Var(helper_name.to_string()),
        ],
    })];
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

fn generator_frame_type_from_bindings(
    value: &Expr,
    bindings: &[GeneratorFrameBinding],
) -> Option<Type> {
    match value {
        Expr::Var(name) => bindings
            .iter()
            .find(|binding| binding.name == *name)
            .map(|binding| binding.ty.clone()),
        Expr::Tuple(values) => Some(Type::Tuple(
            values
                .iter()
                .map(|value| generator_frame_type_from_bindings(value, bindings))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::List(values) => {
            let first = generator_frame_type_from_bindings(values.first()?, bindings)?;
            values
                .iter()
                .skip(1)
                .all(|value| {
                    generator_frame_type_from_bindings(value, bindings).as_ref()
                        == Some(&first)
                })
                .then(|| Type::Named("List".into(), vec![first]))
        }
        Expr::Binary { op, lhs, rhs } => match op {
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::LtEq
            | BinOp::Gt
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or => Some(Type::Named("Bool".into(), Vec::new())),
            BinOp::Coalesce => generator_frame_type_from_bindings(rhs, bindings),
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::Concat
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => {
                let lhs = generator_frame_type_from_bindings(lhs, bindings)?;
                let rhs = generator_frame_type_from_bindings(rhs, bindings)?;
                (lhs == rhs).then_some(lhs)
            }
        },
        Expr::Unary { op, expr } => match op {
            UnOp::Not => Some(Type::Named("Bool".into(), Vec::new())),
            UnOp::Neg | UnOp::BitNot | UnOp::Move => {
                generator_frame_type_from_bindings(expr, bindings)
            }
            UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref | UnOp::Await => None,
        },
        Expr::As { ty, .. } => Some(ty.clone()),
        _ => generator_frame_type(value),
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

fn block_has_generator_loop_control_transfer(block: &Block) -> bool {
    fn directly_transfers(block: &Block) -> bool {
        block.stmts.iter().any(|statement| {
            matches!(statement, Stmt::Return(Some(_)) | Stmt::Break | Stmt::Continue)
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

/// A bare source `return` ends the generator. In an owned resume helper that
/// means returning the helper's `Option((item, frame))` value `None`. This
/// rewrite is applied independently to the pre-yield and post-yield segments,
/// so post-yield effects and termination happen on the next pull, never eagerly
/// before the current item is observed.
fn rewrite_owned_frame_returns(block: Block) -> Result<Block, String> {
    let mut statements = Vec::with_capacity(block.stmts.len());
    for statement in block.stmts {
        statements.push(match statement {
            Stmt::Return(None) => Stmt::Return(Some(Expr::Ctor {
                name: "None".into(),
                args: Vec::new(),
            })),
            Stmt::Return(Some(_)) => {
                return Err("`return <value>` is not allowed in a generator".into());
            }
            Stmt::Let {
                name,
                ty,
                mutable,
                value,
            } => Stmt::Let {
                name,
                ty,
                mutable,
                value: rewrite_owned_frame_return_expr(value)?,
            },
            Stmt::Assign { name, value } => Stmt::Assign {
                name,
                value: rewrite_owned_frame_return_expr(value)?,
            },
            Stmt::LetPattern { pattern, value } => Stmt::LetPattern {
                pattern,
                value: rewrite_owned_frame_return_expr(value)?,
            },
            Stmt::Expr(value) => Stmt::Expr(rewrite_owned_frame_return_expr(value)?),
            other => other,
        });
    }
    Ok(Block {
        stmts: statements,
        lines: block.lines,
        region: block.region,
    })
}

fn rewrite_owned_frame_return_expr(expression: Expr) -> Result<Expr, String> {
    Ok(match expression {
        Expr::If {
            cond,
            then_block,
            else_block,
        } => Expr::If {
            cond,
            then_block: rewrite_owned_frame_returns(then_block)?,
            else_block: else_block.map(rewrite_owned_frame_returns).transpose()?,
        },
        Expr::While { cond, body } => Expr::While {
            cond,
            body: rewrite_owned_frame_returns(body)?,
        },
        Expr::For { var, iter, body } => Expr::For {
            var,
            iter,
            body: rewrite_owned_frame_returns(body)?,
        },
        Expr::WhileLet {
            pattern,
            scrutinee,
            body,
        } => Expr::WhileLet {
            pattern,
            scrutinee,
            body: rewrite_owned_frame_returns(body)?,
        },
        Expr::Match { scrutinee, arms } => Expr::Match {
            scrutinee,
            arms: arms
                .into_iter()
                .map(|arm| {
                    Ok(MatchArm {
                        line: arm.line,
                        pattern: arm.pattern,
                        guard: arm.guard,
                        body: rewrite_owned_frame_return_expr(arm.body)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        },
        Expr::Block(block) => Expr::Block(rewrite_owned_frame_returns(block)?),
        other => other,
    })
}

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
            Stmt::Let { name, ty, mutable, value } => out.push(Stmt::Let {
                name,
                ty,
                mutable,
                value: rewrite_expr(value, gen_name, in_region)?,
            }),
            Stmt::Assign { name, value } => out.push(Stmt::Assign {
                name,
                value: rewrite_expr(value, gen_name, in_region)?,
            }),
            Stmt::LetPattern { pattern, value } => out.push(Stmt::LetPattern {
                pattern,
                value: rewrite_expr(value, gen_name, in_region)?,
            }),
            Stmt::Return(None) => out.push(Stmt::Return(Some(Expr::Ctor {
                name: "None".to_string(),
                args: vec![],
            }))),
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

/// Rewrite nested control-flow blocks for the compatibility fallback while
/// their equivalent owned-frame lowering is migrated.
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
        Expr::WhileLet { pattern, scrutinee, body } => Expr::WhileLet {
            pattern,
            scrutinee,
            body: rewrite_block(body, gen_name, in_region)?,
        },
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
        assert!(generated.iter().all(|function| !function
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_BOXED_ATTRIBUTE)));
        assert!(matches!(
            wrapper.body.stmts.last(),
            Some(Stmt::Expr(Expr::Call { name, .. })) if name == "iter.unfold"
        ));
    }

    #[test]
    fn finite_direct_yields_use_a_one_shot_owned_phase_frame() {
        let module = crate::parser::parse_module(
            "gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n",
        )
        .expect("parse finite generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower finite generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.unfold"), "finite generator needs an owned frame: {debug}");
        assert!(!debug.contains("iter.from_gen"), "finite generator must not replay: {debug}");
        assert!(debug.contains("once"), "finite frame must remember completion: {debug}");
        assert!(debug.contains("Int(2)"), "second yield needs its own phase: {debug}");
    }

    #[test]
    fn finite_conditional_then_direct_yield_uses_owned_phases() {
        let module = crate::parser::parse_module(
            "gen fn values() -> Iter(Int):\n    if true:\n        yield 1\n    yield 2\n",
        )
        .expect("parse finite conditional generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked)
            .expect("lower finite conditional generator")
            .into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.unfold"), "conditional generator needs an owned frame: {debug}");
        assert!(!debug.contains("iter.from_gen"), "conditional generator must not replay: {debug}");
        assert!(debug.contains("finite_stage"), "conditional frame must record its stage: {debug}");
    }

    #[test]
    fn prefix_yield_then_terminal_loop_uses_one_owned_frame() {
        let module = crate::parser::parse_module(
            "gen fn collatz(start: Int) -> Iter(Int):\n    var n = start\n    yield n\n    while n > 1:\n        if n % 2 == 0:\n            n = n / 2\n        else:\n            n = 3 * n + 1\n        yield n\n",
        )
        .expect("parse prefix-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked)
            .expect("lower prefix-yield generator")
            .into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.unfold"), "Collatz needs an owned frame: {debug}");
        assert!(!debug.contains("iter.from_gen"), "Collatz must not replay its seed: {debug}");
        assert!(debug.contains("once"), "Collatz must record whether the seed was yielded: {debug}");
    }

    #[test]
    fn terminal_for_yield_uses_an_owned_indexed_frame() {
        let module = crate::parser::parse_module(
            "gen fn values() -> Iter(Int):\n    for value in [1, 2]:\n        yield value\n",
        )
        .expect("parse for-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower terminal for generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.unfold"), "terminal for needs an owned frame: {debug}");
        assert!(!debug.contains("iter.from_gen"), "terminal for must not replay: {debug}");
        assert!(debug.contains("for_index"), "the frame must carry its list index: {debug}");
    }

    #[test]
    fn tuple_pattern_local_crossing_a_yield_uses_frame_fields() {
        let module = crate::parser::parse_module(
            "gen fn values() -> Iter(Int):\n    var running = true\n    while running:\n        let (a, b) = (1, 2)\n        yield a\n        running = false\n        yield b\n",
        )
        .expect("parse pattern-local generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower pattern-local generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(!debug.contains("iter.from_gen"), "tuple pattern locals must not replay: {debug}");
        assert!(debug.contains("direct_live"), "the live tuple field must be restored: {debug}");
    }

    #[test]
    fn conditional_local_crossing_a_yield_waits_for_branch_frame_fields() {
        let module = crate::parser::parse_module(
            "gen fn values() -> Iter(Int):\n    var running = true\n    while running:\n        if running:\n            let value = 7\n            yield value\n            running = false\n            let after = value\n",
        )
        .expect("parse branch-local generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("preserve branch-local generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.from_gen"), "branch locals need explicit frame fields: {debug}");
    }

    #[test]
    fn mutated_match_scrutinee_waits_for_pattern_binding_frame_fields() {
        let module = crate::parser::parse_module(
            "gen fn values() -> Iter(Int):\n    var current: Option(Int) = Some(1)\n    while true:\n        match current:\n            Some(value) ->\n                current = None\n                yield value\n                let after = value\n            None -> return\n",
        )
        .expect("parse mutated-match generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("preserve mutated-match generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(debug.contains("iter.from_gen"), "match bindings need explicit frame fields: {debug}");
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
            Some(Type::Tuple(ref fields)) if fields.len() == 3
        ));
        assert!(!format!("{:?}", lowered).contains("iter.from_gen"));
    }

    #[test]
    fn generator_early_return_stays_in_the_owned_frame() {
        let module = crate::parser::parse_module(
            "gen fn firstn(n: Int) -> Iter(Int):\n    var i = 0\n    while true:\n        if i >= n:\n            return\n        yield i\n        i = i + 1\n",
        )
        .expect("parse generator return");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        let debug = format!("{:?}", lowered);
        assert!(!debug.contains("iter.from_gen"), "early return must not select replay: {debug}");
        assert!(debug.contains("resume_after_yield"));
    }

    #[test]
    fn direct_multi_yield_loop_uses_owned_resume_phases() {
        let module = crate::parser::parse_module(
            "gen fn pairs() -> Iter(Int):\n    var i = 0\n    while i < 3:\n        yield i\n        i = i + 10\n        yield i\n        i = i - 9\n",
        )
        .expect("parse multi-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        let debug = format!("{:?}", lowered);
        assert!(!debug.contains("iter.from_gen"), "direct yields must not replay: {debug}");
        assert!(debug.contains("resume_after_yield"));
        assert!(debug.contains("Int(2)"), "second yield needs its own resume phase: {debug}");
    }

    #[test]
    fn conditional_yield_loop_resumes_inside_the_taken_branch() {
        let module = crate::parser::parse_module(
            "gen fn evens() -> Iter(Int):\n    var i = 0\n    while i < 4:\n        if i % 2 == 0:\n            yield i\n            i = i + 1\n        else:\n            i = i + 1\n",
        )
        .expect("parse conditional-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower generator").into_module();
        let debug = format!("{:?}", lowered);
        assert!(!debug.contains("iter.from_gen"), "conditional yield must not replay: {debug}");
        assert!(debug.contains("resume_after_yield"));
    }

    #[test]
    fn two_yielding_branches_record_the_resuming_branch() {
        let module = crate::parser::parse_module(
            "gen fn alternating() -> Iter(Int):\n    var i = 0\n    while i < 4:\n        if i % 2 == 0:\n            yield i\n            i = i + 1\n        else:\n            yield i + 10\n            i = i + 1\n",
        )
        .expect("parse branch-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower branch-yield generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(!debug.contains("iter.from_gen"), "branch yields must not replay: {debug}");
        assert!(debug.contains("resume_after_yield"));
        assert!(debug.contains("yielded_option"));
        assert!(debug.contains("Int(2)"), "the else branch needs a distinct resume phase: {debug}");
    }

    #[test]
    fn yielding_match_arms_record_the_resuming_arm() {
        let module = crate::parser::parse_module(
            "gen fn alternating() -> Iter(Int):\n    var i = 0\n    var current: Option(Int) = Some(0)\n    while i < 4:\n        match current:\n            Some(value) ->\n                yield value\n                i = i + 1\n                current = None\n            None ->\n                yield i + 10\n                i = i + 1\n                current = Some(i)\n",
        )
        .expect("parse match-yield generator");
        let checked = crate::source_check::check(module).expect("source check");
        let lowered = lower(checked).expect("lower match-yield generator").into_module();
        let debug = format!("{lowered:?}");
        assert!(!debug.contains("iter.from_gen"), "match-arm yields must not replay: {debug}");
        assert!(debug.contains("resume_after_yield"));
        assert!(debug.contains("yielded_option"));
        assert!(debug.contains("Int(2)"), "the second match arm needs its own phase: {debug}");
    }
}
