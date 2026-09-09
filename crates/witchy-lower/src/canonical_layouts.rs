//! Canonical v1 layout projection from the checked source product.
//!
//! Layout authority is derived here from the typed module that owns the source
//! witness. Callers cannot provide a second layout graph and cannot make a
//! layout-bearing type appear supported by supplying a matching descriptor.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};
use witchy_types::canonical_aggregates::{aggregate_key, concrete_aggregate_identity, is_scalar_type, is_v1_list_element_type, scalar_aggregate_shape, shape_from_definition};
use witchy_syntax::ast::{self, Item, Type, TypeDef};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck;
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, PointerSlot, RcHeader,
    ResolvedNamed, ScalarKind,
};

/// The source-derived storage family for one `fn` signature.  The callable
/// object remains the uniform value ABI; its second field owns one tagged
/// environment record.  Variant zero is the empty environment used by named
/// function adapters.  Every other variant is one closure witness, ordered by
/// that witness manifest rather than by a source spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosureFamilyLayout {
    pub closure: LayoutId,
    pub environment: LayoutId,
    pub variants: Vec<ClosureEnvironmentVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosureEnvironmentVariant {
    pub witness: Option<u32>,
    pub fields: Vec<Type>,
    pub first_field: usize,
}

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
    project_layouts(source)?.finish_with_families().map(|(bundle, _)| bundle)
}

/// The closure layout of every `fn` type the canonical functions reach: a
/// heap record of the code slot and the environment's `Int` capture slots.
/// Derived by the same projection the bundle comes from, so the ids agree
/// with the bundle's roots (a layout id is its content digest).
pub(crate) fn closure_layouts(
    source: &CanonicalCheckedSource,
) -> Result<HashMap<Type, witchy_wir::layout::LayoutId>, CanonicalLayoutError> {
    closure_families(source).map(|families| families
        .into_iter()
        .map(|(signature, family)| (signature, family.closure))
        .collect())
}

/// Derive the complete closure-family storage schema from the same checked
/// source product as the canonical layout bundle.  This is intentionally not
/// a caller-provided record schema: witness order, checked capture types, and
/// every physical child layout are all reconstructed here.
pub(crate) fn closure_families(
    source: &CanonicalCheckedSource,
) -> Result<HashMap<Type, ClosureFamilyLayout>, CanonicalLayoutError> {
    project_layouts(source)?.finish_with_families().map(|(_, families)| families)
}

/// The identity of a `fn` type for closure purposes: the declared
/// conventions spelled out (an empty list means every parameter is `let`)
/// and the callable qualifiers dropped, so `fn(Int) -> Int` written at a
/// parameter, a lambda, and a declaration share one closure layout.
pub(crate) fn closure_key(ty: &Type) -> Type {
    match ty.unqualified() {
        Type::Fn(parameters, result, conventions, _) => {
            let conventions = if conventions.is_empty() {
                vec![ast::Convention::Let; parameters.len()]
            } else {
                conventions.clone()
            };
            Type::Fn(
                parameters.iter().map(closure_key).collect(),
                Box::new(closure_key(result)),
                conventions,
                ast::CallableQualifiers::default(),
            )
        }
        Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(closure_key).collect()),
        Type::Tuple(items) => Type::Tuple(items.iter().map(closure_key).collect()),
        other => other.clone(),
    }
}

/// The exact capture variants of each `fn` family, ordered by the checked
/// closure witness manifest.  The manifest is the source authority for both
/// tag values and capture types.
fn closure_capture_variants(
    source: &CanonicalCheckedSource,
) -> Result<HashMap<Type, Vec<(u32, Vec<Type>)>>, CanonicalLayoutError> {
    let mut variants = HashMap::new();
    for (index, witness) in source.closure_witness().entries().iter().enumerate() {
        let Some(fn_type) = typeck::ty_to_ast(witness.signature()) else { continue };
        let index = u32::try_from(index).map_err(|_| CanonicalLayoutError::InvalidLayout("closure witness index overflow".into()))?;
        variants.entry(closure_key(&fn_type)).or_insert_with(Vec::new).push((
            index,
            witness.captures().iter().map(|capture| capture.checked_type().clone()).collect(),
        ));
    }
    Ok(variants)
}

/// Give every `fn` type a distinct slot count, so distinct signatures never
/// share a closure layout: in a fixed order, each type takes at least one
/// slot more than the type before it.
pub(crate) fn closure_slot_assignment(mut closures: Vec<(Type, usize)>) -> Vec<(Type, usize)> {
    closures.sort_by_key(|(ty, _)| format!("{ty:?}"));
    closures.dedup_by_key(|(ty, _)| format!("{ty:?}"));
    let mut previous: Option<usize> = None;
    closures
        .into_iter()
        .map(|(ty, slots)| {
            let slots = match previous {
                Some(previous) => slots.max(previous + 1),
                None => slots,
            };
            previous = Some(slots);
            (ty, slots)
        })
        .collect()
}

/// A closure record: slot 0 holds the code, slots 1..=`slots` the `Int`
/// captures.
pub(crate) fn intern_closure_layout(
    interner: &mut LayoutInterner,
    slots: usize,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    let code = interner
        .intern_scalar(ScalarKind::Callable)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    let int = interner
        .intern_scalar(ScalarKind::Int)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    let mut children = vec![code];
    children.extend(std::iter::repeat(int).take(slots));
    interner
        .intern_packed_record(children)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn project_layouts(
    source: &CanonicalCheckedSource,
) -> Result<LayoutProjection, CanonicalLayoutError> {
    let typed = source.typed_module();
    let scope = witchy_types::pipeline::canonical_scope(typed.module());
    // The checker spells an entry-module type as `entry.Name` while the
    // definition keeps its bare source name; key both spellings.
    let entry_module = typed.module().linked_entry.as_deref();
    let definitions = typed.module().items.iter().filter_map(|item| match item {
        Item::Type(definition) => Some(definition),
        _ => None,
    }).flat_map(|definition| {
        let qualified = entry_module
            .filter(|_| !definition.name.contains('.'))
            .map(|entry| (format!("{entry}.{}", definition.name), definition.clone()));
        std::iter::once((definition.name.clone(), definition.clone())).chain(qualified)
    }).collect();
    let mut projection = LayoutProjection {
        roots: Vec::new(),
        definitions,
        closures: BTreeMap::new(),
        capture_variants: closure_capture_variants(source)?,
        visiting: HashSet::new(),
        visited: HashSet::new(),
    };

    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) {
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
        // The declaration's own `fn` type is how it is called, not a value:
        // its parameters and result are roots, the type itself is not a
        // closure. A `fn` type nested in them is.
        match &signature {
            Type::Fn(parameters, result, _, _) => {
                for parameter in parameters {
                    projection.visit_type(parameter)?;
                }
                projection.visit_type(result)?;
            }
            other => projection.visit_type(other)?,
        }

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

    Ok(projection)
}

#[derive(Default)]
struct LayoutProjection {
    roots: Vec<Type>,
    definitions: BTreeMap<String, TypeDef>,
    /// Every `fn` type reached, keyed by its rendering, with its exact
    /// source-witness environment variants.
    closures: BTreeMap<String, (Type, Vec<(u32, Vec<Type>)>)>,
    capture_variants: HashMap<Type, Vec<(u32, Vec<Type>)>>,
    visiting: HashSet<Type>,
    visited: HashSet<Type>,
}

impl LayoutProjection {
    fn visit_type(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
        let key = canonical_layout_type(ty, &self.definitions);
        if self.visited.contains(&key) || !self.visiting.insert(key.clone()) {
            return Ok(());
        }
        let result = self.visit_type_fields(&key);
        self.visiting.remove(&key);
        if result.is_ok() {
            self.visited.insert(key);
        }
        result
    }

    fn visit_type_fields(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
        // Checked capability brands and their optional form are externrefs,
        // never heap aggregates with pointer-bearing payload slots.
        if witchy_types::canonical_capabilities::representation(ty, &self.definitions).is_some() {
            return Ok(());
        }
        // A dictionary is laid out as its canonical tuple of the key list
        // and the value list, before any aggregate rule sees the name.
        if let Some(tuple) = witchy_types::canonical_aggregates::dictionary_as_tuple(ty) {
            return self.visit_type(&tuple);
        }
        match ty {
            Type::Qualified(_, inner) => self.visit_type(inner),
            // A tuple is an anonymous heap aggregate; its reference fields
            // are roots of their own.
            Type::Tuple(fields) if scalar_aggregate_shape(ty, &self.definitions).is_some() => {
                for field in fields {
                    if !is_scalar_type(field) {
                        self.visit_type(field)?;
                    }
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Tuple(fields) => {
                for field in fields {
                    self.visit_type(field)?;
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            // A `fn` type's values are closure records; the type's
            // parameters and result carry their own roots.
            Type::Fn(parameters, result, _, _) => {
                let fn_type = closure_key(ty);
                let variants = self.capture_variants.get(&fn_type).cloned().unwrap_or_default();
                self.closures.insert(format!("{fn_type:?}"), (fn_type, variants));
                for parameter in parameters {
                    self.visit_type(parameter)?;
                }
                self.visit_type(result)
            }
            // A heap aggregate (record, sum, scalar-or-reference
            // `Option`/`Result`) is a root; its reference fields are roots of
            // their own.
            Type::Named(..)
                if scalar_aggregate_shape(ty, &self.definitions).is_some_and(|shape| !shape.packed) =>
            {
                let shape = scalar_aggregate_shape(ty, &self.definitions).expect("checked above");
                for field in &shape.fields {
                    if !is_scalar_type(field) {
                        self.visit_type(field)?;
                    }
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            // A `Dir` token carries its rights as type arguments; like every
            // capability it has no layout (an externref minted by the host).
            Type::Named(name, _) if name == "Dir" => Ok(()),
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "Console" | "Env" => Ok(()),
                "String" | "Bytes" => {
                    self.roots.push(ty.clone());
                    Ok(())
                }
                _ if scalar_aggregate_shape(ty, &self.definitions).is_some() => {
                    self.roots.push(ty.clone());
                    Ok(())
                }
                _ => Err(CanonicalLayoutError::UnsupportedType(name.clone())),
            },
            // A scalar `Option`/`Result` instantiation is a tagged scalar
            // aggregate behind one reference.
            Type::Named(name, _)
                if matches!(name.as_str(), "Option" | "Result")
                    && scalar_aggregate_shape(ty, &self.definitions).is_some() =>
            {
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Named(name, arguments) if name == "List" => {
                if arguments.len() != 1 || !is_v1_list_element_type(&arguments[0], &self.definitions) {
                    return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
                }
                self.visit_type(&arguments[0])?;
                self.roots.push(ty.clone());
                Ok(())
            }
            // A `Dict` with other than two arguments has no canonical form.
            Type::Named(name, _) if name == "Dict" => {
                Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")))
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
        self.finish_with_families().map(|(bundle, _)| bundle)
    }

    fn finish_with_families(self) -> Result<(LayoutBundle, HashMap<Type, ClosureFamilyLayout>), CanonicalLayoutError> {
        let recursive_types = recursive_heap_types(&self.roots, &self.definitions);
        let mut interner = LayoutInterner::new();
        let mut roots = BTreeSet::new();
        // Reserve every closure environment identity, and its two-field
        // closure record, before constructing any body. A capture may itself
        // be a function from the same family (or a mutually recursive
        // family), so constructing one environment before reserving the
        // others would recurse through physical bodies and lose the checked
        // callable identity.
        let closure_specs = self.closures.into_values().collect::<Vec<_>>();
        let mut closure_environments = HashMap::new();
        for (signature, _) in &closure_specs {
            let key = format!("{:?}", closure_key(signature));
            let mut digest = Sha256::new();
            digest.update(b"witchy/canonical-layout/closure-environment-v1\0");
            digest.update((key.len() as u64).to_le_bytes());
            digest.update(key.as_bytes());
            let environment = interner.intern_opaque_nominal(digest.finalize().into())
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            closure_environments.insert(signature.clone(), environment);
        }
        let mut closure_layouts = HashMap::new();
        for (signature, _) in &closure_specs {
            let environment = closure_environments[signature];
            let closure = intern_closure_layout_with_environment(&mut interner, environment)?;
            closure_layouts.insert(signature.clone(), closure);
        }
        // Reserve all nominal cycle breakers before constructing any body.
        // A structural root such as List(Tree) may be visited before Tree;
        // following its pointee must not recursively rebuild that list while
        // the list's content-derived descriptor is still being constructed.
        let mut recursive = recursive_types.iter().collect::<Vec<_>>();
        recursive.sort_by_key(|ty| format!("{ty:?}"));
        for ty in &recursive {
            intern_recursive_nominal(&mut interner, ty)?;
        }
        for ty in recursive {
            let nominal = intern_recursive_nominal(&mut interner, ty)?;
            let body = intern_v1_layout_fields(
                &mut interner, ty, &self.definitions, &recursive_types,
                &mut HashSet::new(), &closure_layouts,
            )?;
            interner.bind_nominal(nominal, body)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        }
        for ty in self.roots {
            let root = intern_v1_layout(
                &mut interner, &ty, &self.definitions, &recursive_types, &closure_layouts,
            )?;
            roots.insert(root);
        }
        let mut families = HashMap::new();
        for (signature, variants) in closure_specs {
            let environment = closure_environments[&signature];
            let (environment_body, variants) = intern_closure_environment(
                &mut interner,
                variants,
                &self.definitions,
                &recursive_types,
                &closure_layouts,
            )?;
            interner.bind_nominal(environment, environment_body)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let closure = closure_layouts[&signature];
            roots.insert(environment);
            roots.insert(closure);
            families.insert(signature, ClosureFamilyLayout { closure, environment, variants });
        }
        let bundle = LayoutBundle::from_interner(&interner, roots)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        Ok((bundle, families))
    }
}

fn intern_closure_environment(
    interner: &mut LayoutInterner,
    witnesses: Vec<(u32, Vec<Type>)>,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
) -> Result<(LayoutId, Vec<ClosureEnvironmentVariant>), CanonicalLayoutError> {
    let tag = interner
        .intern_scalar(ScalarKind::Int)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    let mut children = vec![tag];
    // Variant zero is reserved for named-function adapters.  A lambda's tag
    // is its index in this vector, never a declaration name or code pointer.
    let mut variants = vec![ClosureEnvironmentVariant {
        witness: None,
        fields: Vec::new(),
        first_field: 1,
    }];
    for (witness, fields) in witnesses {
        let first_field = children.len();
        for field in &fields {
            if witchy_types::canonical_capabilities::representation(field, definitions).is_some() {
                return Err(CanonicalLayoutError::UnsupportedType(format!(
                    "closure capture requires unavailable typed storage: {field:?}"
                )));
            }
            let child = if let Some(scalar) = scalar_kind_of(field) {
                interner
                    .intern_scalar(scalar)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            } else {
                let pointee = intern_v1_layout(
                    interner, field, definitions, recursive_types, closure_layouts,
                )?;
                interner
                    .intern_runtime_pointer_value_slot(pointee, PointerSlot::OwnedNullable)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            };
            children.push(child);
        }
        variants.push(ClosureEnvironmentVariant { witness: Some(witness), fields, first_field });
    }
    let layout = interner
        .intern_packed_record(children)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    Ok((layout, variants))
}

fn intern_closure_layout_with_environment(
    interner: &mut LayoutInterner,
    environment: LayoutId,
) -> Result<LayoutId, CanonicalLayoutError> {
    let code = interner
        .intern_scalar(ScalarKind::Callable)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    let environment = interner
        .intern_runtime_pointer_value_slot(environment, PointerSlot::Owned)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    interner
        .intern_packed_record(vec![code, environment])
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn intern_v1_layout(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    intern_v1_layout_checked(
        interner, ty, definitions, recursive_types, &mut HashSet::new(), closure_layouts,
    )
}

fn intern_v1_layout_checked(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    active: &mut HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    let key = canonical_layout_type(ty, definitions);
    if matches!(key.unqualified(), Type::Fn(..)) {
        return closure_layouts.get(&closure_key(&key)).copied().ok_or_else(|| {
            CanonicalLayoutError::UnsupportedType(format!("unregistered closure family {key:?}"))
        });
    }
    if recursive_types.contains(&key) {
        // Bodies are bound separately by finish. References use only the
        // reserved identity, regardless of whether its body is already bound.
        return intern_recursive_nominal(interner, &key);
    }
    if !active.insert(key.clone()) {
        return Err(CanonicalLayoutError::UnsupportedType(format!(
            "recursive nominal layout {key:?} requires nullable owned-slot lowering"
        )));
    }
    let result = intern_v1_layout_fields(
        interner, &key, definitions, recursive_types, active, closure_layouts,
    );
    active.remove(&key);
    result
}

fn intern_v1_layout_fields(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    active: &mut HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
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
        let element = intern_v1_layout_checked(
            interner, &arguments[0], definitions, recursive_types, active, closure_layouts,
        )?;
        let element = if !is_scalar_type(&arguments[0]) {
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
    if let Some(tuple) = witchy_types::canonical_aggregates::dictionary_as_tuple(ty) {
        return intern_v1_layout_checked(
            interner, &tuple, definitions, recursive_types, active, closure_layouts,
        );
    }
    if let Some(shape) = scalar_aggregate_shape(ty, definitions)
        && !shape.packed
    {
        // A heap aggregate is a fixed record of its slots: a scalar in place,
        // a string or list behind an owned pointer slot. Built directly: the
        // interner's type resolver cannot place a reference inside a record.
        let mut children = Vec::with_capacity(shape.fields.len());
        for field in &shape.fields {
            let child = if let Some(scalar) = scalar_kind_of(field) {
                interner
                    .intern_scalar(scalar)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            } else {
                let pointee = intern_v1_layout_checked(
                    interner, field, definitions, recursive_types, active, closure_layouts,
                )?;
                interner
                    .intern_runtime_pointer_value_slot(
                        pointee,
                        if shape.tagged {
                            PointerSlot::OwnedNullable
                        } else {
                            PointerSlot::Owned
                        },
                    )
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            };
            children.push(child);
        }
        return interner
            .intern_packed_record(children)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    interner
        .intern_type(ty, &V1Resolver::new(definitions, ty))
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn intern_recursive_nominal(
    interner: &mut LayoutInterner,
    ty: &Type,
) -> Result<LayoutId, CanonicalLayoutError> {
    let key = aggregate_key(ty).ok_or_else(|| {
        CanonicalLayoutError::UnsupportedType(format!("recursive nominal layout {ty:?}"))
    })?;
    let mut digest = Sha256::new();
    digest.update(b"witchy/canonical-layout/recursive-nominal-v1\0");
    digest.update((key.len() as u64).to_le_bytes());
    digest.update(key.as_bytes());
    interner
        .intern_opaque_nominal(digest.finalize().into())
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

/// Derive the catalog's exact nominal ID without granting any layout authority.
/// Consumers must still find this ID and its binding in the checked catalog.
pub(crate) fn recursive_nominal_layout_id(ty: &Type) -> Result<LayoutId, CanonicalLayoutError> {
    intern_recursive_nominal(&mut LayoutInterner::new(), ty)
}

/// Like [`recursive_nominal_layout_id`], but derives the opaque identity from
/// the checked aggregate identity rather than a declaration-local spelling.
/// The catalog uses this normalization for every graph edge; lowering must use
/// this companion when it maps a checked aggregate source type back to the
/// catalog root.
pub(crate) fn recursive_nominal_layout_id_for_shape(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<LayoutId, CanonicalLayoutError> {
    let identity = concrete_aggregate_identity(ty, definitions).ok_or_else(|| {
        CanonicalLayoutError::UnsupportedType(format!("recursive nominal layout {ty:?}"))
    })?;
    intern_recursive_nominal(&mut LayoutInterner::new(), &identity)
}

/// Aggregate aliases and recursively substituted fields must enter the layout
/// graph in the exact checked identity domain.  In particular, an entry root
/// `main.Stack(Int)` and the `Stack(a)` field substituted from its definition
/// become one `main.Stack(Int)` node before cycle detection or nominal hashing.
/// Non-aggregate types retain their checked structural spelling.
fn canonical_layout_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Type {
    concrete_aggregate_identity(ty, definitions).unwrap_or_else(|| ty.unqualified().clone())
}

/// Find the reachable user-defined heap types that are actually on a source
/// cycle. Acyclic heap records retain their existing concrete content IDs;
/// only cycle participants need an opaque identity reserved before their body.
/// The graph is completed before cycle membership is classified, so the result
/// cannot depend on which source root happened to be visited first.
fn recursive_heap_types(roots: &[Type], definitions: &BTreeMap<String, TypeDef>) -> HashSet<Type> {
    let mut graph: HashMap<Type, HashSet<Type>> = HashMap::new();
    let mut pending = Vec::new();
    let mut reachable = HashSet::new();
    for root in roots {
        collect_heap_targets(root, definitions, &mut pending);
    }
    while let Some(node) = pending.pop() {
        let node = canonical_layout_type(&node, definitions);
        if !reachable.insert(node.clone()) || !is_heap_user_type(&node, definitions) {
            continue;
        }
        let mut edges = HashSet::new();
        if let Some(shape) = scalar_aggregate_shape(&node, definitions) {
            for field in shape.fields.iter().filter(|field| !is_scalar_type(field)) {
                collect_heap_targets(field, definitions, &mut edges);
            }
        }
        pending.extend(edges.iter().cloned());
        graph.insert(node, edges);
    }

    graph
        .keys()
        .filter(|node| graph_reaches(&graph, node, node, &mut HashSet::new()))
        .cloned()
        .collect()
}

fn collect_heap_targets(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    targets: &mut impl Extend<Type>,
) {
    let normalized = canonical_layout_type(ty, definitions);
    let ty = normalized.unqualified();
    if is_heap_user_type(ty, definitions) {
        targets.extend(std::iter::once(ty.clone()));
        return;
    }
    if let Some(tuple) = witchy_types::canonical_aggregates::dictionary_as_tuple(ty) {
        collect_heap_targets(&tuple, definitions, targets);
        return;
    }
    if let Some(shape) = scalar_aggregate_shape(ty, definitions) {
        for field in shape.fields.iter().filter(|field| !is_scalar_type(field)) {
            collect_heap_targets(field, definitions, targets);
        }
        return;
    }
    match ty {
        Type::Qualified(_, inner) => collect_heap_targets(inner, definitions, targets),
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            collect_heap_targets(&arguments[0], definitions, targets)
        }
        Type::Tuple(fields) => {
            for field in fields {
                collect_heap_targets(field, definitions, targets);
            }
        }
        _ => {}
    }
}

fn graph_reaches(
    graph: &HashMap<Type, HashSet<Type>>,
    current: &Type,
    target: &Type,
    visited: &mut HashSet<Type>,
) -> bool {
    graph.get(current).is_some_and(|edges| {
        edges.iter().any(|child| {
            child == target
                || (visited.insert(child.clone()) && graph_reaches(graph, child, target, visited))
        })
    })
}

fn is_heap_user_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    let Type::Named(name, _) = ty.unqualified() else {
        return false;
    };
    definitions.contains_key(name)
        && scalar_aggregate_shape(ty, definitions).is_some_and(|shape| !shape.packed)
}

fn intern_dictionary_slot(
    interner: &mut LayoutInterner,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLayoutError> {
    if is_byte_buffer_type(ty) {
        let pointee = intern_v1_layout(interner, ty, definitions, recursive_types, &HashMap::new())?;
        return interner
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()));
    }
    interner
        .intern_type(ty, &V1Resolver::new(definitions, ty))
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

fn scalar_kind_of(ty: &Type) -> Option<ScalarKind> {
    match ty.unqualified() {
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" => Some(ScalarKind::Int),
            "Float" => Some(ScalarKind::Float),
            "Duration" => Some(ScalarKind::Duration),
            "Bool" => Some(ScalarKind::Bool),
            _ => None,
        },
        _ => None,
    }
}

fn is_byte_buffer_type(ty: &Type) -> bool {
    matches!(
        ty.unqualified(),
        Type::Named(name, arguments)
            if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty()
    )
}


struct V1Resolver<'a> {
    definitions: &'a BTreeMap<String, TypeDef>,
    /// Scalar aggregates viewed as packed one-variant records: the interner
    /// accepts only that definition form for a fixed field layout, and a
    /// heap record or a tagged sum has the same slot layout behind its
    /// reference. Keyed by [`aggregate_key`]; the view is named as the
    /// resolver will be asked for it.
    packed_views: BTreeMap<String, TypeDef>,
}

impl<'a> V1Resolver<'a> {
    fn new(definitions: &'a BTreeMap<String, TypeDef>, ty: &Type) -> Self {
        let mut packed_views = BTreeMap::new();
        for (name, definition) in definitions {
            if definition.packed {
                continue;
            }
            if let Some(shape) = shape_from_definition(definition, definitions) {
                let mut view = shape.packed_view();
                view.name = name.clone();
                packed_views.insert(name.clone(), view);
            }
        }
        collect_builtin_views(ty, definitions, &mut packed_views);
        Self { definitions, packed_views }
    }
}

/// Views for every scalar `Option`/`Result` instantiation inside `ty`.
fn collect_builtin_views(ty: &Type, definitions: &BTreeMap<String, TypeDef>, views: &mut BTreeMap<String, TypeDef>) {
    match ty {
        Type::Qualified(_, inner) => collect_builtin_views(inner, definitions, views),
        Type::Named(name, arguments) => {
            if matches!(name.as_str(), "Option" | "Result")
                && let Some(key) = aggregate_key(ty)
                && let Some(shape) = scalar_aggregate_shape(ty, definitions)
            {
                // The interner resolves the instantiation by its key as a
                // nominal without arguments (`Option(Int)`), so the view
                // carries that name.
                views.entry(key.clone()).or_insert_with(|| {
                    let mut view = shape.packed_view();
                    view.name = key;
                    view
                });
            }
            for argument in arguments {
                collect_builtin_views(argument, definitions, views);
            }
        }
        Type::Tuple(fields) => {
            for field in fields {
                collect_builtin_views(field, definitions, views);
            }
        }
        _ => {}
    }
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
            name => {
                // A keyed instantiation (`Option(Int)`) arrives as its own
                // name; a user type arrives bare.
                let key = aggregate_key(&Type::Named(name.to_owned(), _arguments.to_vec()));
                self.packed_views
                    .get(name)
                    .or_else(|| key.as_deref().and_then(|key| self.packed_views.get(key)))
                    .or_else(|| self.definitions.get(name).filter(|definition| is_v1_scalar_record(definition)))
                    .map(ResolvedNamed::PackedRecord)
            }
        }
    }
}

/// A one-variant record whose fields are all scalars. Declared `packed` it is
/// an inline aggregate; otherwise it is an immutable reference-counted heap
/// aggregate with the same field layout.
pub(crate) fn is_v1_scalar_record(definition: &TypeDef) -> bool {
    definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool")
        ))
}

#[cfg(test)]
fn is_v1_packed_scalar_record(definition: &TypeDef) -> bool {
    definition.packed
        && definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool")
        ))
}

#[cfg(test)]
mod tests {
    use super::{LayoutProjection, Type};
    use witchy_wir::layout::{HeaderLayout, LayoutKind, PointerSlot, RcHeader};

    fn named(name: &str) -> Type {
        Type::Named(name.to_owned(), Vec::new())
    }

    fn heap_definition(name: &str, fields: Vec<Type>) -> witchy_syntax::ast::TypeDef {
        let mut definition = witchy_types::canonical_aggregates::ScalarAggregateShape {
            name: name.into(), fields, variants: Vec::new(), tagged: false, packed: false,
        }.packed_view();
        definition.packed = false;
        definition
    }

    fn generic_heap_definition(name: &str, parameters: &[&str], fields: Vec<Type>) -> witchy_syntax::ast::TypeDef {
        let mut definition = heap_definition(name, fields);
        definition.params = parameters.iter().map(|parameter| (*parameter).to_owned()).collect();
        definition
    }

    #[test]
    fn recursive_container_layouts_keep_structural_lists_and_stable_bindings() {
        fn bundle(list_first: bool) -> witchy_wir::layout::LayoutBundle {
            let list = Type::Named("List".into(), vec![named("Tree")]);
            let mut projection = LayoutProjection::default();
            projection.definitions.insert("Tree".into(), heap_definition("Tree", vec![list.clone()]));
            let order = if list_first { vec![list, named("Tree")] } else { vec![named("Tree"), list] };
            for ty in order {
                projection.visit_type(&ty).expect("finite container source graph");
            }
            projection.finish().expect("container recursion binds without eager descent")
        }
        let first = bundle(true);
        assert_eq!(first.canonical_bytes(), bundle(false).canonical_bytes());
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&first.canonical_bytes())
            .expect("closed recursive container catalog");
        let list = first.roots().iter().copied().find(|root| {
            matches!(layouts.get(*root).map(|layout| layout.kind()), Some(LayoutKind::PackedList { .. }))
        }).expect("list retains its structural descriptor");
        let LayoutKind::PackedList { element, .. } = layouts.get(list).unwrap().kind() else { unreachable!() };
        let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } =
            layouts.get(*element).unwrap().kind() else { panic!("list elements remain non-null owned references") };
        let body = layouts.nominal_target(*pointee).expect("nominal child is bound");
        let LayoutKind::PackedRecord { fields } = layouts.get(body).unwrap().kind() else { panic!("Tree body") };
        assert!(matches!(layouts.get(fields[0]).unwrap().kind(),
            LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } if *pointee == list));
    }

    #[test]
    fn recursive_container_layouts_bind_mutual_bodies_before_structural_roots() {
        let list_b = Type::Named("List".into(), vec![named("B")]);
        let mut projection = LayoutProjection::default();
        projection.definitions.insert("A".into(), heap_definition("A", vec![list_b.clone()]));
        projection.definitions.insert("B".into(), heap_definition("B", vec![named("A"), list_b]));
        projection.visit_type(&named("A")).expect("mutual source graph");
        let bundle = projection.finish().expect("a body never descends into another nominal body");
        witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("all mutual bindings are closed");
    }

    #[test]
    fn recursive_layout_projection_roundtrips_finite_nominal_binding() {
        let mut projection = LayoutProjection::default();
        projection.definitions.insert("Tree".into(), heap_definition("Tree", vec![named("Int"), named("Tree")]));
        projection.visit_type(&named("Tree")).expect("source root graph is finite");
        assert_eq!(projection.roots, vec![named("Tree")]);
        let bundle = projection.finish().expect("recursive source layout is finite");
        let bytes = bundle.canonical_bytes();
        let (decoded, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bytes)
            .expect("recursive nominal bundle decodes canonically");
        assert_eq!(decoded.canonical_bytes(), bytes);
        let root = decoded.roots()[0];
        let target = layouts.nominal_target(root).expect("recursive root has one binding");
        let LayoutKind::PackedRecord { fields } = layouts.get(target).expect("bound body").kind() else {
            panic!("recursive nominal target must be a packed record");
        };
        assert_eq!(fields.len(), 2);
        let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } =
            layouts.get(fields[1]).expect("recursive child slot").kind()
        else {
            panic!("recursive child must remain an owned pointer slot");
        };
        assert_eq!(*pointee, root, "recursive child slot retains the nominal identity");
    }

    #[test]
    fn mutual_recursive_nominals_are_stable_across_root_order() {
        fn bundle(order: &[&str]) -> witchy_wir::layout::LayoutBundle {
            let mut projection = LayoutProjection::default();
            projection.definitions.insert("A".into(), heap_definition("A", vec![named("B")]));
            projection.definitions.insert("B".into(), heap_definition("B", vec![named("A")]));
            for name in order {
                projection.visit_type(&named(name)).expect("mutual root graph is finite");
            }
            projection.finish().expect("mutual recursive layout is finite")
        }

        let first = bundle(&["A", "B"]);
        let second = bundle(&["B", "A"]);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.roots(), second.roots());

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&first.canonical_bytes())
            .expect("mutual nominal bundle decodes canonically");
        assert_eq!(first.roots().len(), 2);
        for root in first.roots() {
            let target = layouts.nominal_target(*root).expect("mutual root has one binding");
            let LayoutKind::PackedRecord { fields } = layouts.get(target).expect("bound mutual body").kind() else {
                panic!("mutual nominal target must be a packed record");
            };
            let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } =
                layouts.get(fields[0]).expect("mutual child slot").kind()
            else {
                panic!("mutual child must remain an owned pointer slot");
            };
            assert!(first.roots().contains(pointee), "mutual child retains a nominal root identity");
        }
    }

    #[test]
    fn recursive_nominal_scc_membership_is_stable_for_branching_cycles() {
        fn bundle(order: &[&str]) -> witchy_wir::layout::LayoutBundle {
            let mut projection = LayoutProjection::default();
            projection.definitions.insert("A".into(), heap_definition("A", vec![named("B"), named("C")]));
            projection.definitions.insert("B".into(), heap_definition("B", vec![named("A")]));
            projection.definitions.insert("C".into(), heap_definition("C", vec![named("B")]));
            for name in order {
                projection.visit_type(&named(name)).expect("branching cycle is finite");
            }
            projection.finish().expect("branching recursive layout is finite")
        }

        let first = bundle(&["A", "B", "C"]);
        let second = bundle(&["C", "B", "A"]);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.roots(), second.roots());

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&first.canonical_bytes())
            .expect("branching nominal bundle decodes canonically");
        assert_eq!(first.roots().len(), 3);
        assert!(first.roots().iter().all(|root| layouts.nominal_target(*root).is_some()));
    }

    #[test]
    fn recursive_layout_projection_keeps_nonrecursive_nested_aggregates() {
        let mut projection = LayoutProjection::default();
        for (name, field) in [("A", "B"), ("B", "C"), ("C", "String")] {
            projection.definitions.insert(name.into(), heap_definition(name, vec![named(field)]));
        }
        projection.visit_type(&named("A")).expect("nested source types are supported");
        projection.visit_type(&named("A")).expect("revisiting a root is harmless");
        assert_eq!(projection.roots.len(), 4);
        projection.finish().expect("acyclic owned fields retain concrete layouts");
    }

    #[test]
    fn dictionary_projection_is_its_tuple_of_two_lists() {
        let mut projection = LayoutProjection::default();
        projection
            .visit_type(&Type::Named(
                "Dict".into(),
                vec![named("Int"), named("Int")],
            ))
            .expect("Dict(Int, Int) has a canonical tuple form");
        let bundle = projection.finish().expect("dictionary layout is canonical");

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(
            &bundle.canonical_bytes(),
        )
        .expect("decode canonical dictionary layout");
        let kinds = bundle
            .roots()
            .iter()
            .map(|root| layouts.get(*root).expect("dictionary root").kind().clone())
            .collect::<Vec<_>>();
        // The dictionary is a heap record of two owned list slots (the keys
        // and the values, in insertion order); the legacy dense body never
        // reaches the canonical bundle.
        assert!(kinds.iter().any(|kind| matches!(kind, LayoutKind::PackedRecord { fields } if fields.len() == 2)));
        assert!(kinds.iter().any(|kind| matches!(kind, LayoutKind::PackedList { .. })));
        assert!(!kinds.iter().any(|kind| matches!(kind, LayoutKind::RuntimeDictionaryBody { .. })));
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
        // A tuple is an anonymous heap aggregate: a fixed record of its
        // items behind one reference (see `canonical_aggregates`).
        assert!(matches!(
            layouts.get(bundle.roots()[0]).expect("tuple root").kind(),
            LayoutKind::PackedRecord { fields } if fields.len() == 2
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

    #[test]
    fn concrete_recursive_generic_nominals_are_distinct_stable_and_closed() {
        fn stack() -> witchy_syntax::ast::TypeDef {
            generic_heap_definition(
                "Stack",
                &["a"],
                vec![
                    Type::Named("a".into(), Vec::new()),
                    Type::Named("Stack".into(), vec![Type::Named("a".into(), Vec::new())]),
                ],
            )
        }

        fn bundle(order: &[Type]) -> witchy_wir::layout::LayoutBundle {
            let definition = stack();
            let mut projection = LayoutProjection::default();
            projection.definitions.insert("Stack".into(), definition.clone());
            projection.definitions.insert("main.Stack".into(), definition);
            for ty in order {
                projection.visit_type(ty).expect("concrete recursive generic root");
            }
            projection.finish().expect("closed generic recursive catalog")
        }

        let int_stack = Type::Named("main.Stack".into(), vec![named("Int")]);
        let string_stack = Type::Named("main.Stack".into(), vec![named("String")]);
        let first = bundle(&[int_stack.clone(), string_stack.clone()]);
        let second = bundle(&[string_stack, int_stack]);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.roots(), second.roots());

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&first.canonical_bytes())
            .expect("decode generic recursive catalog");
        let nominal_roots = first.roots().iter().copied().filter(|root| {
            matches!(layouts.get(*root).map(|layout| layout.kind()), Some(LayoutKind::OpaqueNominal { .. }))
        }).collect::<Vec<_>>();
        assert_eq!(nominal_roots.len(), 2, "Stack(Int) and Stack(String) retain separate identities");
        assert_ne!(nominal_roots[0], nominal_roots[1]);
        for root in nominal_roots {
            assert!(layouts.nominal_target(root).is_some(), "every generic nominal root is bound");
        }

        let definition = stack();
        let mut projection = LayoutProjection::default();
        projection.definitions.insert("Stack".into(), definition.clone());
        projection.definitions.insert("main.Stack".into(), definition);
        assert!(projection.visit_type(&Type::Named("main.Stack".into(), vec![named("a")])).is_err(),
            "unresolved generic arguments must not acquire a layout");
    }
}
