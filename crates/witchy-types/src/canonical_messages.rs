//! Checked, closed-world task messages. Erasure changes representation, never
//! payload identity: recovery is an ordinary exhaustive tagged-sum match.
use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Block, Expr, Item, MatchArm, Pattern, Stmt, Type, TypeDef, TypeQual, Variant};
use witchy_syntax::ast::visit::{visit_block, visit_block_mut, visit_expr_mut};
use witchy_syntax::intrinsics::{self, IntrinsicId};

use crate::runtime_type::{RuntimeDeclarationCatalog, RuntimeTypeIdentity};
use crate::typeck::{self, Ty, TypedModule};

const PAYLOAD: &str = "$canonical.MessagePayload";
const STAMP: &str = "$canonical.MessageSite_";

fn variant(index: usize) -> String { format!("{PAYLOAD}_{index}") }

/// Only source-bearing checked linking calls this. All old expression facts
/// are consumed before structural mutation; the caller derives final witnesses
/// from the fallibly rechecked result, retaining source bytes and loader owners.
pub(crate) fn prepare(
    typed: TypedModule,
    catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<TypedModule, String> {
    let scope = crate::pipeline::canonical_scope(typed.module());
    // Match the existing callable witness's runtime inventory exactly. Entry
    // libraries retain generic templates in canonical_scope, but templates
    // are not executable bodies and cannot define a concrete message tag.
    // Every retained body still undergoes conversion and fallible rechecking;
    // an unstamped erasure in one of those bodies continues to reject.
    let runtime_functions = typed.module().items.iter().filter_map(|item| match item {
        Item::Function(function) if scope.contains(&function.name)
            && !crate::pipeline::function_has_free_signature_variables(function) => Some(function.name.clone()),
        _ => None,
    }).collect::<std::collections::BTreeSet<_>>();
    // Rechecking a rewritten root still needs every exact declaration its body
    // calls, including private generic std helpers. These are typechecking
    // dependencies only: `runtime_functions` remains the concrete canonical
    // execution inventory used for payload discovery and downstream ABI facts.
    let recheck_functions = recheck_function_dependencies(typed.module(), &runtime_functions);
    let mut payloads = BTreeMap::<RuntimeTypeIdentity, Type>::new();
    let mut sites = BTreeMap::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !runtime_functions.contains(&function.name) { continue; }
        visit_block(&function.body, &mut |expr| {
            let Expr::Call { name, args } = expr else { return Ok::<_, String>(()) };
            let Some(spec) = intrinsics::lookup(name) else { return Ok(()) };
            let pack = match spec.id {
                IntrinsicId::Erase => true,
                IntrinsicId::Unerase => false,
                _ => return Ok(()),
            };
            let [argument] = args.as_slice() else {
                return Err("canonical message bridge requires exactly one checked argument".into());
            };
            let message = if pack { expr } else { argument };
            if !matches!(typed.table().type_of(message), Some(Ty::Msg)) {
                return Err("canonical message bridge lost its checked opaque message type".into());
            }
            let payload = if pack { argument } else { expr };
            let ty = typed.table().type_of(payload).and_then(typeck::ty_to_ast)
                .ok_or_else(|| format!("canonical message payload in `{}` has no concrete checked type", function.name))?;
            reject_unowned(&ty)?;
            let catalog = catalog.ok_or("canonical messages require retained authenticated loader ownership")?;
            let identity = catalog.type_identity(&ty)
                .map_err(|error| format!("canonical message payload {ty:?} is not yet supported: {error}"))?;
            if let Some(previous) = payloads.insert(identity.clone(), ty.clone()) {
                if previous != ty {
                    return Err(format!("canonical message payload identities have inconsistent checked types: {previous:?} and {ty:?}"));
                }
            }
            sites.insert(expr as *const Expr as usize, (pack, identity));
            Ok(())
        })?;
    }
    if sites.is_empty() { return Ok(typed); }
    // Generated names are source-unspellable; also reject collisions in any
    // compiler input rather than treating a matching name as authority.
    for item in &typed.module().items {
        match item {
            Item::Type(definition) if definition.name == PAYLOAD
                || definition.variants.iter().any(|v| v.name.starts_with(PAYLOAD)) => {
                    return Err("canonical message declaration collision".into());
                }
            Item::Function(function) => {
                visit_block(&function.body, &mut |expr| {
                    if matches!(expr, Expr::Call { name, .. } | Expr::Var(name) if name.starts_with(STAMP)) {
                        return Err("canonical message site collision".to_owned());
                    }
                    Ok(())
                })?;
            }
            _ => {}
        }
    }
    let indices = payloads.keys().cloned().enumerate()
        .map(|(index, identity)| (identity, index)).collect::<BTreeMap<_, _>>();
    let mut stamps = BTreeMap::new();
    let (mut module, _, result) = typed.rewrite_into_module(|_, module| {
        for item in &mut module.items {
            if let Item::Function(function) = item {
                visit_block_mut(&mut function.body, &mut |expr| {
                    let address = expr as *const Expr as usize;
                    if let Some((pack, identity)) = sites.get(&address) {
                        let Expr::Call { name, .. } = expr else {
                            return Err("canonical message site changed before consuming checked facts".to_owned());
                        };
                        let stamp = format!("{STAMP}{}", stamps.len());
                        stamps.insert(stamp.clone(), (*pack, indices[identity]));
                        // Rename in place only. No Expr or argument allocation
                        // moves until every address-keyed fact has been read.
                        *name = stamp;
                    }
                    Ok(())
                })?;
            }
        }
        Ok::<_, String>(())
    });
    result?;
    if stamps.len() != sites.len() { return Err("canonical message preparation lost a checked site".into()); }
    module.items.retain(|item| match item {
        Item::Function(function) => recheck_functions.contains(&function.name),
        Item::Comptime(_) => false,
        _ => true,
    });
    let variants = payloads.values().enumerate().map(|(index, ty)| Variant {
        name: variant(index), line: 0, fields: vec![ty.clone()], field_names: vec![], field_lines: vec![],
    }).collect();
    module.items.push(Item::Type(TypeDef {
        name: PAYLOAD.into(), params: vec![], variants, derives: vec![],
        must_consume: false, sealed: true, is_capability: false, grantable: false,
        packed: false, partial_eq_derived: false, public_state_derived: false,
    }));
    for item in &mut module.items {
        match item {
            Item::Function(function) => {
                for parameter in &mut function.params {
                    if let Some(ty) = &mut parameter.ty { replace_type(ty); }
                }
                if let Some(ty) = &mut function.ret { replace_type(ty); }
                block_types(&mut function.body);
                visit_block_mut(&mut function.body, &mut |expr| close(expr, &stamps, payloads.len()))?;
            }
            Item::Type(definition) => for variant in &mut definition.variants {
                for field in &mut variant.fields { replace_type(field); }
            },
            Item::TypeAlias { ty, .. } => replace_type(ty),
            Item::Const { value, .. } => visit_expr_mut(value, &mut |expr| close(expr, &stamps, payloads.len()))?,
            _ => return Err("canonical messages require a monomorphized executable module".into()),
        }
    }
    typeck::annotate_checked(module).map_err(|error| format!("canonical message recheck: {error}"))
}

/// The fresh checker sees only the pruned module produced by message rewriting.
/// Keep an exact, transitive closure of declarations called by its concrete
/// roots so private implementation helpers remain locally bound. Missing calls
/// are deliberately not manufactured into the closure: the fresh checker must
/// continue to reject them.
fn recheck_function_dependencies(module: &witchy_syntax::ast::Module, roots: &BTreeSet<String>) -> BTreeSet<String> {
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some((function.name.as_str(), function)),
        _ => None,
    }).collect::<std::collections::HashMap<_, _>>();
    let mut retained = roots.clone();
    let mut pending = roots.iter().cloned().collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        let Some(function) = functions.get(name.as_str()) else { continue };
        let _: Result<(), ()> = visit_block(&function.body, &mut |expr| {
            let callee = match expr {
                Expr::Call { name, .. } | Expr::Var(name) => Some(name),
                _ => None,
            };
            if let Some(callee) = callee
                && functions.contains_key(callee.as_str())
                && retained.insert(callee.clone())
            {
                pending.push(callee.clone());
            }
            Ok(())
        });
    }
    retained
}

fn reject_unowned(ty: &Type) -> Result<(), String> {
    match ty {
        // An immutable rooted view is a value (RFC-0167 section 5): it
        // retains its own root, so the message is a snapshot the receiving
        // task reads without sharing memory with the sender. It becomes an
        // ordinary field of the closed payload sum, where the carrier
        // witness admits or refuses it like any other aggregate field.
        Type::Qualified(TypeQual::View, inner) => match inner.as_ref() {
            Type::Slice(element) => reject_unowned(element),
            inner => reject_unowned(inner),
        },
        Type::Qualified(_, _) | Type::Slice(_) | Type::Dyn(_, _) => {
            Err(format!("canonical message payload {ty:?} is not yet supported as an owned value"))
        }
        Type::Named(name, args) => {
            if name == "__Msg" { return Err("canonical nested message erasure is not yet supported".into()); }
            for ty in args { reject_unowned(ty)?; }
            Ok(())
        }
        Type::Tuple(items) => { for ty in items { reject_unowned(ty)?; } Ok(()) }
        // Callable identity includes its checked conventions. Captures are
        // checked later by the normal closure/storage witness, not by Dynamic's
        // capability-free predicate or any invented serialization authority.
        Type::Fn(params, result, _, _) => {
            for ty in params { reject_unowned(ty)?; }
            reject_unowned(result)
        }
        Type::RecordCompose { .. } => Err("canonical message record composition must be resolved first".into()),
    }
}

fn close(expr: &mut Expr, stamps: &BTreeMap<String, (bool, usize)>, count: usize) -> Result<(), String> {
    let Expr::Call { name, args } = expr else { return Ok(()) };
    let Some(&(pack, index)) = stamps.get(name) else {
        if intrinsics::is_erasure_bridge(name) {
            return Err("canonical message bridge remains without a concrete checked payload".into());
        }
        return Ok(());
    };
    if args.len() != 1 { return Err("canonical stamped message arity changed".into()); }
    let argument = args.remove(0);
    *expr = if pack {
        Expr::Ctor { name: variant(index), args: vec![argument] }
    } else {
        // A checked singleton-or-empty list gives recovery a genuinely typed
        // trapping expression even though source `fail` returns Unit. The
        // ordinary checked index traps on every wrong tag. No inactive payload
        // is loaded and no dummy value or unchecked cast is introduced.
        Expr::Index { index: Box::new(Expr::Int(0)), base: Box::new(Expr::Match {
            scrutinee: Box::new(argument),
            arms: (0..count).map(|actual| {
                let selected = actual == index;
                MatchArm {
                    line: u32::MAX,
                    pattern: Pattern::Ctor { name: variant(actual), args: vec![if selected {
                        Pattern::Var("$message_value".into())
                    } else { Pattern::Wildcard }] },
                    guard: None,
                    body: Expr::List(if selected { vec![Expr::Var("$message_value".into())] } else { vec![] }),
                }
            }).collect(),
        }) }
    };
    Ok(())
}

fn replace_type(ty: &mut Type) {
    if matches!(ty, Type::Named(name, args) if name == "__Msg" && args.is_empty()) {
        *ty = Type::Named(PAYLOAD.into(), vec![]);
        return;
    }
    match ty {
        Type::Named(_, types) | Type::Dyn(_, types) | Type::Tuple(types) => {
            for ty in types { replace_type(ty); }
        }
        Type::Qualified(_, ty) | Type::Slice(ty) => replace_type(ty),
        Type::Fn(params, result, _, _) => {
            for ty in params { replace_type(ty); }
            replace_type(result);
        }
        Type::RecordCompose { base, fields } => {
            replace_type(base);
            for (_, ty) in fields { replace_type(ty); }
        }
    }
}

fn block_types(block: &mut Block) {
    fn locals(block: &mut Block) {
        for statement in &mut block.stmts {
            if let Stmt::Let { ty: Some(ty), .. } = statement { replace_type(ty); }
        }
    }
    locals(block);
    let _: Result<(), ()> = visit_block_mut(block, &mut |expr| {
        match expr {
            Expr::As { ty, .. } => replace_type(ty),
            Expr::Lambda { params, ret, body, .. } => {
                for parameter in params { if let Some(ty) = &mut parameter.ty { replace_type(ty); } }
                if let Some(ty) = ret { replace_type(ty); }
                locals(body);
            }
            Expr::Block(body) | Expr::While { body, .. } | Expr::For { body, .. }
            | Expr::WhileLet { body, .. } => locals(body),
            Expr::If { then_block, else_block, .. } => {
                locals(then_block);
                if let Some(block) = else_block { locals(block); }
            }
            _ => {}
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use crate::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    use witchy_syntax::origin::OriginTable;

    fn checked(body: &str) -> crate::pipeline::CheckedModule {
        checked_entry(body, "main")
    }

    fn checked_entry(body: &str, entry: &str) -> crate::pipeline::CheckedModule {
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, "test/messages", "0.1.0").unwrap();
        let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
        owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
        }));
        crate::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), b"import task\nfn main() -> Int:\n    task.exercise()\n".to_vec()),
                 ("task".into(), body.as_bytes().to_vec())],
            entry, |_, _, _| Ok(OriginTable::default()),
            &std::collections::HashSet::from(["main".into()]),
            AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap(),
        ).expect("approved primitive-erasure source fixture checks")
    }

    #[test]
    fn recheck_closure_keeps_private_task_helper_for_generated_async_root() {
        let mut module = witchy_syntax::parser::parse_module(r#"
fn generated_async() -> Int:
    task_and_then()
fn task_and_then() -> Int:
    task_and_then_step()
fn task_and_then_step() -> Int:
    1
"#).expect("parse dependency fixture");
        for item in &mut module.items {
            let Item::Function(function) = item else { continue };
            function.name = match function.name.as_str() {
                "task_and_then" => "task.and_then".into(),
                "task_and_then_step" => "task.and_then_step".into(),
                _ => function.name.clone(),
            };
            visit_block_mut(&mut function.body, &mut |expr| {
                if let Expr::Call { name, .. } | Expr::Var(name) = expr {
                    *name = match name.as_str() {
                        "task_and_then" => "task.and_then".into(),
                        "task_and_then_step" => "task.and_then_step".into(),
                        _ => name.clone(),
                    };
                }
                Ok::<_, ()>(())
            }).expect("rewrite dependency names");
        }

        let closure = recheck_function_dependencies(
            &module,
            &BTreeSet::from(["generated_async".to_string()]),
        );
        assert_eq!(
            closure,
            BTreeSet::from([
                "generated_async".to_string(),
                "task.and_then".to_string(),
                "task.and_then_step".to_string(),
            ])
        );
    }

    #[test]
    fn recheck_closure_terminates_on_recursive_private_helpers() {
        let module = witchy_syntax::parser::parse_module(r#"
fn root() -> Int:
    first()
fn first() -> Int:
    second()
fn second() -> Int:
    first()
"#).expect("parse recursive dependency fixture");
        let closure = recheck_function_dependencies(
            &module,
            &BTreeSet::from(["root".to_string()]),
        );
        assert_eq!(
            closure,
            BTreeSet::from(["root".to_string(), "first".to_string(), "second".to_string()])
        );
    }

    #[test]
    fn recheck_closure_leaves_unknown_callees_for_the_fresh_checker() {
        let mut module = witchy_syntax::parser::parse_module(r#"
fn root() -> Int:
    missing()
"#).expect("parse missing dependency fixture");
        let closure = recheck_function_dependencies(
            &module,
            &BTreeSet::from(["root".to_string()]),
        );
        assert_eq!(closure, BTreeSet::from(["root".to_string()]));
        module.items.retain(|item| matches!(item, Item::Function(function) if closure.contains(&function.name)));
        let error = typeck::annotate_checked(module).expect_err("missing declaration must not be synthesized");
        assert!(error.message.contains("missing"), "{error}");
    }

    #[test]
    fn message_preparation_respecializes_private_generic_dependencies() {
        let checked = checked(r#"
pub fn exercise() -> Int:
    let payload = __erase(and_then(1))
    let recovered: Int = __unerase(payload)
    recovered

pub fn and_then(value: a) -> a:
    and_then_step(value)

fn and_then_step(value: a) -> a:
    value
"#);
        let typed = checked.canonical_source().expect("canonical source").typed_module();
        let mut declarations = BTreeSet::new();
        for item in &typed.module().items {
            let Item::Function(function) = item else { continue };
            assert!(
                declarations.insert(function.name.clone()),
                "post-message specialization must not retain a duplicate callable `{}`",
                function.name,
            );
        }
        let public = typed.module().items.iter().find_map(|item| match item {
            Item::Function(function) if function.name.starts_with("task.and_then__") => Some(function),
            _ => None,
        }).expect("message root has a concrete public specialization");
        let mut calls = BTreeSet::new();
        visit_block::<()>(&public.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression {
                calls.insert(name.clone());
            }
            Ok(())
        }).expect("concrete public helper body");
        assert!(
            calls.iter().any(|name| name.starts_with("task.and_then_step__")),
            "post-message specialization must supply the private concrete helper: {calls:?}"
        );
        assert!(
            !calls.contains("task.and_then_step"),
            "a concrete public helper must not retain the private generic call: {calls:?}"
        );
    }

    #[test]
    fn checked_mixed_messages_have_exact_variants_and_tagged_recovery() {
        let checked = checked(r#"
pub fn exercise() -> Int:
    let first = __erase(0)
    let second = __erase("owned")
    let values = [first, second]
    let recovered: Int = __unerase(values[0])
    recovered
"#);
        let canonical = checked.canonical_source().unwrap();
        let typed = canonical.typed_module();
        let definition = typed.module().items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == PAYLOAD => Some(definition),
            _ => None,
        }).expect("compiler-private message sum");
        assert_eq!(definition.variants.len(), 2);
        assert!(definition.variants.iter().any(|v| v.fields == vec![Type::Named("Int".into(), vec![])]));
        assert!(definition.variants.iter().any(|v| v.fields == vec![Type::Named("String".into(), vec![])]));
        let mut traps = 0;
        for item in &typed.module().items {
            if let Item::Function(function) = item {
                visit_block::<()>(&function.body, &mut |expr| {
                    if let Expr::Call { name, .. } = expr {
                        assert!(!intrinsics::is_erasure_bridge(name));
                    }
                    traps += usize::from(matches!(expr, Expr::Index { base, .. } if matches!(base.as_ref(), Expr::Match { .. })));
                    Ok(())
                }).unwrap();
            }
        }
        assert_eq!(traps, 1, "the wrong-tag arm is not a cast");
        assert!(canonical.obligations().is_empty(), "{:?}", canonical.obligations());
    }

    #[test]
    fn ordinary_source_is_unchanged_without_erasure() {
        let typed = typeck::annotate_checked(witchy_syntax::parser::parse_module(
            "fn main() -> Int:\n    41 + 1\n",
        ).unwrap()).unwrap();
        let original = typed.module().clone();
        assert_eq!(prepare(typed, None).unwrap().module(), &original);
    }

    #[test]
    fn library_templates_do_not_enter_the_concrete_message_universe() {
        let checked = checked_entry(r#"
pub fn exercise() -> Int:
    let value = __erase(0)
    let recovered: Int = __unerase(value)
    recovered
pub fn pack(value: a) -> __Msg:
    __erase(value)
"#, "task");
        assert!(checked.module().items.iter().any(|item| matches!(item,
            Item::Function(function) if crate::pipeline::function_has_free_signature_variables(function))));
        let canonical = checked.canonical_source().unwrap().typed_module();
        for item in &canonical.module().items {
            if let Item::Function(function) = item {
                assert!(!crate::pipeline::function_has_free_signature_variables(function));
                visit_block::<()>(&function.body, &mut |expr| {
                    if let Expr::Call { name, .. } = expr {
                        assert!(!intrinsics::is_erasure_bridge(name));
                    }
                    Ok(())
                }).unwrap();
            }
        }
    }

    #[test]
    fn same_shaped_nominals_remain_distinct_message_tags() {
        let checked = checked(r#"
type Left:
    Left(Int)
type Right:
    Right(Int)
pub fn exercise() -> Int:
    let left = __erase(Left(1))
    let right = __erase(Right(2))
    let recovered: Right = __unerase(left)
    match recovered:
        Right(value) -> value
"#);
        let typed = checked.canonical_source().unwrap().typed_module();
        let definition = typed.module().items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == PAYLOAD => Some(definition),
            _ => None,
        }).unwrap();
        assert_eq!(definition.variants.len(), 2);
        assert_ne!(definition.variants[0].fields, definition.variants[1].fields);
        let catalog = checked.runtime_declaration_catalog().unwrap();
        assert_ne!(catalog.type_identity(&definition.variants[0].fields[0]).unwrap(),
            catalog.type_identity(&definition.variants[1].fields[0]).unwrap());
    }

    #[test]
    fn message_preparation_requires_loader_ownership() {
        let typed = typeck::annotate_checked(witchy_syntax::parser::parse_module(
            "fn main() -> Int:\n    let value = __erase(0)\n    let result: Int = __unerase(value)\n    result\n",
        ).unwrap()).unwrap();
        assert!(prepare(typed, None).unwrap_err().contains("authenticated loader ownership"));
    }
}
