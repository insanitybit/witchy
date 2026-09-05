//! Canonical v1 layout projection from the checked source product.
//!
//! Layout authority is derived here from the typed module that owns the source
//! witness. Callers cannot provide a second layout graph and cannot make a
//! layout-bearing type appear supported by supplying a matching descriptor.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{self, Item, Type, TypeDef};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck;
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutInterner, PointerSlot, RcHeader, ResolvedNamed,
    ScalarKind,
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
    let definitions = typed.module().items.iter().filter_map(|item| match item {
        Item::Type(definition) => Some((definition.name.clone(), definition.clone())),
        _ => None,
    }).collect();
    let mut projection = LayoutProjection { roots: Vec::new(), definitions };

    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }

        // Generic declarations are templates, not executable callables.  The
        // checked linker emits a concrete, monomorphized function item for
        // every resolved use site and records its finalized signature in the
        // same authoritative table.  Do not try to invent a layout for the
        // unresolved template (its `a`/`b` types have no physical layout).
        let Some(signature) = source.callable_signature(&function.name).cloned() else {
            continue;
        };
        projection.visit_type(&signature)?;

        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let Some(checked) = typed.table().type_of(expression) else {
                // Intrinsic scalar literals and variable references are
                // intentionally absent from the expression side table. Their
                // semantics are fixed by the AST constructor or checked
                // declaration and they add no layout root. A string literal is
                // memory-bearing, so materialize its semantic type and let the
                // v1 layout policy accept or reject it explicitly.
                return match expression {
                    ast::Expr::Int(_)
                    | ast::Expr::Float(_)
                    | ast::Expr::Duration(_)
                    | ast::Expr::Bool(_)
                    | ast::Expr::Var(_) => Ok(()),
                    ast::Expr::Str(_) => projection.visit_type(&Type::Named("String".into(), Vec::new())),
                    _ => Err(CanonicalLayoutError::MissingCheckedExpressionType(
                        format!("{expression:?}"),
                    )),
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
    definitions: BTreeMap<String, TypeDef>,
}

impl LayoutProjection {
    fn visit_type(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
        match ty {
            Type::Qualified(_, inner) => self.visit_type(inner),
            Type::Tuple(fields) => {
                for field in fields {
                    self.visit_type(field)?;
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Fn(parameters, result, _, _) => {
                for parameter in parameters {
                    self.visit_type(parameter)?;
                }
                self.visit_type(result)
            }
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "Console" => Ok(()),
                "String" | "Bytes" => {
                    self.roots.push(ty.clone());
                    Ok(())
                }
                _ if self.definitions.get(name).is_some_and(is_v1_packed_scalar_record) => {
                    self.roots.push(ty.clone());
                    Ok(())
                }
                _ => Err(CanonicalLayoutError::UnsupportedType(name.clone())),
            },
            Type::Named(name, arguments) if name == "List" => {
                if arguments.len() != 1 || !is_v1_list_element_type(&arguments[0]) {
                    return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
                }
                self.visit_type(&arguments[0])?;
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Named(name, arguments) if name == "Dict" => {
                if arguments.len() != 2
                    || !arguments.iter().all(|argument| {
                        matches!(
                            argument.unqualified(),
                            Type::Named(element, args)
                                if args.is_empty()
                                    && matches!(element.as_str(), "Int" | "Float" | "Duration" | "String")
                        )
                    })
                {
                    return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
                }
                for argument in arguments {
                    self.visit_type(argument)?;
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            // Iterators are GC/state-machine values, not linear-memory roots.
            // Their element type still participates in the layout graph because
            // an iterator may yield an owned linear-memory value.
            Type::Named(name, arguments) if name == "Iter" => {
                if arguments.len() != 1 {
                    return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
                }
                self.visit_type(&arguments[0])
            }
            _ => Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}"))),
        }
    }

    fn finish(self) -> Result<LayoutBundle, CanonicalLayoutError> {
        let mut interner = LayoutInterner::new();
        let mut roots = BTreeSet::new();
        for ty in self.roots {
            let root = intern_v1_layout(&mut interner, &ty, &self.definitions)?;
            roots.insert(root);
        }
        LayoutBundle::from_interner(&interner, roots)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
    }
}

fn intern_v1_layout(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    if is_byte_buffer_type(ty) {
        // String is `[u32 byte_length][UTF-8 bytes...]`. RawByte is
        // representation-only and grants no scalar or pointer authority.
        let byte = interner
            .intern_scalar(ScalarKind::RawByte)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        return interner
            .intern_runtime_buffer(byte)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    if let Type::Named(name, arguments) = ty.unqualified()
        && name == "List"
        && arguments.len() == 1
    {
        let element = intern_v1_layout(interner, &arguments[0], definitions)?;
        let element = if requires_owned_pointer_slot(&arguments[0]) {
            interner
                .intern_runtime_pointer_value_slot(element, PointerSlot::Owned)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
        } else {
            element
        };
        return interner
            .intern_packed_list(element, RcHeader::Required)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    if let Type::Named(name, arguments) = ty.unqualified()
        && name == "Dict"
        && arguments.len() == 2
    {
        let key = intern_dictionary_slot(interner, &arguments[0], definitions)?;
        let value = intern_dictionary_slot(interner, &arguments[1], definitions)?;
        return interner
            .intern_runtime_dictionary_body(key, value)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    interner
        .intern_type(ty, &V1Resolver { definitions })
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn intern_dictionary_slot(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    if is_byte_buffer_type(ty) {
        let pointee = intern_v1_layout(interner, ty, definitions)?;
        return interner
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    interner
        .intern_type(ty, &V1Resolver { definitions })
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn is_byte_buffer_type(ty: &Type) -> bool {
    matches!(
        ty.unqualified(),
        Type::Named(name, arguments)
            if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty()
    )
}

fn is_v1_list_element_type(ty: &Type) -> bool {
    match ty.unqualified() {
        Type::Named(name, arguments)
            if arguments.is_empty() && matches!(name.as_str(), "Int" | "String") => true,
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            is_v1_list_element_type(&arguments[0])
        }
        _ => false,
    }
}

fn requires_owned_pointer_slot(ty: &Type) -> bool {
    is_byte_buffer_type(ty)
        || matches!(ty.unqualified(), Type::Named(name, arguments)
            if name == "List" && arguments.len() == 1)
}

struct V1Resolver<'a> {
    definitions: &'a BTreeMap<String, TypeDef>,
}

impl ClosedTypeResolver for V1Resolver<'_> {
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
            name => self.definitions.get(name).filter(|definition| is_v1_packed_scalar_record(definition)).map(ResolvedNamed::PackedRecord),
        }
    }
}

fn is_v1_packed_scalar_record(definition: &TypeDef) -> bool {
    definition.packed
        && definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool")
        ))
}

fn is_canonical_source_function(module: &ast::Module, name: &str) -> bool {
    let Some(entry) = module.linked_entry.as_deref() else {
        return true;
    };
    name == entry || name.starts_with(&format!("{entry}."))
}

#[cfg(test)]
mod tests {
    use super::{LayoutProjection, Type};
    use witchy_wir::layout::{HeaderLayout, LayoutKind, PointerSlot, RcHeader};

    fn named(name: &str) -> Type {
        Type::Named(name.to_owned(), Vec::new())
    }

    #[test]
    fn dictionary_projection_uses_the_canonical_dense_body() {
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&Type::Named(
                "Dict".into(),
                vec![named("Int"), named("Int")],
            ))
            .expect("Dict(Int, Int) has canonical scalar lanes");
        let bundle = projection.finish().expect("dictionary layout is canonical");

        assert_eq!(bundle.roots().len(), 1);
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(
            &bundle.canonical_bytes(),
        )
        .expect("decode canonical dictionary layout");
        assert!(matches!(
            layouts.get(bundle.roots()[0]).expect("dictionary root" ).kind(),
            LayoutKind::RuntimeDictionaryBody { .. }
        ));
    }

    #[test]
    fn iterator_projection_does_not_mint_a_linear_memory_root() {
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&Type::Named("Iter".into(), vec![named("Int")]))
            .expect("Iter(Int) is a non-linear-memory value");
        let bundle = projection.finish().expect("iterator projection is canonical");

        assert!(bundle.roots().is_empty());
    }

    #[test]
    fn tuple_projection_uses_canonical_inline_fields() {
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&Type::Tuple(vec![named("Int"), named("Int")]))
            .expect("(Int, Int) has canonical scalar fields");
        let bundle = projection.finish().expect("tuple layout is canonical");

        assert_eq!(bundle.roots().len(), 1);
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(
            &bundle.canonical_bytes(),
        )
        .expect("decode canonical tuple layout");
        assert!(matches!(
            layouts.get(bundle.roots()[0]).expect("tuple root").kind(),
            LayoutKind::Tuple { .. }
        ));
    }

    #[test]
    fn bytes_projection_uses_the_canonical_byte_buffer() {
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&named("Bytes"))
            .expect("Bytes has the canonical byte-buffer representation");
        let bundle = projection.finish().expect("bytes layout is canonical");

        assert_eq!(bundle.roots().len(), 1);
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(
            &bundle.canonical_bytes(),
        )
        .expect("decode canonical bytes layout");
        assert!(matches!(
            layouts.get(bundle.roots()[0]).expect("bytes root").kind(),
            LayoutKind::RuntimeBuffer { .. }
        ));
    }

    #[test]
    fn nested_list_of_strings_uses_owned_pointer_slots_and_independent_headers() {
        let nested = Type::Named(
            "List".into(),
            vec![Type::Named("List".into(), vec![named("String")])],
        );
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&nested)
            .expect("List(List(String)) has a recursive canonical layout");
        let bundle = projection.finish().expect("nested list layout is canonical");
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(
            &bundle.canonical_bytes(),
        )
        .expect("decode canonical nested-list layout");

        let outer = bundle
            .roots()
            .iter()
            .copied()
            .find(|root| {
                let Some(descriptor) = layouts.get(*root) else { return false };
                let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return false };
                let Some(element) = layouts.get(*element) else { return false };
                let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } = element.kind() else { return false };
                matches!(layouts.get(*pointee).map(|layout| layout.kind()), Some(LayoutKind::PackedList { .. }))
            })
            .expect("outer list root points to an inner list");
        let LayoutKind::PackedList { element: outer_element, .. } = layouts.get(outer).unwrap().kind() else {
            panic!("outer root must be a packed list");
        };
        let LayoutKind::RuntimePointerValueSlot { pointee: inner, semantics: PointerSlot::Owned } =
            layouts.get(*outer_element).unwrap().kind()
        else {
            panic!("outer list elements must own inner-list pointers");
        };
        let LayoutKind::PackedList { element: inner_element, .. } = layouts.get(*inner).unwrap().kind() else {
            panic!("inner pointee must be a packed list");
        };
        let LayoutKind::RuntimePointerValueSlot { pointee: string, semantics: PointerSlot::Owned } =
            layouts.get(*inner_element).unwrap().kind()
        else {
            panic!("inner list elements must own String pointers");
        };
        assert!(matches!(layouts.get(*string).unwrap().kind(), LayoutKind::RuntimeBuffer { .. }));

        for list in [outer, *inner] {
            let HeaderLayout::PackedList {
                length_offset,
                capacity_offset,
                rc: RcHeader::Required,
                ..
            } = layouts.get(list).unwrap().header()
            else {
                panic!("each nested list must retain its own packed-list header");
            };
            assert_ne!(length_offset, capacity_offset);
        }
    }
}
