//! Typed comparator roots for dictionary primitives whose source bodies are
//! runtime placeholders. Ordinary trait lowering has already selected the
//! implementation universe; retain those semantic calls in canonical scope.

use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Param, Stmt, Type, TypeQual};
use witchy_syntax::ast::visit::visit_block;

use crate::typeck::{self, TypedModule};
use crate::witness::WitnessCatalog;

fn discover(typed: &TypedModule, known: &[(Type, String, String)]) -> Result<Vec<Type>, String> {
    let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
    let mut keys = Vec::<Type>::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name)
            || witchy_canonical_source::scope::function_has_free_signature_variables(function)
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
                if arguments.is_empty() && matches!(name.as_str(), "Int" | "String" | "Duration"))
            {
                return Ok(());
            }
            if !known.iter().any(|(previous, _, _)| previous == key) && !keys.contains(key) {
                keys.push(key.clone());
            }
            Ok(())
        })?;
    }
    Ok(keys)
}

fn discover_equivalents(
    typed: &TypedModule,
    known: &[(Type, Type, String, String)],
) -> Result<Vec<(Type, Type)>, String> {
    let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
    let mut queries = Vec::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name)
            || witchy_canonical_source::scope::function_has_free_signature_variables(function)
        {
            continue;
        }
        visit_block(&function.body, &mut |expression| {
            let Expr::Call { name, args } = expression else { return Ok::<_, String>(()) };
            if name != "dict.__contains_by" || args.len() != 2 { return Ok(()) }
            let Some(dict_ty) = typed.table().type_of(&args[0]).and_then(typeck::ty_to_ast) else {
                return Err("dictionary query has no checked dictionary type".into());
            };
            let Type::Named(dict, children) = dict_ty.unqualified() else {
                return Err("dictionary query has no checked dictionary shape".into());
            };
            if dict != "Dict" || children.len() != 2 {
                return Err("dictionary query has no checked dictionary shape".into());
            }
            let Some(query_ty) = typed.table().type_of(&args[1]).and_then(typeck::ty_to_ast) else {
                return Err("dictionary query has no checked query type".into());
            };
            let Type::Qualified(TypeQual::View, query) = query_ty else {
                return Err("dictionary query requires a rooted query view".into());
            };
            let pair = (children[0].unqualified().clone(), query.unqualified().clone());
            if !known.iter().any(|(key, query, _, _)| *key == pair.0 && *query == pair.1)
                && !queries.contains(&pair)
            {
                queries.push(pair);
            }
            Ok(())
        })?;
    }
    Ok(queries)
}

/// One preparation session, extended as other source transformations expose
/// new dictionary users. Existing wrappers remain tied to their concrete key
/// and checked catalog; their names are never rediscovered as authority.
pub(crate) struct Preparation<'catalog> {
    prefix: String,
    catalog: &'catalog WitnessCatalog,
    key_methods: Vec<(Type, String, String)>,
    equivalent_methods: Vec<(Type, Type, String, String)>,
}

impl<'catalog> Preparation<'catalog> {
    pub(crate) fn new(typed: &TypedModule, catalog: &'catalog WitnessCatalog) -> Result<Self, String> {
        let prefix = format!("{}.$dictionary_eq_", typed.module().linked_entry.as_deref().unwrap_or("$canonical"));
        if typed.module().items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name.starts_with(&prefix)))
        {
            return Err("canonical dictionary comparator declaration collision".into());
        }
        Ok(Self { prefix, catalog, key_methods: Vec::new(), equivalent_methods: Vec::new() })
    }

    pub(crate) fn into_key_methods(self) -> (Vec<(Type, String, String)>, Vec<(Type, Type, String, String)>) {
        (self.key_methods, self.equivalent_methods)
    }

    pub(crate) fn extend(&mut self, mut typed: TypedModule) -> Result<TypedModule, String> {
    loop {
        let keys = discover(&typed, &self.key_methods)?;
        let equivalents = discover_equivalents(&typed, &self.equivalent_methods)?;
        if keys.is_empty() && equivalents.is_empty() { return Ok(typed); }
        let mut module = typed.into_module();
        for key in keys {
        let target = self.catalog.dictionary_equality(&key)?;
        let hash_target = self.catalog.dictionary_hash(&key)?;
        if !module.items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name == target
                && !witchy_canonical_source::scope::function_has_free_signature_variables(function)))
        {
            return Err(format!("canonical dictionary comparator for `{}` has no concrete checked body",
                witchy_syntax::format::type_str(&key)));
        }
        if !module.items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name == hash_target
                && !witchy_canonical_source::scope::function_has_free_signature_variables(function)))
        {
            return Err(format!("canonical dictionary hash for `{}` has no concrete checked body",
                witchy_syntax::format::type_str(&key)));
        }
        // The wrapper is an actual called function, not a fabricated scope
        // reference. Its checked body makes the selected implementation and
        // all its transitive callees part of ordinary canonical reachability.
        let name = format!("{}{}", self.prefix, self.key_methods.len());
        module.items.push(Item::Function(Function {
            line: 0, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: name.clone(),
            type_params: Vec::new(),
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
        let hash_name = format!("{}hash_{}", self.prefix, self.key_methods.len());
        module.items.push(Item::Function(Function {
            line: 0, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: hash_name.clone(),
            type_params: Vec::new(),
            params: vec![Param {
                name: "key".into(), ty: Some(key.clone()),
                convention: Convention::Let, default: None,
            }],
            ret: Some(Type::Named("Int".into(), Vec::new())),
            body: Block {
                stmts: vec![Stmt::Expr(Expr::Call {
                    name: hash_target,
                    args: vec![Expr::Var("key".into())],
                })],
                lines: vec![0], region: None,
            },
            bounds: Vec::new(), is_gen: false, is_async: false,
        }));
        self.key_methods.push((key, name, hash_name));
        }
        for (key, query) in equivalents {
            let target = self.catalog.dictionary_equivalent_method(&query, &key, "equivalent")?;
            let hash_target = self.catalog.dictionary_equivalent_method(&query, &key, "equivalent_hash")?;
            for method in [&target, &hash_target] {
                if !module.items.iter().any(|item| matches!(item,
                    Item::Function(function) if function.name == *method
                        && !witchy_canonical_source::scope::function_has_free_signature_variables(function)))
                {
                    return Err(format!("dictionary query method `{method}` has no concrete checked body"));
                }
            }
            let query_type = Type::Qualified(TypeQual::View, Box::new(query.clone()));
            let index = self.equivalent_methods.len();
            let eq_name = format!("{}query_{index}", self.prefix);
            module.items.push(Item::Function(Function {
                line: 0, public: false, pure: false, comptime_only: false,
                attributes: Vec::new(), name: eq_name.clone(), type_params: Vec::new(),
                params: vec![
                    Param { name: "query".into(), ty: Some(query_type.clone()), convention: Convention::Let, default: None },
                    Param { name: "key".into(), ty: Some(key.clone()), convention: Convention::Let, default: None },
                ],
                ret: Some(Type::Named("Bool".into(), Vec::new())),
                body: Block { stmts: vec![Stmt::Expr(Expr::Call { name: target,
                    args: vec![Expr::Var("query".into()), Expr::Var("key".into())] })],
                    lines: vec![0], region: None },
                bounds: Vec::new(), is_gen: false, is_async: false,
            }));
            let hash_name = format!("{}query_hash_{index}", self.prefix);
            module.items.push(Item::Function(Function {
                line: 0, public: false, pure: false, comptime_only: false,
                attributes: Vec::new(), name: hash_name.clone(), type_params: Vec::new(),
                params: vec![Param { name: "query".into(), ty: Some(query_type), convention: Convention::Let, default: None }],
                ret: Some(Type::Named("Int".into(), Vec::new())),
                body: Block { stmts: vec![Stmt::Expr(Expr::Call { name: hash_target,
                    args: vec![Expr::Var("query".into())] })], lines: vec![0], region: None },
                bounds: Vec::new(), is_gen: false, is_async: false,
            }));
            self.equivalent_methods.push((key, query, eq_name, hash_name));
        }
        // Comparator bodies can themselves use dictionaries. Close over newly
        // reachable bodies, visiting each concrete key once. No pass invents
        // bodies here: targets must already exist in the finite checked module.
        // Structural mutation also invalidates all expression-address facts.
        typed = typeck::annotate_checked(module).map_err(|error| error.to_string())?;
    }
    }
}
