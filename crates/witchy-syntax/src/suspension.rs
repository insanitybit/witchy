//! Shared compiler contract for values carried across a suspension point.
//!
//! Async and generator lowering must agree on lexical binding identity and
//! deterministic slot order before they can share one owned runtime frame ABI.
//! This module is the single binder-aware free-variable analysis used to derive
//! those slots. It deliberately reports source names in first-use order; each
//! lowering intersects that order with its lexical scope to retain declaration
//! order and attach the checked slot type/ownership metadata.

use crate::ast::{pattern_binds, Block, Expr, Function, Stmt};
use foldhash::{HashSet, HashSetExt as _};

/// Compiler-owned marker on generated segment functions. An `own` parameter on
/// an ordinary function is a consuming operation; on a suspension segment it
/// is an affine frame transfer, so the callee assumes (rather than discharges)
/// any must-consume obligation carried by that slot.
pub const FRAME_FUNCTION_ATTRIBUTE: &str = "__compiler_suspension_frame";

/// Compiler-owned marker on the ordinary entry wrapper of an async function.
/// The wrapper is not itself a frame transfer (and therefore must not receive
/// [`FRAME_FUNCTION_ATTRIBUTE`]), but a synthesized executor needs to know the
/// first state associated with this source callable.
pub const FRAME_ENTRY_ATTRIBUTE: &str = "__compiler_suspension_entry";

/// Source callable represented by a compiler-generated suspension segment.
/// The runtime function name remains an internal dispatch identity; diagnostics
/// use this attribute so lowering never leaks `__async_*` names to users.
pub const SOURCE_CALLABLE_ATTRIBUTE_PREFIX: &str = "__compiler_source_callable=";

/// Compiler-owned marker for the historical replay-based generator adapter.
/// It has suspension metadata for diagnostics and target propagation, but it
/// is not an owned direct frame and must keep the module on the boxed carrier.
pub const FRAME_BOXED_ATTRIBUTE: &str = "__compiler_suspension_boxed";

/// Prefix for the stable, module-local integer state attached to every async
/// entry and lifted segment. The state is data consumed by the typed carrier
/// catalog; generated function names are deliberately not the runtime ABI.
pub const FRAME_STATE_ATTRIBUTE_PREFIX: &str = "__compiler_suspension_state=";

/// Stable identity for one compiler-formed async region.  A region is the
/// entry or continuation state between two suspension boundaries.  It is
/// deliberately distinct metadata from the generated function name: later
/// typed lowering may promote a region while preserving ordinary statements
/// before or after its task expression in source order.
pub const ASYNC_REGION_ATTRIBUTE_PREFIX: &str = "__compiler_async_region=";

/// Identity of the source suspension whose successful completion enters a
/// generated continuation state. It is per source callable and assigned in
/// source AST preorder before continuation lowering recurses, so it remains
/// stable even when the CPS transform emits later continuations first.
pub const SOURCE_SUSPENSION_ATTRIBUTE_PREFIX: &str = "__compiler_source_suspension=";

/// Unspellable local in the checked task expression that binds a source await
/// to the scheduler call or direct-return task that actually executes it.
pub const SOURCE_TASK_OPERAND_PREFIX: &str = "@async.await.operand:";


/// Maps one physical parameter of a compiler-generated async frame to the
/// lexical source binding that supplied it.  Both numbers are local to the
/// source callable: the parameter is a final callable ABI position, while the
/// binding is a source declaration coordinate.  It is deliberately metadata
/// on the generated function rather than a parameter annotation so ordinary
/// source functions cannot forge it.
pub const FRAME_BINDING_ATTRIBUTE_PREFIX: &str = "__compiler_frame_binding=";

/// Source-item identity retained across async CPS lowering. This is compiler
/// metadata only; final execution IDs are assigned after typed layout.
pub const ASYNC_SURFACE_ATTRIBUTE_PREFIX: &str = "__compiler_async_surface=";

/// Linked-module identity paired with [`ASYNC_SURFACE_ATTRIBUTE_PREFIX`]. The
/// item number alone is module-local, so final typed lowering carries the
/// module before it assigns canonical execution IDs.
pub const ASYNC_SURFACE_MODULE_ATTRIBUTE_PREFIX: &str = "__compiler_async_surface_module=";

pub fn async_surface_attribute(item: usize, method: Option<usize>) -> String {
    match method {
        Some(method) => format!("{ASYNC_SURFACE_ATTRIBUTE_PREFIX}{item}:{method}"),
        None => format!("{ASYNC_SURFACE_ATTRIBUTE_PREFIX}{item}:_"),
    }
}

pub fn async_surface(function: &Function) -> Option<(usize, Option<usize>)> {
    function.attributes.iter().find_map(|attribute| {
        let encoded = attribute.strip_prefix(ASYNC_SURFACE_ATTRIBUTE_PREFIX)?;
        let (item, method) = encoded.split_once(':')?;
        let item = item.parse().ok()?;
        let method = (method != "_").then(|| method.parse().ok()).flatten();
        Some((item, method))
    })
}

pub fn async_surface_module_attribute(module: &str) -> String {
    format!("{ASYNC_SURFACE_MODULE_ATTRIBUTE_PREFIX}{module}")
}

pub fn async_surface_module(function: &Function) -> Option<&str> {
    function.attributes.iter().find_map(|attribute| {
        attribute.strip_prefix(ASYNC_SURFACE_MODULE_ATTRIBUTE_PREFIX)
    })
}

pub fn frame_state_attribute(state: usize) -> String {
    format!("{FRAME_STATE_ATTRIBUTE_PREFIX}{state}")
}

/// Recover the compiler-owned state identity from a generated callable.
pub fn frame_state(function: &Function) -> Option<usize> {
    function.attributes.iter().find_map(|attribute| {
        attribute
            .strip_prefix(FRAME_STATE_ATTRIBUTE_PREFIX)
            .and_then(|state| state.parse().ok())
    })
}

pub fn async_region_attribute(region: usize) -> String {
    format!("{ASYNC_REGION_ATTRIBUTE_PREFIX}{region}")
}

/// Recover the region identity assigned by async lowering.  Generator helpers
/// carry suspension state metadata but intentionally have no async region.
pub fn async_region(function: &Function) -> Option<usize> {
    function.attributes.iter().find_map(|attribute| {
        attribute
            .strip_prefix(ASYNC_REGION_ATTRIBUTE_PREFIX)
            .and_then(|region| region.parse().ok())
    })
}

pub fn source_suspension_attribute(suspension: u32) -> String {
    format!("{SOURCE_SUSPENSION_ATTRIBUTE_PREFIX}{suspension}")
}

/// Recover the source suspension that enters this continuation. Entries and
/// compiler-formed loop joins intentionally have no such identity.
pub fn source_suspension(function: &Function) -> Option<u32> {
    function.attributes.iter().find_map(|attribute| {
        attribute
            .strip_prefix(SOURCE_SUSPENSION_ATTRIBUTE_PREFIX)
            .and_then(|suspension| suspension.parse().ok())
    })
}

pub fn frame_binding_attribute(parameter: usize, source_binding: u32) -> String {
    format!("{FRAME_BINDING_ATTRIBUTE_PREFIX}{parameter}:{source_binding}")
}

/// Recover all compiler-stamped frame parameter bindings. Malformed metadata
/// is not silently ignored: consumers must reject it rather than guessing a
/// lane association from a generated name.
pub fn frame_bindings(function: &Function) -> Result<Vec<(usize, u32)>, String> {
    function.attributes.iter().filter_map(|attribute| {
        let encoded = attribute.strip_prefix(FRAME_BINDING_ATTRIBUTE_PREFIX)?;
        Some((|| {
            let (parameter, binding) = encoded.split_once(':')
                .ok_or_else(|| format!("malformed frame binding attribute `{attribute}`"))?;
            let parameter = parameter.parse().map_err(|_| format!(
                "malformed frame binding parameter in `{attribute}`"
            ))?;
            let binding = binding.parse().map_err(|_| format!(
                "malformed frame binding source coordinate in `{attribute}`"
            ))?;
            Ok((parameter, binding))
        })())
    }).collect()
}

pub fn source_callable_attribute(name: &str) -> String {
    format!("{SOURCE_CALLABLE_ATTRIBUTE_PREFIX}{name}")
}

pub fn source_callable_name(function: &Function) -> String {
    let Some(source) = function.attributes.iter().find_map(|attribute| {
        attribute.strip_prefix(SOURCE_CALLABLE_ATTRIBUTE_PREFIX)
    }) else {
        return function.name.clone();
    };
    match function.name.rsplit_once('.') {
        Some((module, _)) if !source.starts_with(&format!("{module}.")) => {
            format!("{module}.{source}")
        }
        _ => source.to_string(),
    }
}

/// Free lexical bindings read or assigned by `expression`, in deterministic
/// first-use order. Bindings introduced by nested blocks, patterns, loops, and
/// lambdas are excluded from the result.
pub(crate) fn free_bindings(expression: &Expr) -> Vec<String> {
    free_bindings_with_bound(expression, &HashSet::new())
}

pub(crate) fn free_bindings_with_bound(
    expression: &Expr,
    bound: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::<&str>::new();
    let mut order = Vec::new();
    visit_expr(expression, bound, &mut seen, &mut order);
    order.into_iter().map(str::to_owned).collect()
}

/// Free lexical bindings as borrowed names. Consumers that only need
/// membership can avoid allocating a `String` for every distinct identifier.
pub(crate) fn free_binding_set(expression: &Expr) -> HashSet<&str> {
    let mut seen = HashSet::<&str>::new();
    let mut order = Vec::new();
    visit_expr(expression, &HashSet::new(), &mut seen, &mut order);
    seen
}

pub(crate) fn free_bindings_in_block(
    block: &Block,
    bound: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::<&str>::new();
    let mut order = Vec::new();
    visit_block(block, bound, &mut seen, &mut order);
    order.into_iter().map(str::to_owned).collect()
}

fn visit_expr<'a>(
    expression: &'a Expr,
    bound: &HashSet<String>,
    seen: &mut HashSet<&'a str>,
    output: &mut Vec<&'a str>,
) {
    match expression {
        Expr::Var(name) => {
            if !bound.contains(name) && seen.insert(name.as_str()) {
                output.push(name);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr(expr, bound, seen, output),
        Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::Index { base, index } | Expr::Binary { lhs: base, rhs: index, .. } => {
            visit_expr(base, bound, seen, output);
            visit_expr(index, bound, seen, output);
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr(lo, bound, seen, output);
            visit_expr(hi, bound, seen, output);
        }
        Expr::List(items) | Expr::Tuple(items) => visit_all(items, bound, seen, output),
        Expr::Call { name, args } => {
            // Before linker rewriting, application of a local callable is still
            // represented as a named `Call`. Report a bare callee as a possible
            // lexical binding; the lowering's scope intersection discards real
            // top-level function names.
            if !name.contains('.') && !bound.contains(name) && seen.insert(name.as_str()) {
                output.push(name);
            }
            visit_all(args, bound, seen, output);
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            visit_all(args, bound, seen, output);
        }
        Expr::LabeledCall { name, args } => {
            if !name.contains('.') && !bound.contains(name) && seen.insert(name.as_str()) {
                output.push(name);
            }
            for (_, argument) in args {
                visit_expr(argument, bound, seen, output);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            for (_, argument) in args {
                visit_expr(argument, bound, seen, output);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::Apply { func, args } => {
            visit_expr(func, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr(base, bound, seen, output);
            for (_, value) in fields {
                visit_expr(value, bound, seen, output);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr(value, bound, seen, output);
            }
            if let Some(spread) = spread {
                visit_expr(spread, bound, seen, output);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr(cond, bound, seen, output);
            visit_block(then_block, bound, seen, output);
            if let Some(else_block) = else_block {
                visit_block(else_block, bound, seen, output);
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr(scrutinee, bound, seen, output);
            for arm in arms {
                let mut names = Vec::new();
                pattern_binds(&arm.pattern, &mut names);
                let mut inner = bound.clone();
                inner.extend(names);
                if let Some(guard) = &arm.guard {
                    visit_expr(guard, &inner, seen, output);
                }
                visit_expr(&arm.body, &inner, seen, output);
            }
        }
        Expr::Block(block) => visit_block(block, bound, seen, output),
        Expr::While { cond, body } => {
            visit_expr(cond, bound, seen, output);
            visit_block(body, bound, seen, output);
        }
        Expr::For { var, iter, body } => {
            visit_expr(iter, bound, seen, output);
            let mut inner = bound.clone();
            inner.insert(var.clone());
            visit_block(body, &inner, seen, output);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            visit_expr(scrutinee, bound, seen, output);
            let mut names = Vec::new();
            pattern_binds(pattern, &mut names);
            let mut inner = bound.clone();
            inner.extend(names);
            visit_block(body, &inner, seen, output);
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner = bound.clone();
            inner.extend(params.iter().map(|parameter| parameter.name.clone()));
            visit_block(body, &inner, seen, output);
        }
    }
}

fn visit_all<'a>(
    expressions: &'a [Expr],
    bound: &HashSet<String>,
    seen: &mut HashSet<&'a str>,
    output: &mut Vec<&'a str>,
) {
    for expression in expressions {
        visit_expr(expression, bound, seen, output);
    }
}

fn visit_block<'a>(
    block: &'a Block,
    bound: &HashSet<String>,
    seen: &mut HashSet<&'a str>,
    output: &mut Vec<&'a str>,
) {
    let mut bound = bound.clone();
    for statement in &block.stmts {
        match statement {
            Stmt::Let { name, value, .. } => {
                visit_expr(value, &bound, seen, output);
                bound.insert(name.clone());
            }
            Stmt::LetPattern { pattern, value } => {
                visit_expr(value, &bound, seen, output);
                let mut names = Vec::new();
                pattern_binds(pattern, &mut names);
                bound.extend(names);
            }
            Stmt::Assign { name, value, .. } => {
                // A write still requires the existing binding, even when its
                // previous value is never read. Continuations must carry that
                // binding across suspension before performing the assignment.
                if !bound.contains(name) && seen.insert(name.as_str()) {
                    output.push(name);
                }
                visit_expr(value, &bound, seen, output);
            }
            Stmt::Expr(expression) | Stmt::Yield(expression) => {
                visit_expr(expression, &bound, seen, output);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    visit_expr(value, &bound, seen, output);
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
}
