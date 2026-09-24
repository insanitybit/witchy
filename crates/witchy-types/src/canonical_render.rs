//! Native structural rendering expressed as ordinary checked source calls.
//!
//! Trait elaboration has already selected public Show calls. Only the remaining
//! structural intrinsic is prepared here: its children are structural too,
//! regardless of any Show implementations on their types. Scalar intrinsics
//! remain leaves, so lowerer payload/import discovery never unfolds a recursive
//! nominal graph. Helper spelling carries no callable or storage authority.

use std::collections::BTreeMap;

use witchy_syntax::ast::{
    BinOp, Block, Convention, Expr, Function, Item, Param, Pattern, Stmt, Type, TypeDef,
};
use witchy_syntax::ast::visit::{visit_block, visit_block_mut, visit_expr_mut};

use witchy_canonical_source::aggregates::{concrete_aggregate_identity, scalar_aggregate_shape, type_definitions};
use crate::canonical_equality::{arm, binary, binding, bindings, block, call, destructure, var};
use crate::typeck::{self, TypeTable, TypedModule};

struct Helper {
    identity: Type,
    ty: Type,
    function: Function,
}

/// Retained alongside dictionary/equality preparation, not restarted when
/// specialization reveals another concrete body needing native rendering.
pub(crate) struct Preparation {
    definitions: BTreeMap<String, TypeDef>,
    prefix: String,
    helpers: Vec<Helper>,
}

impl Preparation {
    pub(crate) fn new(typed: &TypedModule) -> Result<Self, String> {
        let prefix = format!("{}.$structural_render_",
            typed.module().linked_entry.as_deref().unwrap_or("$canonical"));
        if typed.module().items.iter().any(|item| matches!(item,
            Item::Function(function) if function.name.starts_with(&prefix)))
        {
            return Err("canonical render helper declaration collision".into());
        }
        Ok(Self { definitions: type_definitions(typed.module()), prefix, helpers: Vec::new() })
    }

    pub(crate) fn extend(&mut self, typed: TypedModule) -> Result<(TypedModule, bool), String> {
        self.definitions = type_definitions(typed.module());
        let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
        let mut needed = false;
        for item in &typed.module().items {
            if let Item::Function(function) = item
                && scope.contains(&function.name)
                && !witchy_canonical_source::scope::function_has_free_signature_variables(function)
            {
                visit_block::<String>(&function.body, &mut |expression| {
                    if let Some(ty) = render_operand_type(expression, typed.table())
                        && compound(&ty)
                    {
                        needed = true;
                    }
                    Ok(())
                })?;
            }
        }
        // Preserve the address-keyed checked table on the no-change path.
        if !needed { return Ok((typed, false)); }
        let first_new = self.helpers.len();
        let mut changed = false;
        let (mut module, _, result) = typed.rewrite_into_module(|table, module| {
            for item in &mut module.items {
                if let Item::Function(function) = item
                    && scope.contains(&function.name)
                    && !witchy_canonical_source::scope::function_has_free_signature_variables(function)
                {
                    visit_block_mut(&mut function.body, &mut |expression| {
                        self.rewrite(expression, table, &mut changed)
                    })?;
                }
            }
            Ok::<_, String>(())
        });
        result?;
        if !changed {
            return Err("render preparation lost a checked operand discovered by its preflight".into());
        }
        // reserve() only records an identity and signature. Expanding a child
        // adds work here, never recursively constructs another function body.
        let mut next = first_new;
        while next < self.helpers.len() {
            let ty = self.helpers[next].ty.clone();
            self.helpers[next].function.body = self.body(&ty)?;
            next += 1;
        }
        module.items.extend(self.helpers[first_new..].iter()
            .map(|helper| Item::Function(helper.function.clone())));
        let typed = typeck::annotate_checked(module).map_err(|error| error.to_string())?;
        Ok((typed, true))
    }

    fn rewrite(&mut self, expression: &mut Expr, table: &TypeTable, changed: &mut bool) -> Result<(), String> {
        let Some(ty) = render_operand_type(expression, table) else { return Ok(()) };
        if !compound(&ty) { return Ok(()); }
        let helper = self.reserve(&ty)?;
        let Expr::Call { args, .. } = expression else { unreachable!() };
        let argument = &mut args[0];
        // Visit nested source operands before moving their address-keyed nodes.
        // The resulting ordinary call evaluates this operand exactly once.
        visit_expr_mut(argument, &mut |child| self.rewrite(child, table, changed))?;
        let argument = std::mem::replace(argument, Expr::Bool(false));
        *expression = call(&helper, vec![argument]);
        *changed = true;
        Ok(())
    }

    fn reserve(&mut self, ty: &Type) -> Result<String, String> {
        let identity = concrete_aggregate_identity(ty, &self.definitions)
            .ok_or("structural rendering needs a concrete checked type")?;
        if witchy_canonical_source::capabilities::representation(ty, &self.definitions).is_some() {
            return Err("structural rendering cannot inspect a capability representation".into());
        }
        // Concrete arguments AND source qualifiers are part of the key. Equal
        // physical layouts do not identify Int/Float or erase a qualifier.
        if let Some(helper) = self.helpers.iter().find(|helper|
            helper.identity == identity && helper.ty == *ty)
        {
            return Ok(helper.function.name.clone());
        }
        if self.helpers.len() >= 4096 {
            return Err("structural rendering has too many concrete helper identities".into());
        }
        let name = format!("{}{}", self.prefix, self.helpers.len());
        self.helpers.push(Helper {
            identity, ty: ty.clone(),
            function: Function {
                line: 0, public: false, pure: true, comptime_only: false, attributes: Vec::new(),
                name: name.clone(),
                type_params: Vec::new(),
                params: vec![Param {
                    name: "value".into(), ty: Some(ty.clone()), convention: Convention::Let, default: None,
                }],
                ret: Some(Type::Named("String".into(), Vec::new())), body: block(Vec::new()),
                bounds: Vec::new(), is_gen: false, is_async: false,
            },
        });
        Ok(name)
    }

    fn render(&mut self, ty: &Type, value: Expr) -> Result<Expr, String> {
        if compound(ty) {
            Ok(call(&self.reserve(ty)?, vec![value]))
        } else {
            // Do not send these leaves through trait elaboration: native
            // structural fallback does not invoke a nested custom Show.
            Ok(call(witchy_syntax::intrinsics::GENERATED_RENDER, vec![value]))
        }
    }

    fn body(&mut self, ty: &Type) -> Result<Block, String> {
        match ty.unqualified() {
            Type::Tuple(fields) => {
                let mut pieces = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    pieces.push(self.render(field, var(&format!("field{index}")))?);
                }
                Ok(block(vec![
                    destructure(Pattern::Tuple(bindings("field", fields.len())), var("value")),
                    Stmt::Expr(joined("(", pieces, ")")),
                ]))
            }
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                let rendered = self.render(&arguments[0], indexed("value"))?;
                Ok(sequence("[", "]", vec![
                    binding("length", false, call("list.length", vec![var("value")])),
                ], rendered))
            }
            Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2 => {
                // The source fallback renders entry key, then entry value, in
                // insertion order. It is NOT the public Dict Show protocol.
                let key = self.render(&arguments[0], indexed("keys"))?;
                let value = self.render(&arguments[1], indexed("values"))?;
                Ok(sequence("{", "}", vec![
                    binding("length", false, call("dict.length", vec![var("value")])),
                    binding("keys", false, call("dict.keys", vec![var("value")])),
                    binding("values", false, call("dict.values", vec![var("value")])),
                ], concat(concat(key, string(": ")), value)))
            }
            Type::Named(name, _) => {
                let shape = scalar_aggregate_shape(ty, &self.definitions)
                    .ok_or("structural rendering has no finite checked nominal shape")?;
                let mut arms = Vec::new();
                for variant in &shape.variants {
                    let anonymous = witchy_syntax::linker::is_generated_anon_name(&variant.name);
                    let field_names = if anonymous {
                        let declaration = self.definitions.get(name)
                            .and_then(|definition| definition.variants.iter()
                                .find(|declaration| declaration.name == variant.name))
                            .ok_or("anonymous structural rendering lost its checked declaration")?;
                        if declaration.field_names.len() != variant.arity {
                            return Err("anonymous structural rendering lost its checked field names".into());
                        }
                        Some(declaration.field_names.clone())
                    } else { None };
                    let mut pieces = Vec::new();
                    for (offset, field) in shape.fields[variant.first_field..variant.first_field + variant.arity]
                        .iter().enumerate()
                    {
                        let rendered = self.render(field, var(&format!("field{offset}")))?;
                        pieces.push(if let Some(names) = &field_names {
                            concat(string(&format!("{}: ", names[offset])), rendered)
                        } else { rendered });
                    }
                    let shown = if variant.name.starts_with('.') { variant.name.as_str() }
                        else { variant.name.rsplit('.').next().unwrap_or(&variant.name) };
                    let rendered = if anonymous {
                        joined(".{", pieces, "}")
                    } else if variant.arity == 0 {
                        string(shown)
                    } else {
                        joined(&format!("{shown}("), pieces, ")")
                    };
                    // Only the selected constructor binds/reads its payload.
                    arms.push(arm(Pattern::Ctor {
                        name: variant.name.clone(), args: bindings("field", variant.arity),
                    }, rendered));
                }
                if arms.is_empty() {
                    return Err("structural rendering cannot inspect a fieldless nominal type".into());
                }
                Ok(block(vec![Stmt::Expr(Expr::Match { scrutinee: Box::new(var("value")), arms })]))
            }
            _ => Err("structural rendering does not support this checked operand type".into()),
        }
    }
}

fn render_operand_type(expression: &Expr, table: &TypeTable) -> Option<Type> {
    let Expr::Call { name, args } = expression else { return None };
    if !witchy_syntax::ast::is_render_intrinsic(name) { return None; }
    let [argument] = args.as_slice() else { return None };
    table.type_of(argument).and_then(typeck::ty_to_ast)
}

fn compound(ty: &Type) -> bool {
    match ty.unqualified() {
        Type::Tuple(fields) => !fields.is_empty(),
        Type::Named(name, arguments) => !(arguments.is_empty()
            && matches!(name.as_str(), "Int" | "Float" | "Bool" | "String" | "Bytes" | "Duration" | "Nil" | "Unit")),
        _ => false,
    }
}

fn string(value: &str) -> Expr { Expr::Str(value.into()) }
fn concat(left: Expr, right: Expr) -> Expr { binary(BinOp::Add, left, right) }
fn indexed(name: &str) -> Expr { Expr::Index { base: Box::new(var(name)), index: Box::new(var("cursor")) } }
fn joined(open: &str, pieces: Vec<Expr>, close: &str) -> Expr {
    let mut result = string(open);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 { result = concat(result, string(", ")); }
        result = concat(result, piece);
    }
    concat(result, string(close))
}
fn append(piece: Expr) -> Stmt {
    Stmt::Assign { name: "output".into(), value: concat(var("output"), piece), place_depth: 0 }
}
fn sequence(open: &str, close: &str, mut setup: Vec<Stmt>, element: Expr) -> Block {
    setup.extend([
        binding("output", true, string(open)),
        binding("cursor", true, Expr::Int(0)),
        Stmt::Expr(Expr::While {
            cond: Box::new(binary(BinOp::Lt, var("cursor"), var("length"))),
            body: block(vec![
                Stmt::Expr(Expr::If {
                    cond: Box::new(binary(BinOp::Gt, var("cursor"), Expr::Int(0))),
                    then_block: block(vec![append(string(", "))]), else_block: None,
                }),
                append(element),
                Stmt::Assign { name: "cursor".into(),
                    value: binary(BinOp::Add, var("cursor"), Expr::Int(1)), place_depth: 0 },
            ]),
        }),
        Stmt::Expr(concat(var("output"), string(close))),
    ]);
    block(setup)
}
