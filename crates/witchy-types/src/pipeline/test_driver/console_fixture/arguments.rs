//! Compiler-owned construction from retained entry declarations, never a
//! caller-supplied AST, provider description, constructor name or owner label.
//! Fresh ordinary source checking remains responsible for every seal and type.
use super::*;
use witchy_syntax::ast::{Module, Param};

#[derive(Clone, Copy)]
enum Leaf { Console, Arguments }

pub(super) enum Argument {
    Root { index: usize, leaf: Leaf },
    Record { constructor: String, fields: Vec<Argument> },
}

pub(super) struct Shape {
    pub(super) parameters: Vec<Param>,
    pub(super) arguments: Vec<Argument>,
}

impl Argument {
    fn expression(&self, parameters: &[Param]) -> Expr {
        match self {
            Self::Root { index, .. } => Expr::Var(parameters[*index].name.clone()),
            Self::Record { constructor, fields } => Expr::Ctor {
                name: constructor.clone(), args: fields.iter().map(|field| field.expression(parameters)).collect(),
            },
        }
    }

    fn check(&self, typed: &crate::typeck::TypedModule, entry: &str, expression: &Expr,
        parameters: &[Param]) -> Result<(), PipelineError>
    {
        let valid = match (self, expression) {
            (Self::Root { index, leaf }, Expr::Var(name)) if name == &parameters[*index].name => {
                match (leaf, typed.table().type_of(expression)) {
                    (Leaf::Console, Some(Ty::Console(rights))) => rights.write,
                    (Leaf::Arguments, Some(Ty::List(element))) => matches!(element.as_ref(), Ty::String),
                    _ => false,
                }
            }
            (Self::Record { constructor, fields }, Expr::Ctor { name, args })
                if name == &format!("{entry}.{constructor}") && fields.len() == args.len() =>
            {
                for (field, argument) in fields.iter().zip(args) {
                    field.check(typed, entry, argument, parameters)?;
                }
                true
            }
            _ => false,
        };
        if !valid {
            return Err(refused("Console fixture checked constructor, leaf type or parameter binding changed"));
        }
        Ok(())
    }
}

impl Shape {
    pub(super) fn expressions(&self) -> Vec<Expr> {
        self.arguments.iter().map(|argument| argument.expression(&self.parameters)).collect()
    }

    pub(super) fn check(&self, typed: &crate::typeck::TypedModule, entry: &str,
        arguments: &[Expr], parameters: &[Param]) -> Result<(), PipelineError>
    {
        for (expected, actual) in self.parameters.iter().zip(parameters) {
            if expected.name != actual.name || actual.convention != Convention::Let || actual.default.is_some() {
                return Err(refused("Console fixture checked entry parameter binding changed"));
            }
        }
        for (expected, actual) in self.arguments.iter().zip(arguments) {
            expected.check(typed, entry, actual, parameters)?;
        }
        Ok(())
    }
}

pub(super) fn prepare(entry: &Module, parameters: &[Param]) -> Result<Shape, PipelineError> {
    fn value(entry: &Module, ty: &Type, top: bool, roots: &mut Vec<Param>,
        active: &mut std::collections::HashSet<String>) -> Result<Argument, PipelineError>
    {
        let leaf = match ty {
            Type::Named(name, _) if name == "Console" => Some(Leaf::Console),
            Type::Named(name, args) if name == "List" && matches!(args.as_slice(),
                [Type::Named(element, args)] if element == "String" && args.is_empty()) => Some(Leaf::Arguments),
            _ => None,
        };
        if let Some(leaf) = leaf {
            let index = roots.len();
            roots.push(Param { name: format!("fixture_input_{index}"), ty: Some(ty.clone()),
                convention: Convention::Let, default: None });
            return Ok(Argument::Root { index, leaf });
        }
        let Type::Named(name, args) = ty else {
            return Err(refused("Console fixture requires concrete Console/argv leaves or source-local capability records"));
        };
        let definitions: Vec<_> = entry.items.iter().filter_map(|item| match item {
            Item::Type(definition) if &definition.name == name => Some(definition), _ => None,
        }).collect();
        let [definition] = definitions.as_slice() else {
            return Err(refused("fixture record must be declared in the retained entry source; imported seals cannot be assembled"));
        };
        if top && (!definition.is_capability || !definition.sealed) {
            return Err(refused("only a source-owned capability record may aggregate fixture roots"));
        }
        if !args.is_empty() || !witchy_syntax::ast::effective_type_def_params(definition).is_empty() {
            return Err(refused("fixture record must be concrete and non-generic"));
        }
        let [variant] = definition.variants.as_slice() else {
            return Err(refused("fixture aggregation requires one named-field record variant"));
        };
        if variant.fields.is_empty() || variant.field_names.len() != variant.fields.len() {
            return Err(refused("fixture aggregation requires a nonempty named-field record"));
        }
        if !active.insert(name.clone()) { return Err(refused("recursive fixture capability record")); }
        let fields = variant.fields.iter().map(|field| value(entry, field, false, roots, active))
            .collect::<Result<Vec<_>, _>>()?;
        active.remove(name);
        Ok(Argument::Record { constructor: variant.name.clone(), fields })
    }
    let mut roots = Vec::new();
    let mut active = std::collections::HashSet::new();
    let arguments = parameters.iter().map(|parameter| value(entry,
        parameter.ty.as_ref().ok_or_else(|| refused("fixture parameter needs an explicit type"))?,
        true, &mut roots, &mut active)).collect::<Result<Vec<_>, _>>()?;
    if !roots.iter().any(|parameter| matches!(&parameter.ty, Some(Type::Named(name, _)) if name == "Console")) {
        return Err(refused("Console fixture recipe requires a Console leaf"));
    }
    Ok(Shape { parameters: roots, arguments })
}
