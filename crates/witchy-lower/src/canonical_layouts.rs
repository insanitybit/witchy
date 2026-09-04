//! Canonical v1 layout projection from the checked source product.
//!
//! Layout authority is derived here from the typed module that owns the source
//! witness. Callers cannot provide a second layout graph and cannot make a
//! layout-bearing type appear supported by supplying a matching descriptor.

use std::collections::BTreeSet;
use std::fmt;

use witchy_syntax::ast::{self, Item, Type};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck;
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutInterner, RcHeader, ResolvedNamed, ScalarKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalLayoutError {
    MissingCheckedFunctionType(String),
    MissingCheckedExpressionType(String),
    NonConcreteExpressionType(String),
    UnsupportedType(String),
    InvalidLayout(String),
}

impl fmt::Display for CanonicalLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCheckedFunctionType(name) => {
                write!(formatter, "missing checked function type for `{name}`")
            }
            Self::MissingCheckedExpressionType(expression) => {
                write!(formatter, "missing checked expression type for `{expression}`")
            }
            Self::NonConcreteExpressionType(ty) => {
                write!(formatter, "expression has non-concrete checked type `{ty}`")
            }
            Self::UnsupportedType(ty) => {
                write!(formatter, "unsupported canonical v1 layout type `{ty}`")
            }
            Self::InvalidLayout(error) => write!(formatter, "canonical layout rejected: {error}"),
        }
    }
}

impl std::error::Error for CanonicalLayoutError {}

/// Derive the exact layout graph needed by the canonical v1 lowerer.
///
/// The source product is the only authority. Function signatures and every
/// expression in canonical functions are checked against the same typed
/// module, while layout offsets, strides, headers, and child descriptors are
/// derived by `LayoutInterner`.
pub(crate) fn derive_layout_bundle(
    source: &CanonicalCheckedSource,
) -> Result<LayoutBundle, CanonicalLayoutError> {
    let typed = source.typed_module();
    let mut projection = LayoutProjection::default();

    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }

        let signature = typed
            .table()
            .function_type(&function.name)
            .ok_or_else(|| CanonicalLayoutError::MissingCheckedFunctionType(function.name.clone()))?;
        projection.visit_type(&signature)?;

        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let Some(checked) = typed.table().type_of(expression) else {
                // Variable references are resolved through their declaration
                // and are intentionally absent from the expression side table.
                // Their binding type is visited through the initializer or
                // function signature, so this is not a missing layout fact.
                return if matches!(expression, ast::Expr::Var(_)) {
                    Ok(())
                } else {
                    Err(CanonicalLayoutError::MissingCheckedExpressionType(
                        format!("{expression:?}"),
                    ))
                };
            };
            let source_type = typeck::ty_to_ast(checked).ok_or_else(|| {
                CanonicalLayoutError::NonConcreteExpressionType(checked.to_string())
            })?;
            projection.visit_type(&source_type)
        })
        .map_err(|error| error)?;
    }

    projection.finish()
}

#[derive(Default)]
struct LayoutProjection {
    roots: Vec<Type>,
}

impl LayoutProjection {
    fn visit_type(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
        match ty {
            Type::Qualified(_, inner) => self.visit_type(inner),
            Type::Fn(parameters, result, _, _) => {
                for parameter in parameters {
                    self.visit_type(parameter)?;
                }
                self.visit_type(result)
            }
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" => Ok(()),
                _ => Err(CanonicalLayoutError::UnsupportedType(name.clone())),
            },
            Type::Named(name, arguments) if name == "List" => {
                if arguments.len() != 1
                    || !matches!(arguments[0].unqualified(), Type::Named(element, args)
                        if element == "Int" && args.is_empty())
                {
                    return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            _ => Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}"))),
        }
    }

    fn finish(self) -> Result<LayoutBundle, CanonicalLayoutError> {
        let mut interner = LayoutInterner::new();
        let mut roots = BTreeSet::new();
        for ty in self.roots {
            let root = interner
                .intern_type(&ty, &V1Resolver)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            roots.insert(root);
        }
        LayoutBundle::from_interner(&interner, roots)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
    }
}

struct V1Resolver;

impl ClosedTypeResolver for V1Resolver {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        _arguments: &[Type],
    ) -> Option<ResolvedNamed<'a>> {
        match name {
            "Int" => Some(ResolvedNamed::Scalar(ScalarKind::Int)),
            "Float" => Some(ResolvedNamed::Scalar(ScalarKind::Float)),
            "Duration" => Some(ResolvedNamed::Scalar(ScalarKind::Duration)),
            "Bool" => Some(ResolvedNamed::Scalar(ScalarKind::Bool)),
            "List" => Some(ResolvedNamed::PackedList { rc: RcHeader::Required }),
            _ => None,
        }
    }
}

fn is_canonical_source_function(module: &ast::Module, name: &str) -> bool {
    let Some(entry) = module.linked_entry.as_deref() else {
        return true;
    };
    name == entry || name.starts_with(&format!("{entry}."))
}
