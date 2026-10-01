//! Whole-module checks: float keys, `main`/build/export signatures, dynamic
//! methods, PublicState impls, target availability, grantable capabilities,
//! borrow escape, unique root owners, and region-promotion helpers.

use super::*;

/// Whether a resolved [`Ty`] has a concrete `Float` in a `Dict` KEY position or a
/// `Set` MEMBER position, at any depth (a `Dict(k, Dict(Float, v))` counts). Value
/// positions and list elements are fine — only the hashed/compared key set needs
/// `Hash`. `None` when there is no such Float.
pub(super) fn float_key_position(t: &Ty) -> Option<FloatKeyKind> {
    match t {
        Ty::Named(n, args) if **n == *"Dict" => {
            // args[0] = key, args[1] = value. A Float directly as the key is the
            // reject; otherwise recurse into BOTH (a nested dict/set anywhere).
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::DictKey);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(n, args) if **n == *"Set" => {
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::SetMember);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(_, args) => args.iter().find_map(float_key_position),
        Ty::List(e) => float_key_position(e),
        Ty::Tuple(ts) => ts.iter().find_map(float_key_position),
        Ty::Fn(ps, r, _, _, _) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(float_key_position),
        _ => None,
    }
}

/// (BUG-395) The argument index of the KEY of a generic `Dict` key operation (the
/// ones that hash/compare the key), or `None` for a non-key-op. Key operations
/// require a `Hash` key; the position lets the checker read the key's type after
/// argument unification.
pub(super) fn dict_key_op_index(name: &str) -> Option<usize> {
    match name {
        "dict.insert" | "dict.update" | "dict.remove" => Some(1),
        _ => intrinsics::lookup(name).and_then(|spec| {
            spec.signature
                .trait_bounds()
                .iter()
                .find(|bound| bound.trait_name == "Hash")
                .map(|bound| bound.parameter)
        }),
    }
}

/// The first type variable appearing anywhere in `t`, if any.
pub(super) fn first_type_var(t: &Ty) -> Option<u32> {
    match t {
        Ty::Var(v) => Some(*v),
        Ty::List(e) => first_type_var(e),
        Ty::Tuple(ts) => ts.iter().find_map(first_type_var),
        Ty::Named(_, args) => args.iter().find_map(first_type_var),
        Ty::Fn(ps, r, _, _, _) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(first_type_var),
        _ => None,
    }
}

pub(super) fn float_key_reject_message(kind: FloatKeyKind) -> String {
    match kind {
        FloatKeyKind::DictKey =>
            "`Float` is not a valid `Dict` key — keys require `Hash: Eq`, but `Float` is \
             only `PartialEq` (`NaN != NaN`, so a Float key can be unretrievable and \
             `0.1 + 0.2` is a precision trap). Use an `Int` key (scale to a fixed \
             precision) or a `String` rendering of the value"
                .to_string(),
        FloatKeyKind::SetMember =>
            "`Float` is not a valid `Set` member — members require `Hash: Eq`, but `Float` \
             is only `PartialEq` (`NaN != NaN`). Use an `Int` (scaled) or a `String` \
             rendering"
                .to_string(),
    }
}

/// The teaching error for an `==`/`!=` on an un-comparable type. `ty` is the whole
/// operand type so the message can point at the container when the offender nests.
pub(super) fn equality_reject_message(kind: Uncomparable, ty: &Ty) -> String {
    // The offender is nested when the whole operand type is a container (a List,
    // Tuple, or a generic like `Option`/`Result`) rather than the fn/capability
    // itself. `SecretStore` is the one capability spelled as a `Named` type.
    let nested = matches!(ty, Ty::List(_) | Ty::Tuple(_))
        || matches!(ty, Ty::Named(n, _) if **n != *"SecretStore");
    let where_ = if nested { format!(" (found nested in `{ty}`)") } else { String::new() };
    match kind {
        Uncomparable::Function => format!(
            "`==` is not defined on function types{where_} — there is no meaningful \
             equality for functions (identity is not stable across compilation). \
             Compare the values functions *produce*, not the functions"
        ),
        Uncomparable::Capability => format!(
            "`==` is not defined on capability types{where_} — capabilities are \
             authority, not data; there is no meaningful equality between two \
             authorities"
        ),
        Uncomparable::Existential => format!(
            "`==` is not defined on existential type `{ty}` — `dyn` values expose \
             only their declared trait methods; payload address and witness identity \
             are not observable. Declare an existential-safe comparison method with \
             explicit domain semantics"
        ),
    }
}

/// Whether `t` is `List(String)` — the command-line-arguments parameter `main`
/// may declare.
pub fn is_args_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if n == "List"
            && matches!(args.as_slice(), [ast::Type::Named(s, inner)] if s == "String" && inner.is_empty()))
}

/// Validate `main`'s signature: every parameter must be a host capability or the
/// `List(String)` args parameter, since `main` is the program's root entrypoint and
/// only the host's granted authority can enter there. Catches at check time what
/// would otherwise be a runtime error when the root capabilities are minted. A
/// module without `main` is a library and passes.
pub(super) fn check_main_signature(module: &Module) -> Result<(), TypeError> {
    let Some(main) = module.items.iter().find_map(|it| match it {
        Item::Function(f) if f.name == "main" => Some(f),
        _ => None,
    }) else {
        return Ok(());
    };
    // RFC-0038: a library-defined capability marked `grantable` may also enter at
    // the root (the host mints it from a `[user_caps]` grant). Bareness is enforced
    // separately by `check_grantable_caps`, so here we only recognize the name.
    let grantable: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_grantable_cap = |t: &ast::Type| {
        matches!(t, ast::Type::Named(n, _) if grantable.contains(n.as_str()))
    };
    for p in &main.params {
        if matches!(&p.ty, Some(t) if is_capability_type(t) || is_args_type(t) || is_grantable_cap(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`main` parameter `{}` {found}, but `main` may only take host capabilities \
             (Console, Clock, Env, Dir, Net, Fetch, Exec, Secret, SecretStore), a `grantable` \
             library capability, or `List(String)` for command-line args",
            p.name
        ));
    }
    // The runtime's value sink surfaces a plain `main` return — an `Int` becomes
    // the process exit code, a `Float`/`String`/… is printed — but it does NOT
    // surface a `Result` or `Option`: their wrapper is dropped, so an `Err`/`None`
    // is silently swallowed (no message, exit stays 0). That makes the Rust
    // `fn main() -> Result(...)` habit a quiet-failure trap. Reject those two
    // specifically and point at handling the outcome in `main`.
    if let Some(ast::Type::Named(n, _)) = &main.ret {
        if n == "Result" || n == "Option" {
            return terr(format!(
                "`main` returns `{}`, but a `Result`/`Option` returned from `main` is \
                 silently discarded — its `Err`/`None` never surfaces and the exit code \
                 stays 0. Handle the outcome inside `main` and return an exit code (or \
                 print it), e.g. `match r: Ok(_) -> 0; Err(e) -> ... ; 1`.",
                witchy_syntax::format::type_str(main.ret.as_ref().unwrap())
            ));
        }
    }
    // (BUG-335, spec §16.3) `main` may return only `()` (the default) or `Int` (the
    // process exit code); `Float` is also surfaced (both backends print it). Any
    // OTHER return type — `String`/`Bool`/`List`/a record — is a parity trap: the
    // interpreter echoes it when the program printed nothing, but the compiled run
    // wrapper wires only `print_int`/`print_float` and silently drops the rest. Reject
    // it at check time so the backends agree by construction (fail loud, never a
    // silently different answer).
    if let Some(t) = &main.ret {
        let allowed = match t.unqualified() {
            ast::Type::Tuple(items) => items.is_empty(),
            ast::Type::Named(n, _) => n == "Nil" || n == "Int" || n == "Float",
            _ => false,
        };
        if !allowed {
            return terr(format!(
                "`main` returns `{}`, but `main` may return only `()` (the default) or \
                 `Int` (the process exit code) — a `String`/`Bool`/`List`/record result is \
                 printed by the interpreter but dropped by the compiled backend, so the two \
                 diverge. Print the value inside `main` and return `()` or an exit code.",
                witchy_syntax::format::type_str(t)
            ));
        }
    }
    Ok(())
}

pub(super) fn check_dynamic_method_declarations(module: &Module) -> Result<(), TypeError> {
    let mut registered = HashSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !function.attributes.iter().any(|attribute| attribute == "dynamic") {
            continue;
        }
        if !function.public {
            return terr(format!("@dynamic function `{}` must be public", function.name));
        }
        if function.comptime_only || function.is_async || function.is_gen {
            return terr(format!(
                "@dynamic function `{}` must be an ordinary runtime function",
                function.name
            ));
        }
        let signature_types = function
            .params
            .iter()
            .filter_map(|parameter| parameter.ty.as_ref())
            .chain(function.ret.iter());
        if !function.bounds.is_empty() || !type_param_names(signature_types).is_empty() {
            return terr(format!(
                "@dynamic function `{}` must have a closed non-generic signature",
                function.name
            ));
        }
        let Some(receiver) = function.params.first() else {
            return terr(format!(
                "@dynamic function `{}` requires `self` as its first parameter",
                function.name
            ));
        };
        if receiver.name != "self" || receiver.ty.is_none() {
            return terr(format!(
                "@dynamic function `{}` requires an explicitly typed `self` first parameter",
                function.name
            ));
        }
        for parameter in &function.params {
            if parameter.ty.is_none() {
                return terr(format!(
                    "@dynamic function `{}` parameter `{}` requires an explicit type",
                    function.name, parameter.name
                ));
            }
            if parameter.default.is_some() {
                return terr(format!(
                    "@dynamic function `{}` cannot use default parameters because runtime arity is exact",
                    function.name
                ));
            }
            if parameter.convention != ast::Convention::Let {
                return terr(format!(
                    "@dynamic function `{}` parameter `{}` must use the `let` convention",
                    function.name, parameter.name
                ));
            }
        }
        let receiver = receiver.ty.as_ref().expect("checked above");
        let method = function
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&function.name);
        let key = (
            witchy_syntax::format::type_str(receiver),
            method.to_string(),
        );
        if !registered.insert(key.clone()) {
            return terr(format!(
                "duplicate @dynamic method `{}` for `{}`",
                key.1, key.0
            ));
        }
    }
    Ok(())
}

pub(super) fn canonical_public_state_std_impl(implementation: &ast::ImplDef) -> bool {
    let scalar = matches!(
        implementation.type_name.as_str(),
        "Tuple0" | "Bool" | "Int" | "Float" | "Duration" | "String"
    ) && implementation.target_args.is_empty();
    if scalar {
        return true;
    }
    let expected_params: &[&str] = match implementation.type_name.as_str() {
        "List" | "Option" => &["a"],
        "Result" => &["a", "e"],
        _ => return false,
    };
    let actual_params = implementation
        .target_args
        .iter()
        .map(|argument| match argument.unqualified() {
            ast::Type::Named(name, arguments)
                if arguments.is_empty() && name.chars().next().is_some_and(char::is_lowercase) =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    if actual_params.as_deref() != Some(expected_params) {
        return false;
    }
    expected_params.iter().all(|parameter| {
        implementation.bounds.iter().any(|(variable, trait_name, arguments)| {
            variable == parameter
                && arguments.is_empty()
                && crate::traits::is_standard_trait_identity(
                    trait_name,
                    "public_state",
                    "PublicState",
                )
        })
    })
}

/// `PublicState` is a sealed boundary proof. Ordinary user-defined traits remain
/// open, but this one may be implemented only by its canonical std foundations
/// or by the compiler-authenticated `derive(PublicState)` generator. Without
/// this check, a handwritten or user-generated empty proof could launder Bytes,
/// capabilities, functions, secrets, or host handles through a wrapper.
pub(super) fn check_public_state_impls(module: &Module) -> Result<(), TypeError> {
    let derived_types: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) if definition.public_state_derived => {
                Some(definition.name.as_str())
            }
            _ => None,
        })
        .collect();
    for item in &module.items {
        let Item::Impl(implementation) = item else { continue };
        let Some(trait_name) = implementation.trait_name.as_deref() else { continue };
        if trait_name != "public_state.PublicState" {
            continue;
        }
        let canonical_std = implementation.origin == ast::ImplOrigin::Source
            && canonical_public_state_std_impl(implementation);
        let authenticated_derive = derived_types.contains(implementation.type_name.as_str());
        if !canonical_std && !authenticated_derive {
            return terr(format!(
                "`PublicState` is a sealed public-boundary proof; type `{}` must use \
                 `derive(PublicState)`, whose generated proof checks every field",
                implementation.type_name,
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetAvailability {
    Shared,
    Browser,
    Server,
    Static,
}

impl TargetAvailability {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Browser => "browser",
            Self::Server => "server",
            Self::Static => "static",
        }
    }

    pub(super) fn permits(self, callee: Self) -> bool {
        callee == Self::Shared || self == callee
    }
}

pub(super) fn function_target(function: &Function) -> Result<TargetAvailability, TypeError> {
    let targets: Vec<&str> = function
        .attributes
        .iter()
        .map(String::as_str)
        .filter(|attribute| matches!(*attribute, "browser" | "server" | "static"))
        .collect();
    match targets.as_slice() {
        [] => Ok(TargetAvailability::Shared),
        ["browser"] => Ok(TargetAvailability::Browser),
        ["server"] => Ok(TargetAvailability::Server),
        ["static"] => Ok(TargetAvailability::Static),
        _ => terr(format!(
            "function `{}` declares conflicting target availability: {}",
            function.name,
            targets.iter().map(|target| format!("@{target}")).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// RFC-0107 target availability is a composition proof, not conditional
/// compilation. Unannotated functions are shared and can reference only shared
/// code. A specialized function can additionally reference code for its own
/// target. Direct calls and first-class function references are both checked.
pub(super) fn check_target_availability(module: &Module) -> Result<(), TypeError> {
    let mut targets = HashMap::new();
    for item in &module.items {
        if let Item::Function(function) = item {
            targets.insert(function.name.as_str(), function_target(function)?);
        }
    }
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        let caller_target = function_target(function)?;
        let mut violation: Option<(String, TargetAvailability)> = None;
        crate::loans::walk_block(&function.body, &mut |expression| {
            if violation.is_some() {
                return;
            }
            let referenced = match expression {
                Expr::Call { name, .. } | Expr::LabeledCall { name, .. } => Some(name.as_str()),
                Expr::Var(name) => Some(name.as_str()),
                _ => None,
            };
            let Some(name) = referenced else { return };
            let Some(&callee_target) = targets.get(name) else { return };
            if !caller_target.permits(callee_target) {
                violation = Some((name.to_string(), callee_target));
            }
        });
        if let Some((callee, callee_target)) = violation {
            return terr(format!(
                "{} function `{}` references {}-only function `{callee}`; \
                 move the reference into @{} code or make the callee shared",
                caller_target.label(),
                function.name,
                callee_target.label(),
                callee_target.label(),
            ));
        }
    }
    Ok(())
}

/// RFC-0038: a `grantable capability` may be granted to a root entrypoint, so it
/// must be **bare** — carrying zero transitive built-in host authority. Otherwise
/// granting it would be an invisible `Net`/`Dir`/`Secret` grant (and a later
/// version adding a host-cap field would silently widen root authority with no
/// signature change). Reject any grantable cap that reaches a host capability
/// through its fields, directly or through nested user types.
pub(super) fn check_grantable_caps(module: &Module) -> Result<(), TypeError> {
    let types: HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for it in &module.items {
        let Item::Type(t) = it else { continue };
        if !t.grantable {
            continue;
        }
        let mut seen = HashSet::new();
        if let Some(host) = grantable_host_taint(t, &types, &mut seen) {
            return terr(format!(
                "`grantable capability {}` carries host capability `{host}`; a \
                 root-grantable capability must be BARE (zero transitive host \
                 authority), so it cannot disguise a built-in capability. Construct \
                 it inside the program from an explicit `{host}` root instead.",
                t.name
            ));
        }
    }
    Ok(())
}

/// The first built-in host capability reachable from a type def's fields
/// (transitively through user-type fields), or `None` if the type is bare.
pub(super) fn grantable_host_taint<'a>(
    t: &'a ast::TypeDef,
    types: &HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut HashSet<&'a str>,
) -> Option<String> {
    if !seen.insert(t.name.as_str()) {
        return None; // cycle guard
    }
    for v in &t.variants {
        for fty in &v.fields {
            if let Some(h) = type_host_taint(fty, types, seen) {
                return Some(h);
            }
        }
    }
    None
}

pub(super) fn type_host_taint<'a>(
    ty: &'a ast::Type,
    types: &HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut HashSet<&'a str>,
) -> Option<String> {
    match ty {
        ast::Type::Named(n, args) => {
            if is_capability_type(ty) {
                return Some(n.clone());
            }
            if let Some(inner) = types.get(n.as_str()) {
                if let Some(h) = grantable_host_taint(inner, types, seen) {
                    return Some(h);
                }
            }
            args.iter().find_map(|a| type_host_taint(a, types, seen))
        }
        ast::Type::Qualified(_, inner) => type_host_taint(inner, types, seen),
        ast::Type::Slice(inner) => type_host_taint(inner, types, seen),
        // (RFC-0081) A dyn value is never itself a capability; scan args only.
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| type_host_taint(a, types, seen)),
        ast::Type::Tuple(ts) => ts.iter().find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::Fn(params, ret, _, _) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::RecordCompose { base, fields } => std::iter::once(base.as_ref())
            .chain(fields.iter().map(|(_, ty)| ty))
            .find_map(|ty| type_host_taint(ty, types, seen)),
    }
}

/// (RFC-0040) A cap-gated string export — `pub fn export_*(cap, String) -> String`,
/// a browser app root — must lead with a BARE grantable capability, since the host
/// mints it at that entry (like `main`). Guard the intended-but-wrong shape (a
/// 2-param `[Named, String] -> String` export whose leading type isn't grantable)
/// with a clear error instead of silently not exporting the function.
pub(super) fn check_export_signatures(module: &Module) -> Result<(), TypeError> {
    let grantable: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_string = |t: &Option<ast::Type>| {
        matches!(t, Some(ast::Type::Named(n, a)) if n == "String" && a.is_empty())
    };
    for it in &module.items {
        let Item::Function(f) = it else { continue };
        if !f.public {
            continue;
        }
        let unqual = f.name.rsplit('.').next().unwrap_or(&f.name);
        if !unqual.starts_with("export_") {
            continue;
        }
        if let [p0, p1] = f.params.as_slice() {
            let ret_string = matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "String");
            if is_string(&p1.ty) && ret_string {
                if let Some(ast::Type::Named(n, _)) = &p0.ty {
                    if n != "String" && !grantable.contains(n.as_str()) {
                        return terr(format!(
                            "the leading parameter of the exported entrypoint `{unqual}` is `{n}`, but a \
                             cap-gated export may lead only with a bare `grantable capability` (RFC-0040) — \
                             `{n}` is not grantable"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate a rune's build entrypoint. The build step is the top-level `fn build`
/// whose first parameter is `BuildOut`; it runs in a zero-ambient build sandbox,
/// so it may take *only* build-time capabilities — a runtime capability (or
/// anything else) in its signature is an error. A `build` function that does not
/// take `BuildOut` is treated as an ordinary function, not the entrypoint, so
/// existing code that happens to define `fn build(...)` is unaffected.
pub(super) fn check_build_signature(module: &Module) -> Result<(), TypeError> {
    let Some(build) = build_entrypoint(module) else {
        return Ok(());
    };
    for p in &build.params {
        if matches!(&p.ty, Some(t) if is_build_capability_type(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`build` parameter `{}` {found}, but a build step may only take build-time \
             capabilities (BuildOut, BuildRead, BuildEnv, BuildNet, BuildExec)",
            p.name
        ));
    }
    Ok(())
}

/// Collect the type-parameter names (lowercase, argument-less) appearing in a
/// type expression, in order of first appearance. Used to infer the parameters
/// of a generic ADT from its variant field types.
/// Whether this is an outer `unique` owner of a collection root. Nested
/// owners are permitted, but each component is proved separately by
/// `produces_root_owner` below.
pub(super) fn is_unique_root_owner_result(ty: &ast::Type) -> bool {
    matches!(
        ty,
        ast::Type::Qualified(ast::TypeQual::Unique, inner)
            if matches!(inner.unqualified(), ast::Type::Named(name, _)
                if matches!(name.as_str(), "List" | "Dict"))
    )
}

pub(super) fn is_unique_owner_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Qualified(ast::TypeQual::Unique, _))
}

/// Whether evaluating an expression can reach the statement after it.
///
/// Branch-state joins must ignore successors that have already transferred
/// control with `return`, `break`, or `continue`: facts from those paths are
/// checked at the transfer site and do not constrain a later fallthrough path.
pub(super) fn expr_can_complete(expr: &Expr) -> bool {
    match expr {
        Expr::If { then_block, else_block: Some(else_block), .. } => {
            block_can_complete(then_block) || block_can_complete(else_block)
        }
        Expr::Match { arms, .. } if !arms.is_empty() => {
            arms.iter().any(|arm| expr_can_complete(&arm.body))
        }
        Expr::Block(block) => block_can_complete(block),
        _ => true,
    }
}

pub(super) fn block_can_complete(block: &Block) -> bool {
    let mut reachable = true;
    for stmt in &block.stmts {
        if !reachable {
            break;
        }
        reachable = match stmt {
            Stmt::Return(_) | Stmt::Break | Stmt::Continue => false,
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value) => expr_can_complete(value),
        };
    }
    reachable
}

/// Join ownership facts from control-flow successors that actually reach the
/// continuation. If every successor terminates, retain one checked terminal
/// state so the enclosing scope does not resurrect discharged obligations.
pub(super) fn join_reachable_binding_facts(
    before_consumed: &HashSet<(usize, String)>,
    before_must_live: &HashSet<(usize, String)>,
    before_once_invoked: &HashSet<(usize, String)>,
    branches: &[(
        bool,
        HashSet<(usize, String)>,
        HashSet<(usize, String)>,
        HashSet<(usize, String)>,
    )],
) -> (
    HashSet<(usize, String)>,
    HashSet<(usize, String)>,
    HashSet<(usize, String)>,
) {
    let mut reaching = branches
        .iter()
        .filter(|(completes, _, _, _)| *completes)
        .peekable();
    let first = reaching.next().or_else(|| branches.first());
    let Some((_, first_consumed, first_must_live, first_once_invoked)) = first else {
        return (
            before_consumed.clone(),
            before_must_live.clone(),
            before_once_invoked.clone(),
        );
    };
    let mut consumed = first_consumed.clone();
    let mut must_live = first_must_live.clone();
    let mut once_invoked = first_once_invoked.clone();
    for (_, branch_consumed, branch_must_live, branch_once_invoked) in reaching {
        consumed = &consumed | branch_consumed;
        must_live = &must_live | branch_must_live;
        once_invoked = &once_invoked | branch_once_invoked;
    }
    (consumed, must_live, once_invoked)
}

/// A `unique List` / `unique Dict` result must carry a checked root-owner
/// token. Fresh literals/new storage and direct calls carrying the same result
/// ABI establish one.  A directly returned `own unique` collection parameter
/// carries that same token too: uniqueness is ownership of the root, not a
/// promise of spare list capacity.  Broader local-flow proofs remain outside
/// this deliberately small source proof surface.
pub(super) fn check_unique_root_owner_results(module: &Module) -> Result<(), TypeError> {
    let unique_functions: HashMap<String, ast::Type> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) if function.ret.as_ref().is_some_and(is_unique_owner_type) => {
                Some((function.name.clone(), function.ret.clone().expect("unique return exists")))
            }
            _ => None,
        })
        .collect();
    let unique_result_functions: HashMap<String, ast::Type> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function)
                if function
                    .ret
                    .as_ref()
                    .and_then(result_ok_payload_type)
                    .is_some_and(is_unique_root_owner_result) =>
            {
                Some((
                    function.name.clone(),
                    function.ret.clone().expect("Result return exists"),
                ))
            }
            _ => None,
        })
        .collect();

    /// Establishes every `unique` owner named by `expected`, recursively. A
    /// fresh list proves its outer root and then proves each independently
    /// declared unique element. A direct call/`own` transfer is accepted only
    /// when its complete declared type exactly matches `expected`.
    fn produces_root_owner(
        expr: &Expr,
        expected: &ast::Type,
        functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> bool {
        if !is_unique_owner_type(expected) {
            return true;
        }
        match expr {
            Expr::List(items) => {
                let ast::Type::Qualified(ast::TypeQual::Unique, inner) = expected else {
                    return false;
                };
                let ast::Type::Named(name, arguments) = inner.unqualified() else {
                    return false;
                };
                name.rsplit('.').next() == Some("List")
                    && arguments.len() == 1
                    && items.iter().all(|item| {
                        produces_root_owner(
                            item,
                            &arguments[0],
                            functions,
                            transferred_parameters,
                        )
                    })
            }
            Expr::Str(_) => matches!(
                expected,
                ast::Type::Qualified(ast::TypeQual::Unique, inner)
                    if matches!(inner.unqualified(), ast::Type::Named(name, _)
                        if name.rsplit('.').next() == Some("String"))
            ),
            // This is deliberately only a direct parameter name from the
            // enclosing function body.  Aliases, projections, aggregate
            // fields, and nested returns remain unproved rather than silently
            // inheriting the root-owner token.
            Expr::Var(name) => transferred_parameters.get(name) == Some(expected),
            Expr::Call { name, args } => {
                functions.get(name) == Some(expected)
                    || (args.is_empty()
                        && matches!(
                            expected,
                            ast::Type::Qualified(ast::TypeQual::Unique, inner)
                                if matches!(inner.unqualified(), ast::Type::Named(name, _)
                                    if name.rsplit('.').next() == Some("Dict"))
                        )
                        && (name == intrinsics::DICT_NEW
                            || name
                                .strip_prefix(intrinsics::DICT_NEW)
                                .is_some_and(|suffix| suffix.starts_with("__"))
                            || name.ends_with(".dict.new")))
                    || (args.len() == 1
                        && matches!(
                            expected,
                            ast::Type::Qualified(ast::TypeQual::Unique, inner)
                                if matches!(inner.unqualified(), ast::Type::Named(name, _)
                                    if name.rsplit('.').next() == Some("Dict"))
                        )
                        && (name == intrinsics::DICT_WITH_CAPACITY
                            || name
                                .strip_prefix(intrinsics::DICT_WITH_CAPACITY)
                                .is_some_and(|suffix| suffix.starts_with("__"))
                            || name.ends_with(".dict.with_capacity")))
            }
            Expr::Unary { op: UnOp::Move, expr } => {
                produces_root_owner(expr, expected, functions, transferred_parameters)
            }
            _ => false,
        }
    }

    #[derive(Clone, Copy)]
    enum UniqueReturnContract<'a> {
        Direct(&'a ast::Type),
        ResultOk {
            result: &'a ast::Type,
            payload: &'a ast::Type,
        },
    }

    fn return_expr_produces_root_owner(
        expr: &Expr,
        contract: UniqueReturnContract<'_>,
        functions: &HashMap<String, ast::Type>,
        result_functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> bool {
        match contract {
            UniqueReturnContract::Direct(expected) => {
                produces_root_owner(expr, expected, functions, transferred_parameters)
            }
            UniqueReturnContract::ResultOk { result, payload } => match expr {
                Expr::Call { name, .. } => result_functions.get(name) == Some(result),
                Expr::Ctor { name, args }
                    if name.rsplit('.').next() == Some("Ok") && args.len() == 1 =>
                {
                    produces_root_owner(&args[0], payload, functions, transferred_parameters)
                }
                // `Err` has no success payload and therefore no `Ok` owner
                // root to prove. Its payload remains subject to ordinary type
                // checking.
                Expr::Ctor { name, args }
                    if name.rsplit('.').next() == Some("Err") && args.len() == 1 =>
                {
                    true
                }
                _ => false,
            },
        }
    }

    fn block_produces_root_owner(
        block: &Block,
        contract: UniqueReturnContract<'_>,
        functions: &HashMap<String, ast::Type>,
        result_functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> bool {
        block_can_complete(block)
            && matches!(block.stmts.last(), Some(Stmt::Expr(expr))
                if expr_produces_root_owner(expr, contract, functions, result_functions, transferred_parameters))
    }

    /// A control-flow expression preserves the root-owner proof only when
    /// every path which reaches its continuation produces the same direct
    /// owner. Terminal paths have already been checked at their `return`.
    fn expr_produces_root_owner(
        expr: &Expr,
        contract: UniqueReturnContract<'_>,
        functions: &HashMap<String, ast::Type>,
        result_functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> bool {
        match expr {
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                (!block_can_complete(then_block)
                    || block_produces_root_owner(
                        then_block,
                        contract,
                        functions,
                        result_functions,
                        transferred_parameters,
                    ))
                    && (!block_can_complete(else_block)
                        || block_produces_root_owner(
                            else_block,
                            contract,
                            functions,
                            result_functions,
                            transferred_parameters,
                        ))
            }
            Expr::Match { arms, .. } if !arms.is_empty() => arms.iter().all(|arm| {
                !expr_can_complete(&arm.body)
                    || expr_produces_root_owner(
                        &arm.body,
                        contract,
                        functions,
                        result_functions,
                        transferred_parameters,
                    )
            }),
            Expr::Block(block) => {
                !block_can_complete(block)
                    || block_produces_root_owner(
                        block,
                        contract,
                        functions,
                        result_functions,
                        transferred_parameters,
                    )
            }
            _ => return_expr_produces_root_owner(
                expr,
                contract,
                functions,
                result_functions,
                transferred_parameters,
            ),
        }
    }

    fn check_returns_expr(
        expr: &Expr,
        function: &str,
        contract: UniqueReturnContract<'_>,
        functions: &HashMap<String, ast::Type>,
        result_functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> Result<(), TypeError> {
        match expr {
            Expr::If { cond, then_block, else_block } => {
                check_returns_expr(cond, function, contract, functions, result_functions, transferred_parameters)?;
                check_returns_block(then_block, function, contract, functions, result_functions, transferred_parameters)?;
                if let Some(block) = else_block {
                    check_returns_block(block, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::Match { scrutinee, arms } => {
                check_returns_expr(scrutinee, function, contract, functions, result_functions, transferred_parameters)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        check_returns_expr(guard, function, contract, functions, result_functions, transferred_parameters)?;
                    }
                    check_returns_expr(&arm.body, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::Block(block) => {
                check_returns_block(block, function, contract, functions, result_functions, transferred_parameters)?
            }
            Expr::While { cond, body } => {
                check_returns_expr(cond, function, contract, functions, result_functions, transferred_parameters)?;
                check_returns_block(body, function, contract, functions, result_functions, transferred_parameters)?;
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                check_returns_expr(scrutinee, function, contract, functions, result_functions, transferred_parameters)?;
                check_returns_block(body, function, contract, functions, result_functions, transferred_parameters)?;
            }
            Expr::For { iter, body, .. } => {
                check_returns_expr(iter, function, contract, functions, result_functions, transferred_parameters)?;
                check_returns_block(body, function, contract, functions, result_functions, transferred_parameters)?;
            }
            Expr::Call { args, .. }
            | Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for arg in args {
                    check_returns_expr(arg, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, arg) in args {
                    check_returns_expr(arg, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                check_returns_expr(receiver, function, contract, functions, result_functions, transferred_parameters)?;
                for arg in args {
                    check_returns_expr(arg, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                check_returns_expr(receiver, function, contract, functions, result_functions, transferred_parameters)?;
                for arg in args {
                    check_returns_expr(arg, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::Apply { func, args } => {
                check_returns_expr(func, function, contract, functions, result_functions, transferred_parameters)?;
                for arg in args {
                    check_returns_expr(arg, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => {
                check_returns_expr(expr, function, contract, functions, result_functions, transferred_parameters)?
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range { lo: lhs, hi: rhs, .. }
            | Expr::Index { base: lhs, index: rhs } => {
                check_returns_expr(lhs, function, contract, functions, result_functions, transferred_parameters)?;
                check_returns_expr(rhs, function, contract, functions, result_functions, transferred_parameters)?;
            }
            Expr::RecordUpdate { base, fields, .. } => {
                check_returns_expr(base, function, contract, functions, result_functions, transferred_parameters)?;
                for (_, value) in fields {
                    check_returns_expr(value, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, value) in fields {
                    check_returns_expr(value, function, contract, functions, result_functions, transferred_parameters)?;
                }
                if let Some(spread) = spread {
                    check_returns_expr(spread, function, contract, functions, result_functions, transferred_parameters)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn check_returns_block(
        block: &Block,
        function: &str,
        contract: UniqueReturnContract<'_>,
        functions: &HashMap<String, ast::Type>,
        result_functions: &HashMap<String, ast::Type>,
        transferred_parameters: &HashMap<String, ast::Type>,
    ) -> Result<(), TypeError> {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Return(Some(expr))
                    if !expr_produces_root_owner(
                        expr,
                        contract,
                        functions,
                        result_functions,
                        transferred_parameters,
                    ) =>
                {
                    return terr(format!(
                        "in `{function}`: this `unique` collection return has no root-owner proof — return fresh list/dict storage, the direct result of another `unique` collection function, or this function's direct `own unique` parameter"
                    ));
                }
                Stmt::Return(Some(expr))
                | Stmt::Let { value: expr, .. }
                | Stmt::Assign { value: expr, .. }
                | Stmt::LetPattern { value: expr, .. }
                | Stmt::Expr(expr)
                | Stmt::Yield(expr) => {
                    check_returns_expr(expr, function, contract, functions, result_functions, transferred_parameters)?
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }

    /// `Result` carries its success value in `Ok`.  Its runtime `Ty` erases
    /// source qualifiers, so a `unique` root nested there must receive the
    /// same source proof as an outer result before constructor inference can
    /// hide a `var` write-back root inside `Ok`.
    fn result_ok_payload_type(result: &ast::Type) -> Option<&ast::Type> {
        let ast::Type::Named(name, arguments) = result.unqualified() else {
            return None;
        };
        (name.rsplit('.').next() == Some("Result") && arguments.len() == 2)
            .then(|| &arguments[0])
    }

    for item in &module.items {
        let Item::Function(function) = item else { continue };
        let Some(result) = function.ret.as_ref() else {
            continue;
        };
        let transferred_parameters = function.params.iter().filter_map(|parameter| {
            (parameter.convention == Convention::Own
                && parameter.ty.as_ref().is_some_and(is_unique_owner_type))
                .then(|| (parameter.name.clone(), parameter.ty.clone().expect("own unique type exists")))
        }).collect::<HashMap<_, _>>();
        let contract = if is_unique_root_owner_result(result) {
            UniqueReturnContract::Direct(result)
        } else if let Some(payload) = result_ok_payload_type(result)
            && is_unique_root_owner_result(payload)
        {
            UniqueReturnContract::ResultOk { result, payload }
        } else {
            continue;
        };
        check_returns_block(
            &function.body,
            &function.name,
            contract,
            &unique_functions,
            &unique_result_functions,
            &transferred_parameters,
        )?;
        if block_can_complete(&function.body)
            && !block_produces_root_owner(
                &function.body,
                contract,
                &unique_functions,
                &unique_result_functions,
                &transferred_parameters,
            )
        {
            return terr(format!(
                "in `{}`: the tail of a `unique` collection return has no root-owner proof — return fresh list/dict storage, the direct result of another `unique` collection function, or this function's direct `own unique` parameter",
                function.name
            ));
        }
    }
    Ok(())
}

/// Types a region assignment may freely write through to the outer scope:
/// copied by value, never pointer-backed.
pub(super) fn is_scalar_ty(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Duration)
}

/// The source layouts the initial canonical inline ABI can authenticate.  Do
/// not broaden this to arbitrary `packed` declarations: nested packed fields,
/// generics, and sums need their own descriptor/carrier contracts.
pub(super) fn region_packed_layout_descriptors(module: &Module) -> HashMap<String, RegionPackedLayoutDescriptor> {
    let mut descriptors = HashMap::new();
    for item in &module.items {
        let Item::Type(definition) = item else { continue };
        let Some(variant) = definition.variants.first() else { continue };
        if !(definition.packed
            && definition.params.is_empty()
            && definition.variants.len() == 1
            && variant.fields.iter().all(|field| matches!(field.unqualified(),
                ast::Type::Named(name, arguments)
                    if arguments.is_empty()
                        && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit")
            ))) {
            continue;
        }
        let fields = variant.fields.clone();
        descriptors.insert(definition.name.clone(), RegionPackedLayoutDescriptor {
            record: definition.name.clone(), fields: fields.clone(),
        });
        // The linked entry's own definitions have one known canonical
        // qualification. Do not manufacture aliases for imported modules:
        // an imported packed declaration needs its owner-qualified carrier.
        if let Some(entry) = module.linked_entry.as_deref()
            && !definition.name.contains('.')
        {
            let record = format!("{entry}.{}", definition.name);
            descriptors.insert(record.clone(), RegionPackedLayoutDescriptor { record, fields });
        }
    }
    descriptors
}

pub(super) fn region_value_tail(block: &Block) -> Option<&Expr> {
    match block.stmts.last()? {
        Stmt::Expr(value) | Stmt::Return(Some(value)) => Some(value),
        _ => None,
    }
}

pub(super) fn region_declared_local_names(block: &Block) -> HashSet<String> {
    block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Let { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Recover the checked source-place root of a direct reference handle.  A
/// region-local *handle* may leave when it borrows a parent root (`let h =
/// &parent; h`); a region-local owner may not.  We deliberately accept only
/// direct borrow/alias/ascription chains until the retained loan graph exports
/// a richer projection witness.
/// Whether a value of this type may hold a callable or a task recipe, either
/// of which can capture a view without exposing it in the type. A nominal
/// type is searched through its fields and variant payloads, so an `Iter`
/// (a generator frame behind a thunk) counts as a callable.
pub(super) fn ty_carries_callable_or_task(ty: &Ty, checker: &Checker) -> bool {
    fn go(ty: &Ty, checker: &Checker, seen: &mut HashSet<String>) -> bool {
        match checker.resolve(ty) {
            Ty::Fn(..) => true,
            Ty::Named(name, arguments) => {
                if name.as_ref() == "Task"
                    || name.rsplit('.').next() == Some("Task")
                    || arguments.iter().any(|argument| go(argument, checker, seen))
                {
                    return true;
                }
                if !seen.insert(name.to_string()) {
                    return false;
                }
                let field_hit = checker.record_fields.get(name.as_ref()).is_some_and(|(_, fields)| {
                    fields.iter().any(|(_, field)| go(field, checker, seen))
                });
                let variant_hit = checker.adt_variants.get(name.as_ref()).is_some_and(|variants| {
                    variants.iter().any(|variant| checker.ctor_sigs.get(variant).is_some_and(
                        |(payloads, _)| payloads.iter().any(|payload| go(payload, checker, seen)),
                    ))
                });
                seen.remove(name.as_ref());
                field_hit || variant_hit
            }
            Ty::List(item) => go(&item, checker, seen),
            Ty::Tuple(items) => items.iter().any(|item| go(item, checker, seen)),
            _ => false,
        }
    }
    go(ty, checker, &mut HashSet::new())
}

/// Whether a region body forms a view (`&place`) rooted in one of the
/// region's own locals anywhere, including inside a lambda or a call argument.
pub(super) fn block_forms_view_of_local(block: &Block, locals: &HashSet<String>) -> bool {
    fn root(expression: &Expr) -> Option<&str> {
        match expression {
            Expr::Var(name) => Some(name),
            // A view formed through a view (`&(*b).f`) roots in `b`.
            Expr::Field { base, .. } | Expr::Index { base, .. }
            | Expr::Unary { op: UnOp::Deref, expr: base } => root(base),
            _ => None,
        }
    }
    let mut found = false;
    for statement in &block.stmts {
        crate::loans::walk_stmt_exprs(statement, &mut |expression| {
            if let Expr::Unary { op: UnOp::Borrow, expr } = expression
                && root(expr).is_some_and(|root| locals.contains(root))
            {
                found = true;
            }
        });
    }
    found
}

/// SEC-096 per-field parent-root provenance for a region result aggregate.
/// A record, list, tuple or constructor may leave `region:` carrying views
/// only when every view it can hold is proven parent-rooted: formed directly
/// as `&place` over a root declared outside the region, read from a binding
/// declared outside the region, or read from a region-local immutable `let`
/// whose own value carries the same proof. Any other leaf (a call result, a
/// computed value, a pattern binding) has unknown provenance and refuses.
/// Parent bindings are stable here: assigning or `var`-writing back a
/// view-bearing parent binding inside a region is refused separately.
pub(super) fn region_aggregate_views_parent_rooted(block: &Block, locals: &HashSet<String>) -> bool {
    fn place_root(expression: &Expr) -> Option<&str> {
        match expression {
            Expr::Var(name) => Some(name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
            _ => None,
        }
    }
    fn proven(expression: &Expr, locals: &HashSet<String>, proven_locals: &HashSet<String>) -> bool {
        match expression {
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => true,
            Expr::Var(name) => !locals.contains(name) || proven_locals.contains(name),
            Expr::Unary { op: UnOp::Borrow, expr } => {
                place_root(expr).is_some_and(|root| !locals.contains(root))
            }
            Expr::List(items) | Expr::Tuple(items) => {
                items.iter().all(|item| proven(item, locals, proven_locals))
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                args.iter().all(|argument| proven(argument, locals, proven_locals))
            }
            _ => false,
        }
    }
    let Some((tail, leading)) = block.stmts.split_last() else { return false };
    let mut proven_locals = HashSet::new();
    for statement in leading {
        match statement {
            Stmt::Let { name, mutable: false, value, .. } if proven(value, locals, &proven_locals) => {
                proven_locals.insert(name.clone());
            }
            Stmt::Let { name, .. } => {
                proven_locals.remove(name);
            }
            // A destructuring binding may shadow a proven name; do not model it.
            Stmt::LetPattern { .. } => return false,
            _ => {}
        }
    }
    match tail {
        Stmt::Expr(value) | Stmt::Return(Some(value)) => proven(value, locals, &proven_locals),
        _ => false,
    }
}

pub(super) fn region_reference_root(block: &Block) -> Option<String> {
    fn root(expression: &Expr, aliases: &HashMap<String, String>) -> Option<String> {
        match expression {
            Expr::Var(name) => aliases.get(name).cloned().or_else(|| Some(name.clone())),
            Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref, expr } => root(expr, aliases),
            Expr::Field { base, .. } | Expr::Index { base, .. } => root(base, aliases),
            Expr::As { expr, .. } => root(expr, aliases),
            _ => None,
        }
    }

    let mut aliases = HashMap::new();
    let (tail, leading) = block.stmts.split_last()?;
    for statement in leading {
        match statement {
            // Only immutable bindings preserve their initial root until the
            // region exits. A mutable alias may be rebound to a region-born
            // owner after this statement.
            Stmt::Let { name, mutable: false, value, .. } => {
                let owner_root = root(value, &aliases).filter(|_| {
                    matches!(value, Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, .. }
                        | Expr::Var(_) | Expr::As { .. })
                });
                aliases.remove(name);
                if let Some(owner_root) = owner_root {
                    aliases.insert(name.clone(), owner_root);
                }
            }
            Stmt::Let { name, .. } | Stmt::Assign { name, .. } => {
                aliases.remove(name);
            }
            // A pattern may shadow an alias; without a binding census its
            // provenance cannot be carried through the rest of the block.
            Stmt::LetPattern { .. } => aliases.clear(),
            _ => {}
        }
    }
    match tail {
        Stmt::Expr(value) | Stmt::Return(Some(value)) => root(value, &aliases),
        _ => None,
    }
}

pub(super) fn collect_trait_method_names(module: &Module) -> HashSet<String> {
    let mut names = HashSet::new();
    for item in &module.items {
        if let Item::Trait(tr) = item {
            for method in &tr.methods {
                let bare = method.name.rsplit('.').next().unwrap_or(&method.name);
                names.insert(bare.to_string());
            }
        }
    }
    names
}
