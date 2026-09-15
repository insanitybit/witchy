//! Typed comparator roots for dictionary primitives whose source bodies are
//! runtime placeholders. Ordinary trait lowering has already selected the
//! implementation universe; retain those semantic calls in canonical scope.

use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Param, Stmt, Type};
use witchy_syntax::ast::visit::visit_block;

use crate::typeck::{self, TypedModule};
use crate::witness::WitnessCatalog;

fn discover(typed: &TypedModule, known: &[(Type, String)]) -> Result<Vec<Type>, String> {
    let scope = crate::pipeline::canonical_scope(typed.module());
    let mut keys = Vec::<Type>::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name)
            || crate::pipeline::function_has_free_signature_variables(function)
        {
            continue;
        }
        visit_block(&function.body, &mut |expression| {
            let Some(ty) = typed.table().type_of(expression).and_then(typeck::ty_to_ast) else {
                return Ok::<_, String>(());
            };
            let Type::Named(name, arguments) = ty.unqualified() else { return Ok(()) };
            if name != "Dict" || arguments.len() != 2 { return Ok(()) }
            let key = arguments[0].unqualified();
            if matches!(key, Type::Named(name, arguments)
                if arguments.is_empty() && matches!(name.as_str(), "Int" | "String"))
            {
                return Ok(());
            }
            if !known.iter().any(|(previous, _)| previous == key) && !keys.contains(key) {
                keys.push(key.clone());
            }
            Ok(())
        })?;
    }
    Ok(keys)
}

/// One preparation session, extended as other source transformations expose
/// new dictionary users. Existing wrappers remain tied to their concrete key
/// and checked catalog; their names are never rediscovered as authority.
pub(crate) struct Preparation<'catalog> {
    prefix: String,
    catalog: &'catalog WitnessCatalog,
    comparators: Vec<(Type, String)>,
}

impl<'catalog> Preparation<'catalog> {
    pub(crate) fn new(typed: &TypedModule, catalog: &'catalog WitnessCatalog) -> Result<Self, String> {
        let prefix = format!("{}.$dictionary_eq_", typed.module().linked_entry.as_deref().unwrap_or("$canonical"));
        if typed.module().items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name.starts_with(&prefix)))
        {
            return Err("canonical dictionary comparator declaration collision".into());
        }
        Ok(Self { prefix, catalog, comparators: Vec::new() })
    }

    pub(crate) fn into_comparators(self) -> Vec<(Type, String)> { self.comparators }

    pub(crate) fn extend(&mut self, mut typed: TypedModule) -> Result<TypedModule, String> {
    loop {
        let keys = discover(&typed, &self.comparators)?;
        if keys.is_empty() { return Ok(typed); }
        let mut module = typed.into_module();
        for key in keys {
        let target = self.catalog.dictionary_equality(&key)?;
        if !module.items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name == target
                && !crate::pipeline::function_has_free_signature_variables(function)))
        {
            return Err(format!("canonical dictionary comparator for `{}` has no concrete checked body",
                witchy_syntax::format::type_str(&key)));
        }
        // The wrapper is an actual called function, not a fabricated scope
        // reference. Its checked body makes the selected implementation and
        // all its transitive callees part of ordinary canonical reachability.
        let name = format!("{}{}", self.prefix, self.comparators.len());
        module.items.push(Item::Function(Function {
            line: 0, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: name.clone(),
            params: ["left", "right"].into_iter().map(|name| Param {
                name: name.into(), ty: Some(key.clone()),
                convention: Convention::Let, default: None,
            }).collect(),
            ret: Some(Type::Named("Bool".into(), Vec::new())),
            body: Block {
                stmts: vec![Stmt::Expr(Expr::Call {
                    name: target,
                    args: vec![Expr::Var("left".into()), Expr::Var("right".into())],
                })],
                lines: vec![0], region: None,
            },
            bounds: Vec::new(), is_gen: false, is_async: false,
        }));
        self.comparators.push((key, name));
        }
        // Comparator bodies can themselves use dictionaries. Close over newly
        // reachable bodies, visiting each concrete key once. No pass invents
        // bodies here: targets must already exist in the finite checked module.
        // Structural mutation also invalidates all expression-address facts.
        typed = typeck::annotate_checked(module).map_err(|error| error.to_string())?;
    }
    }
}
