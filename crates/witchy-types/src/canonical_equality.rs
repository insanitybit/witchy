//! Native compound equality prepared as ordinary, checked source functions.
//!
//! No storage identity participates in equality. Helpers read values through
//! patterns/indexing and call the real custom PartialEq implementation at each
//! nominal boundary. Their names carry no admission or memory authority.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{
    BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Param, Pattern,
    Stmt, Type, TypeDef, UnOp,
};
use witchy_syntax::ast::visit::{visit_block, visit_block_mut, visit_expr_mut};

use witchy_canonical_source::aggregates::{concrete_aggregate_identity, scalar_aggregate_shape, type_definitions};
use witchy_canonical_source::specialization::LogicalSpecializationIdentity;
use crate::typeck::{self, TypeTable, TypedModule};
use crate::witness::WitnessCatalog;

type Instances = BTreeMap<String, LogicalSpecializationIdentity>;

struct Helper {
    identity: Type,
    ty: Type,
    function: Function,
}

struct Preparation<'a> {
    definitions: BTreeMap<String, TypeDef>,
    catalog: Option<&'a WitnessCatalog>,
    functions: BTreeSet<String>,
    prefix: String,
    helpers: Vec<Helper>,
    needs_specialization: bool,
}

/// Retain helper reservations, collision checks, and specialization identities
/// across transformations that expose additional authenticated executable roots.
pub(crate) struct PreparationSession<'catalog> {
    equality: Preparation<'catalog>,
    dictionaries: Option<crate::canonical_dictionary::Preparation<'catalog>>,
    rendering: crate::canonical_render::Preparation,
    instances: Instances,
}

impl<'catalog> PreparationSession<'catalog> {
pub(crate) fn new(
    typed: &TypedModule,
    catalog: Option<&'catalog WitnessCatalog>,
    instances: Instances,
) -> Result<Self, String> {
    let prefix = format!("{}.$structural_eq_", typed.module().linked_entry.as_deref().unwrap_or("$canonical"));
    if typed.module().items.iter().any(|item| matches!(item,
        Item::Function(function) if function.name.starts_with(&prefix)))
    {
        return Err("canonical equality helper declaration collision".into());
    }
    let equality = Preparation {
        definitions: type_definitions(typed.module()), catalog,
        functions: BTreeSet::new(), prefix, helpers: Vec::new(), needs_specialization: false,
    };
    let dictionaries = catalog.map(|catalog|
        crate::canonical_dictionary::Preparation::new(typed, catalog)).transpose()?;
    let rendering = crate::canonical_render::Preparation::new(typed)?;
    Ok(Self { equality, dictionaries, rendering, instances })
}

pub(crate) fn into_metadata(self) -> (Instances, Vec<(Type, String)>) {
    let comparators = self.dictionaries.map(|dictionaries| dictionaries.into_comparators()).unwrap_or_default();
    (self.instances, comparators)
}

    /// Carry specializations produced after a fallible canonical rewrite into
    /// the original reservation session. A repeated exact identity is benign;
    /// reusing a generated name for a different checked identity is not.
    pub(crate) fn merge_specialization_instances(&mut self, additional: Instances) -> Result<(), String> {
        merge_specialization_instances(&mut self.instances, additional)
    }

    pub(crate) fn has_specialization_instance(&self, name: &str) -> bool {
        self.instances.contains_key(name)
    }

pub(crate) fn extend(&mut self, mut typed: TypedModule) -> Result<TypedModule, String> {
    let catalog = self.equality.catalog;
    let preparation = &mut self.equality;
    let dictionaries = &mut self.dictionaries;
    let rendering = &mut self.rendering;
    let instances = &mut self.instances;
    loop {
        // Equality can reveal a custom method that uses a new dictionary key;
        // a dictionary comparator can itself contain compound equality. Close
        // both worklists with one catalog-bound dictionary preparation session.
        if let Some(dictionaries) = dictionaries.as_mut() {
            typed = dictionaries.extend(typed)?;
        }
        // Native render helpers can expose dictionary fields; selected custom
        // equality bodies can expose further rendering. Retain all three
        // preparation sessions until the checked executable scope is closed.
        let (prepared, rendered) = rendering.extend(typed)?;
        typed = prepared;
        if rendered { continue; }
        let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
        preparation.functions = typed.module().items.iter().filter_map(|item| match item {
            Item::Function(function) => Some(function.name.clone()),
            _ => None,
        }).collect();
        preparation.definitions = type_definitions(typed.module());
        // Preserve the immutable typed owner on the common no-op path. Do not
        // consume and reconstruct its address-keyed table merely to discover
        // that no source comparison needs preparation.
        let mut needs_rewrite = false;
        for item in &typed.module().items {
            if let Item::Function(function) = item
                && scope.contains(&function.name)
                && !witchy_canonical_source::scope::function_has_free_signature_variables(function)
            {
                visit_block::<String>(&function.body, &mut |expression| {
                    if let Expr::Binary { op: BinOp::Eq | BinOp::NotEq, lhs, .. } = expression
                        && let Some(ty) = typed.table().type_of(lhs).and_then(typeck::ty_to_ast)
                        && !primitive(&ty)
                        && matches!(concrete_aggregate_identity(&ty, &preparation.definitions),
                            Some(Type::Tuple(_) | Type::Named(_, _)))
                    {
                        needs_rewrite = true;
                    }
                    Ok(())
                })?;
            }
        }
        if !needs_rewrite {
            return Ok(typed);
        }
        let first_new = preparation.helpers.len();
        let mut changed = false;
        let (mut module, _, result) = typed.rewrite_into_module(|table, module| {
            for item in &mut module.items {
                if let Item::Function(function) = item
                    && scope.contains(&function.name)
                    && !witchy_canonical_source::scope::function_has_free_signature_variables(function)
                {
                    visit_block_mut(&mut function.body, &mut |expression| {
                        preparation.rewrite(expression, table, &mut changed)
                    })?;
                }
            }
            Ok::<_, String>(())
        });
        result?;
        if !changed {
            return Err("equality preparation lost a checked comparison discovered by its preflight".into());
        }
        let mut next = first_new;
        while next < preparation.helpers.len() {
            let ty = preparation.helpers[next].ty.clone();
            let (body, pure) = preparation.body(&ty)?;
            preparation.helpers[next].function.body = body;
            preparation.helpers[next].function.pure = pure;
            next += 1;
        }
        // A recursive group is pure only when every reachable comparator is
        // pure. Start with structural reads and propagate custom-call effects.
        loop {
            let impure = preparation.helpers.iter().filter(|helper| !helper.function.pure)
                .map(|helper| helper.function.name.clone()).collect::<BTreeSet<_>>();
            let mut progress = false;
            for helper in &mut preparation.helpers {
                if !helper.function.pure { continue; }
                let mut calls_impure = false;
                visit_block::<String>(&helper.function.body, &mut |expression| {
                    if let Expr::Call { name, .. } = expression {
                        calls_impure |= impure.contains(name);
                    }
                    Ok(())
                })?;
                if calls_impure { helper.function.pure = false; progress = true; }
            }
            if !progress { break; }
        }
        let new_names = preparation.helpers[first_new..].iter()
            .map(|helper| helper.function.name.clone()).collect::<BTreeSet<_>>();
        module.items.extend(preparation.helpers[first_new..].iter()
            .map(|helper| Item::Function(helper.function.clone())));
        if preparation.needs_specialization {
            let catalog = catalog.ok_or("custom equality requires its retained checked declarations")?;
            let dispatch = catalog.equality_dispatch_module(module.clone());
            let (elaborated, generated) = crate::traits::lower_for_wasm_with_specializations(dispatch).into_parts();
            // Keep all existing checked bodies. Import the rewritten new roots
            // and genuinely new specializations with their original identities.
            let mut known = module.items.iter().filter_map(|item| match item {
                Item::Function(function) => Some(function.name.clone()), _ => None,
            }).collect::<BTreeSet<_>>();
            for item in elaborated.items {
                let Item::Function(function) = item else { continue };
                if new_names.contains(&function.name) {
                    let existing = module.items.iter_mut().find(|item| matches!(item,
                        Item::Function(previous) if previous.name == function.name))
                        .ok_or("equality specialization lost its root")?;
                    *existing = Item::Function(function);
                } else if known.insert(function.name.clone()) {
                    module.items.push(Item::Function(function));
                }
            }
            for (name, identity) in generated {
                if instances.get(&name).is_some_and(|previous| previous != &identity) {
                    return Err(format!("equality specialization identity changed for `{name}`"));
                }
                instances.insert(name, identity);
            }
            preparation.needs_specialization = false;
        }
        typed = typeck::annotate_checked(module).map_err(|error| error.to_string())?;
        // Newly reachable custom bodies may contain further native compound
        // comparisons. Revisit checked source, never the invalidated old table.
    }
}
}

fn merge_specialization_instances(into: &mut Instances, additional: Instances) -> Result<(), String> {
    for (name, identity) in additional {
        match into.get(&name) {
            Some(existing) if existing == &identity => {}
            Some(_) => return Err(format!(
                "canonical specialization `{name}` has conflicting checked identities"
            )),
            None => {
                into.insert(name, identity);
            }
        }
    }
    Ok(())
}

impl Preparation<'_> {
    fn rewrite(&mut self, expression: &mut Expr, table: &TypeTable, changed: &mut bool) -> Result<(), String> {
        let Expr::Binary { op: operator @ (BinOp::Eq | BinOp::NotEq), lhs, rhs } = expression else {
            return Ok(());
        };
        let Some(left) = table.type_of(lhs).and_then(typeck::ty_to_ast) else { return Ok(()) };
        if primitive(&left) { return Ok(()); }
        let Some(identity) = concrete_aggregate_identity(&left, &self.definitions) else { return Ok(()) };
        if !matches!(identity, Type::Tuple(_) | Type::Named(_, _)) { return Ok(()); }
        let right = table.type_of(rhs).and_then(typeck::ty_to_ast)
            .ok_or("compound equality lost its checked right operand type")?;
        if concrete_aggregate_identity(&right, &self.definitions).as_ref() != Some(&identity) {
            return Err("compound equality operands have different checked identities".into());
        }
        let helper = self.reserve(&left)?;
        let negate = *operator == BinOp::NotEq;
        // Rewrite descendants while their old expression addresses are valid.
        // The surrounding visitor can revisit the replacement afterwards, but
        // it contains no newly unprepared comparison nodes.
        visit_expr_mut(lhs, &mut |child| self.rewrite(child, table, changed))?;
        visit_expr_mut(rhs, &mut |child| self.rewrite(child, table, changed))?;
        let left = std::mem::replace(lhs.as_mut(), Expr::Bool(false));
        let right = std::mem::replace(rhs.as_mut(), Expr::Bool(false));
        let call = call(&helper, vec![left, right]);
        *expression = if negate { not(call) } else { call };
        *changed = true;
        Ok(())
    }

    fn reserve(&mut self, ty: &Type) -> Result<String, String> {
        let identity = concrete_aggregate_identity(ty, &self.definitions)
            .ok_or("structural equality needs a concrete checked type")?;
        // Keep qualifier contracts distinct even when their physical aggregate
        // identity is identical. Parameter checking still owns those contracts.
        if let Some(helper) = self.helpers.iter().find(|helper|
            helper.identity == identity && helper.ty == *ty)
        {
            return Ok(helper.function.name.clone());
        }
        // This is a bounded source expansion, not runtime recursion unrolling.
        // Ordinary Stack(a) reserves its identity before visiting Stack(a).
        if self.helpers.len() >= 4096 {
            return Err("structural equality has too many concrete comparator identities".into());
        }
        let name = format!("{}{}", self.prefix, self.helpers.len());
        let function = Function {
            line: 0, public: false, pure: true, comptime_only: false, attributes: Vec::new(),
            name: name.clone(),
            type_params: Vec::new(),
            params: ["left", "right"].into_iter().map(|name| Param {
                name: name.into(), ty: Some(ty.clone()), convention: Convention::Let, default: None,
            }).collect(),
            ret: Some(Type::Named("Bool".into(), Vec::new())), body: block(Vec::new()),
            bounds: Vec::new(), is_gen: false, is_async: false,
        };
        self.helpers.push(Helper { identity, ty: ty.clone(), function });
        Ok(name)
    }

    fn equal(&mut self, ty: &Type, left: Expr, right: Expr) -> Result<Expr, String> {
        if primitive(ty) {
            if matches!(ty.unqualified(), Type::Named(name, _) if name == "Nil" || name == "Unit") {
                return Ok(Expr::Bool(true));
            }
            return Ok(binary(BinOp::Eq, left, right));
        }
        Ok(call(&self.reserve(ty)?, vec![left, right]))
    }

    fn body(&mut self, ty: &Type) -> Result<(Block, bool), String> {
        match ty.unqualified() {
            Type::Tuple(fields) => {
                let mut statements = vec![
                    destructure(Pattern::Tuple(bindings("l", fields.len())), var("left")),
                    destructure(Pattern::Tuple(bindings("r", fields.len())), var("right")),
                ];
                let mut terms = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    terms.push(self.equal(field, var(&format!("l{index}")), var(&format!("r{index}")))?);
                }
                statements.push(Stmt::Expr(conjunction(terms)));
                Ok((block(statements), true))
            }
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                let element = &arguments[0];
                let comparison = self.equal(element, index("left"), index("right"))?;
                Ok((block(vec![
                    binding("length", false, call("list.length", vec![var("left")])),
                    reject_if(binary(BinOp::NotEq, var("length"), call("list.length", vec![var("right")]))),
                    binding("cursor", true, Expr::Int(0)),
                    Stmt::Expr(Expr::While {
                        cond: Box::new(binary(BinOp::Lt, var("cursor"), var("length"))),
                        body: block(vec![
                            reject_if(not(comparison)),
                            Stmt::Assign { name: "cursor".into(),
                                value: binary(BinOp::Add, var("cursor"), Expr::Int(1)), place_depth: 0 },
                        ]),
                    }),
                    Stmt::Expr(Expr::Bool(true)),
                ]), true))
            }
            Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2 => {
                // Native structural Dict equality is insertion-order-sensitive,
                // unlike the standard Dict PartialEq method. Compare each key
                // and then its value, stopping before later payload effects.
                // Ordered key/value lists also support nominal keys for which
                // the current dict.pairs lowering cannot materialize a pair.
                let key_equal = self.equal(&arguments[0], indexed("left_keys"), indexed("right_keys"))?;
                let value_equal = self.equal(&arguments[1], indexed("left_values"), indexed("right_values"))?;
                Ok((block(vec![
                    binding("length", false, call("dict.length", vec![var("left")])),
                    reject_if(binary(BinOp::NotEq, var("length"), call("dict.length", vec![var("right")]))),
                    binding("left_keys", false, call("dict.keys", vec![var("left")])),
                    binding("right_keys", false, call("dict.keys", vec![var("right")])),
                    binding("left_values", false, call("dict.values", vec![var("left")])),
                    binding("right_values", false, call("dict.values", vec![var("right")])),
                    binding("cursor", true, Expr::Int(0)),
                    Stmt::Expr(Expr::While {
                        cond: Box::new(binary(BinOp::Lt, var("cursor"), var("length"))),
                        body: block(vec![
                            reject_if(not(key_equal)),
                            reject_if(not(value_equal)),
                            Stmt::Assign { name: "cursor".into(),
                                value: binary(BinOp::Add, var("cursor"), Expr::Int(1)), place_depth: 0 },
                        ]),
                    }),
                    Stmt::Expr(Expr::Bool(true)),
                ]), true))
            }
            Type::Named(name, _) => {
                if witchy_canonical_source::capabilities::representation(ty, &self.definitions).is_some() {
                    return Err("structural equality cannot inspect a capability representation".into());
                }
                let definition = self.definitions.get(name);
                if !definition.is_some_and(|definition| definition.partial_eq_derived) {
                    if let Some(method) = self.catalog.map(|catalog| catalog.equality_method(ty)).transpose()?.flatten() {
                        let target = if self.functions.contains(&method.instance) {
                            method.instance
                        } else {
                            self.needs_specialization = true;
                            method.template
                        };
                        return Ok((block(vec![Stmt::Expr(call(&target, vec![var("left"), var("right")]))]), method.pure));
                    }
                }
                let shape = scalar_aggregate_shape(ty, &self.definitions)
                    .ok_or("structural equality has no finite checked nominal shape")?;
                let mut arms = Vec::new();
                for variant in &shape.variants {
                    let mut terms = Vec::new();
                    for (index, field) in shape.fields[variant.first_field..variant.first_field + variant.arity].iter().enumerate() {
                        terms.push(self.equal(field, var(&format!("l{index}")), var(&format!("r{index}")))?);
                    }
                    let mut right_arms = vec![arm(Pattern::Ctor {
                        name: variant.name.clone(), args: bindings("r", variant.arity),
                    }, conjunction(terms))];
                    if shape.variants.len() > 1 {
                        right_arms.push(arm(Pattern::Wildcard, Expr::Bool(false)));
                    }
                    arms.push(arm(Pattern::Ctor {
                        name: variant.name.clone(), args: bindings("l", variant.arity),
                    }, Expr::Match { scrutinee: Box::new(var("right")), arms: right_arms }));
                }
                if arms.is_empty() { return Err("structural equality cannot inspect a fieldless nominal type".into()); }
                Ok((block(vec![Stmt::Expr(Expr::Match { scrutinee: Box::new(var("left")), arms })]), true))
            }
            _ => Err("structural equality does not support this checked operand type".into()),
        }
    }
}

fn primitive(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Named(name, arguments) if arguments.is_empty()
        && matches!(name.as_str(), "Int" | "Float" | "Bool" | "Duration" | "String" | "Nil" | "Unit"))
}

pub(crate) fn var(name: &str) -> Expr { Expr::Var(name.into()) }
pub(crate) fn call(name: &str, args: Vec<Expr>) -> Expr { Expr::Call { name: name.into(), args } }
pub(crate) fn binary(op: BinOp, left: Expr, right: Expr) -> Expr {
    Expr::Binary { op, lhs: Box::new(left), rhs: Box::new(right) }
}
fn not(expr: Expr) -> Expr { Expr::Unary { op: UnOp::Not, expr: Box::new(expr) } }
pub(crate) fn block(stmts: Vec<Stmt>) -> Block { Block { lines: vec![0; stmts.len()], stmts, region: None } }
pub(crate) fn binding(name: &str, mutable: bool, value: Expr) -> Stmt {
    Stmt::Let { name: name.into(), ty: None, mutable, value }
}
pub(crate) fn destructure(pattern: Pattern, value: Expr) -> Stmt { Stmt::LetPattern { pattern, value } }
pub(crate) fn bindings(prefix: &str, count: usize) -> Vec<Pattern> {
    (0..count).map(|index| Pattern::Var(format!("{prefix}{index}"))).collect()
}
pub(crate) fn arm(pattern: Pattern, body: Expr) -> MatchArm { MatchArm { line: 0, pattern, guard: None, body } }
fn conjunction(terms: Vec<Expr>) -> Expr {
    terms.into_iter().reduce(|left, right| binary(BinOp::And, left, right)).unwrap_or(Expr::Bool(true))
}
fn indexed(name: &str) -> Expr { Expr::Index { base: Box::new(var(name)), index: Box::new(var("cursor")) } }
fn index(name: &str) -> Expr { indexed(name) }
fn reject_if(condition: Expr) -> Stmt {
    Stmt::Expr(Expr::If { cond: Box::new(condition),
        then_block: block(vec![Stmt::Return(Some(Expr::Bool(false)))]), else_block: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialization_identity_conflict_is_rejected_while_duplicate_coalesces() {
        let int = LogicalSpecializationIdentity::from_types(&[Type::Named("Int".into(), vec![])]);
        let string = LogicalSpecializationIdentity::from_types(&[Type::Named("String".into(), vec![])]);
        let mut instances = Instances::from([("same".into(), int.clone())]);

        merge_specialization_instances(
            &mut instances,
            Instances::from([("same".into(), int)]),
        ).expect("the same checked specialization identity coalesces");

        let error = merge_specialization_instances(
            &mut instances,
            Instances::from([("same".into(), string)]),
        ).expect_err("a reused specialization name needs the same identity");
        assert!(error.contains("conflicting checked identities"), "{error}");
    }
}
