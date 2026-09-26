//! Canonical v1 layout projection from the checked source product.
//!
//! Layout authority is derived here from the typed module that owns the source
//! witness. Callers cannot provide a second layout graph and cannot make a
//! layout-bearing type appear supported by supplying a matching descriptor.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};
use witchy_canonical_source::aggregates::{
    aggregate_key, canonical_owner_aggregate_identity, concrete_aggregate_identity,
    is_scalar_type,
    scalar_aggregate_shape, shape_from_definition,
};
use witchy_canonical_source::abi::CanonicalScalarKind;
use witchy_canonical_source::carrier::CarrierElement;
use witchy_canonical_source::storage::{OpaqueOwnerClass, OpaqueOwnerEvidence, StoragePlan, StorageRefusal, affine_storage_shape, aggregate_uses_nullable_owned_fields, is_inline_scalar_record};
use witchy_canonical_source::rooted_view::RootedViewStorage;
use witchy_canonical_source::structural_identity::{AggregateOwnerKey, ClosureOwnerKey, SourceTypeKey};
use witchy_syntax::ast::{self, Item, Type, TypeDef};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck;
use witchy_wir::canonical_safewir::{GcTypeId, closure_owner_contract::CaptureStorage};
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, PointerSlot, RcHeader,
    ResolvedNamed, ScalarKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedIntReferenceLayout {
    pub owner_type: GcTypeId,
    pub environment: LayoutId,
    pub scalar_root: LayoutId,
    pub list_root: Option<LayoutId>,
}

/// Canonical storage for an immutable `&[T]`: a retained list root and its
/// checked slice projection.  It is deliberately distinct from `&Int` so a
/// scalar-place reader can never reinterpret a slice carrier.  One family per
/// admitted element; they share the environment shape and differ in the
/// backing list's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootedSliceLayout {
    pub element: CarrierElement,
    pub owner_type: GcTypeId,
    pub environment: LayoutId,
    pub list_root: LayoutId,
}

fn rooted_slice_element_from_checked_definitions(
    element: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<CarrierElement> {
    StoragePlan::classify(element, definitions, |_| Some(0)).into_carrier_element()
}

pub(crate) fn rooted_slice_element_type(element: &CarrierElement) -> Type {
    element.source_type()
}

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
    pub owner_type: Option<GcTypeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosureEnvironmentVariant {
    pub witness: Option<u32>,
    pub fields: Vec<Type>,
    pub first_field: usize,
    pub storage: Vec<CaptureStorage>,
}

/// Source-derived storage for an aggregate whose value cannot live entirely
/// in linear memory. The linear environment contains only the tag and
/// ordinary scalar/reference lanes; opaque owners remain typed GC fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpaqueAggregateFamilyLayout {
    pub source: Type,
    pub environment: LayoutId,
    pub variants: Vec<OpaqueAggregateVariant>,
    pub owner_type: GcTypeId,
    pub list_type: GcTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpaqueAggregateVariant {
    pub name: String,
    pub fields: Vec<Type>,
    pub first_field: usize,
    pub storage: Vec<CaptureStorage>,
    pub environment_tag: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalLayoutError {
    MissingCheckedFunctionType(String),
    MissingCheckedExpressionType(String),
    NonConcreteExpressionType(String),
    UnsupportedType(String),
    InvalidLayout(String),
    InvalidCheckedAccessFacts(String),
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
            Self::InvalidCheckedAccessFacts(error) => {
                write!(formatter, "canonical layout lacks valid checked access facts: {error}")
            }
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
    source.access_facts().map_err(|error| CanonicalLayoutError::InvalidCheckedAccessFacts(error.to_string()))?;
    projected_layouts(source).map(|(bundle, _, _, _, _)| bundle)
}

/// Derive the complete closure-family storage schema from the same checked
/// source product as the canonical layout bundle.  This is intentionally not
/// a caller-provided record schema: witness order, checked capture types, and
/// every physical child layout are all reconstructed here.
pub(crate) fn closure_families(
    source: &CanonicalCheckedSource,
) -> Result<HashMap<Type, ClosureFamilyLayout>, CanonicalLayoutError> {
    projected_layouts(source).map(|(_, families, _, _, _)| families)
}

/// Derive every aggregate that must use the opaque-owner representation.
/// Classification and IDs come from normalized checked source identities and
/// are shared with bundle construction, so a caller cannot cross-pair a
/// family schema with a different layout catalog.
pub(crate) fn opaque_aggregate_families(
    source: &CanonicalCheckedSource,
) -> Result<HashMap<Type, OpaqueAggregateFamilyLayout>, CanonicalLayoutError> {
    projected_layouts(source).map(|(_, _, families, _, _)| families)
}

pub(crate) fn shared_int_reference_layout(
    source: &CanonicalCheckedSource,
) -> Result<Option<SharedIntReferenceLayout>, CanonicalLayoutError> {
    projected_layouts(source).map(|(_, _, _, shared, _)| shared)
}

pub(crate) fn rooted_slice_layouts(
    source: &CanonicalCheckedSource,
) -> Result<Vec<RootedSliceLayout>, CanonicalLayoutError> {
    projected_layouts(source).map(|(_, _, _, _, slice)| slice)
}

/// The closure identity in the same concrete declaration domain as physical
/// layout projection. Callable conventions remain part of the identity; only
/// nominal aliases and concrete aggregate arguments are normalized.
pub(crate) fn canonical_closure_key(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Type {
    witchy_canonical_source::callable_storage::canonical_closure_family_type(ty, definitions)
}

/// The exact capture variants of each `fn` family, ordered by the checked
/// closure witness manifest.  The manifest is the source authority for both
/// tag values and capture types.
fn closure_capture_variants(
    source: &CanonicalCheckedSource,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<HashMap<Type, Vec<(u32, Vec<Type>)>>, CanonicalLayoutError> {
    let mut variants = HashMap::new();
    for (index, witness) in source.closure_witness().entries().iter().enumerate() {
        let Some(fn_type) = witness.signature().cloned() else { continue };
        let index = u32::try_from(index).map_err(|_| CanonicalLayoutError::InvalidLayout("closure witness index overflow".into()))?;
        variants.entry(canonical_closure_key(&fn_type, definitions)).or_insert_with(Vec::new).push((
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

/// Everything a layout projection yields, in `finish_with_families` order.
type ProjectedLayouts = (
    LayoutBundle,
    HashMap<Type, ClosureFamilyLayout>,
    HashMap<Type, OpaqueAggregateFamilyLayout>,
    Option<SharedIntReferenceLayout>,
    Vec<RootedSliceLayout>,
);

thread_local! {
    /// One slot per open `with_layout_memo` scope: the source it was opened
    /// for, and that source's projection once computed.
    static LAYOUT_MEMO: std::cell::RefCell<Vec<(usize, Option<std::rc::Rc<ProjectedLayouts>>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Project `source`'s layouts once for the duration of `body`.
///
/// `derive_layout_bundle`, `closure_families`, `opaque_aggregate_families`,
/// `shared_int_reference_layout` and `rooted_slice_layouts` each used to
/// run the WHOLE projection and keep one element of its result: five full
/// projections per compile of the same source (counted on
/// examples/channels). Inside this scope they share one.
///
/// Keyed by the source's address, which is sound for the same reason the
/// aggregate shape memo is: `body` holds `source` borrowed immutably, so it
/// can be neither mutated nor freed (and its address reused) while the scope
/// is open. Outside a scope every call projects afresh, exactly as before.
pub(crate) fn with_layout_memo<R>(source: &CanonicalCheckedSource, body: impl FnOnce() -> R) -> R {
    LAYOUT_MEMO.with(|memo| memo.borrow_mut().push((std::ptr::from_ref(source) as usize, None)));
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            LAYOUT_MEMO.with(|memo| {
                memo.borrow_mut().pop();
            });
        }
    }
    let _pop = Pop;
    body()
}

fn projected_layouts(source: &CanonicalCheckedSource) -> Result<ProjectedLayouts, CanonicalLayoutError> {
    let address = std::ptr::from_ref(source) as usize;
    let cached = LAYOUT_MEMO.with(|memo| {
        memo.borrow().iter().rev().find(|(key, _)| *key == address).map(|(_, value)| value.clone())
    });
    match cached {
        Some(Some(known)) => Ok((*known).clone()),
        Some(None) => {
            let projected = project_layouts(source)?.finish_with_families()?;
            let shared = std::rc::Rc::new(projected.clone());
            LAYOUT_MEMO.with(|memo| {
                if let Some(slot) = memo.borrow_mut().iter_mut().rev().find(|(key, _)| *key == address) {
                    slot.1 = Some(shared);
                }
            });
            Ok(projected)
        }
        None => project_layouts(source)?.finish_with_families(),
    }
}

fn project_layouts(
    source: &CanonicalCheckedSource,
) -> Result<LayoutProjection, CanonicalLayoutError> {
    let typed = source.typed_module();
    let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
    let definitions = std::rc::Rc::new(source.type_definitions().clone());
    let capture_variants = closure_capture_variants(source, &definitions)?;
    let mut projection = LayoutProjection {
        roots: Vec::new(),
        rooted_slices_requested: BTreeSet::new(),
        definitions,
        closures: HashMap::new(),
        capture_variants,
        closure_witnesses: source.closure_witness().clone(),
        visiting: HashSet::new(),
        visited: HashSet::new(),
    };

    let memo_definitions = std::rc::Rc::clone(&projection.definitions);
    witchy_canonical_source::aggregates::with_shape_memo(&memo_definitions, || -> Result<(), CanonicalLayoutError> {
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
            // The checked callable table intentionally erases loan syntax from
            // scalar carriers.  Preserve the independently checked source
            // declaration long enough to project the addressable scalar root
            // required by a direct `&Int` ABI.
            for parameter in &function.params {
                if let Some(ty) = &parameter.ty {
                    projection.visit_type(ty)?;
                }
            }
            if let Some(result) = &function.ret {
                projection.visit_type(result)?;
            }

            witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
                projection.visit_expression(
                    expression,
                    typed.table().type_of(expression),
                    typed.table().is_non_completing(expression),
                )
            })
            .map_err(|error| error)?;
        }

        Ok(())
    })?;

    Ok(projection)
}

#[derive(Default)]
struct LayoutProjection {
    roots: Vec<Type>,
    /// A rooted slice carrier is authority-bearing, unlike an ordinary
    /// `List(T)` layout. Only an explicit checked `&[T]` source type may
    /// request it, per admitted element; observing a list alone must never
    /// mint the owner family.
    rooted_slices_requested: BTreeSet<CarrierElement>,
    // Shared so the walk can hold a shape-memo scope on the map while it
    // mutates the rest of the projection (see `with_shape_memo`).
    definitions: std::rc::Rc<BTreeMap<String, TypeDef>>,
    /// Every `fn` type reached, keyed by its normalized checked type, with
    /// its exact source-witness environment variants.
    closures: HashMap<Type, Vec<(u32, Vec<Type>)>>,
    capture_variants: HashMap<Type, Vec<(u32, Vec<Type>)>>,
    closure_witnesses: witchy_canonical_source::closure::ClosureWitnessManifest,
    visiting: HashSet<Type>,
    visited: HashSet<Type>,
}

impl LayoutProjection {
    fn visit_expression(
        &mut self,
        expression: &ast::Expr,
        checked: Option<&typeck::Ty>,
        checked_noncompletion: bool,
    ) -> Result<(), CanonicalLayoutError> {
        // This callback omits only the result layout. The AST visitor still
        // checks every descendant, including values on terminating edges.
        if matches!(expression, ast::Expr::Index { .. }) || matches!(expression,
            ast::Expr::Call { name, .. } if matches!(name.as_str(), "list.at" | "list.__at" | "list.__set_at"))
        {
            // The failure edge borrows an immutable source-function label.
            self.visit_type(&Type::Named("String".into(), Vec::new()))?;
        }
        if checked_noncompletion {
            return Ok(());
        }
        let Some(checked) = checked else {
            // Scalar literals and checked variable references add no root.
            // String literals are memory-bearing and retain their layout.
            return match expression {
                ast::Expr::Int(_)
                | ast::Expr::Float(_)
                | ast::Expr::Duration(_)
                | ast::Expr::Bool(_)
                | ast::Expr::Var(_) => Ok(()),
                // A range used as an index is projection syntax, not a
                // separately materialized list. The checker records its
                // retained bounds and the enclosing index result, but
                // deliberately gives this syntax-only node no table entry.
                // A standalone range does have a checked result type and
                // therefore cannot reach this arm.
                ast::Expr::Range { .. } => Ok(()),
                ast::Expr::Str(_) => self.visit_type(&Type::Named("String".into(), Vec::new())),
                _ => Err(CanonicalLayoutError::MissingCheckedExpressionType(
                    format!("{expression:?}"),
                )),
            };
        };
        let source_type = typeck::ty_to_ast(checked).ok_or_else(|| {
            CanonicalLayoutError::NonConcreteExpressionType(checked.to_string())
        })?;
        // Type checking uses the private `Slice(T)` nominal identity for the
        // result of a surface `&place[lo..hi]`.  Only the checked borrow
        // expression may turn that internal identity into slice-carrier
        // authority; later variable/method uses merely reuse the layout.
        if matches!(expression, ast::Expr::Unary { op: ast::UnOp::Borrow, .. })
            && let Some(element) = rooted_slice_element(&source_type)
                .and_then(|element| rooted_slice_element_from_checked_definitions(element, &self.definitions))
        {
            self.rooted_slices_requested.insert(element);
        }
        self.visit_type(&source_type)
    }

    fn visit_type(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
        if matches!(ty.unqualified(), Type::Named(name, _) if name == "Dict")
            && !matches!(StoragePlan::classify(ty.unqualified(), &self.definitions, |_| Some(0)),
                StoragePlan::Owned(CarrierElement::Dict(_, _)) | StoragePlan::AuthenticatedPlace(_))
        {
            return Err(CanonicalLayoutError::UnsupportedType(
                "Dict children lack a recursive checked storage and ownership proof".into(),
            ));
        }
        // Qualifiers are intentionally erased from ordinary physical layout
        // identities. A direct shared scalar reference is the exception: it
        // requires an addressable scalar root even though the unqualified
        // scalar value itself is otherwise carried inline.
        if let Type::Qualified(ast::TypeQual::View | ast::TypeQual::Borrow(_), inner) = ty
            && scalar_kind_of(inner) == Some(ScalarKind::Int)
        {
            self.roots.push(inner.as_ref().clone());
            return Ok(());
        }
        if let Type::Qualified(ast::TypeQual::View, inner) = ty
            && let Some(element) = rooted_slice_element(inner)
        {
            if let Some(element) = rooted_slice_element_from_checked_definitions(element, &self.definitions) {
                self.rooted_slices_requested.insert(element);
            }
            return self.visit_type(&Type::Named("List".into(), vec![element.clone()]));
        }
        // `ty_to_ast` preserves the checker's private `Slice(T)` identity as a
        // nominal. It is storage backed by `List(T)`, never an independently
        // constructible public source type and never authority by itself.
        if let Some(element) = rooted_slice_element(ty) {
            return self.visit_type(&Type::Named("List".into(), vec![element.clone()]));
        }
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
        if witchy_canonical_source::capabilities::representation(ty, &self.definitions).is_some() {
            return Ok(());
        }
        // A dictionary is laid out as its canonical tuple of the key list
        // and the value list, before any aggregate rule sees the name.
        if let Some(tuple) = witchy_canonical_source::aggregates::dictionary_as_tuple(ty) {
            return self.visit_type(&tuple);
        }
        match ty {
            Type::Qualified(_, inner) => self.visit_type(inner),
            Type::Tuple(fields) if fields.is_empty()
                && matches!(StoragePlan::classify(ty, &self.definitions, |_| Some(0)),
                    StoragePlan::InlineScalar(CanonicalScalarKind::Unit)) => Ok(()),
            // A tuple is an anonymous heap aggregate; its reference fields
            // are roots of their own.
            Type::Tuple(fields) if scalar_aggregate_shape(ty, &self.definitions).is_some()
                && matches!(StoragePlan::classify(ty, &self.definitions, |_| Some(0)),
                    StoragePlan::Owned(CarrierElement::Tuple(_)) | StoragePlan::AggregateView(_)
                        | StoragePlan::AuthenticatedPlace(_)) => {
                for field in fields {
                    if !layout_field_is_inline(field, &self.definitions) {
                        self.visit_type(field)?;
                    }
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            // A `fn` type's values are closure records; the type's
            // parameters and result carry their own roots.
            Type::Fn(parameters, result, _, _) => {
                let fn_type = canonical_closure_key(ty, &self.definitions);
                let variants = self.capture_variants.get(&fn_type).cloned().unwrap_or_default();
                self.closures.insert(fn_type, variants);
                for parameter in parameters {
                    self.visit_type(parameter)?;
                }
                self.visit_type(result)
            }
            // A heap aggregate (record, sum, scalar-or-reference
            // `Option`/`Result`) is a root; its reference fields are roots of
            // their own.
            Type::Named(..)
                if scalar_aggregate_shape(ty, &self.definitions).is_some_and(|shape| !shape.packed)
                    && matches!(StoragePlan::classify(ty, &self.definitions, |_| Some(0)),
                        StoragePlan::Owned(CarrierElement::Record { .. } | CarrierElement::Sum { .. })
                            | StoragePlan::AggregateView(_) | StoragePlan::AuthenticatedPlace(_)) =>
            {
                let shape = scalar_aggregate_shape(ty, &self.definitions).expect("checked above");
                for field in &shape.fields {
                    if !layout_field_is_inline(field, &self.definitions) {
                        self.visit_type(field)?;
                    }
                }
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Named(..)
                if matches!(StoragePlan::classify(ty, &self.definitions, |_| Some(0)),
                    StoragePlan::InlineAggregate(_)) =>
            {
                self.roots.push(ty.clone());
                Ok(())
            }
            Type::Named(name, arguments) if arguments.is_empty() =>
                match StoragePlan::classify(ty, &self.definitions, |_| Some(0)) {
                StoragePlan::InlineScalar(_) => Ok(()),
                StoragePlan::Owned(CarrierElement::String | CarrierElement::Bytes) => {
                    self.roots.push(ty.clone());
                    Ok(())
                }
                _ => Err(CanonicalLayoutError::UnsupportedType(name.clone())),
            },
            Type::Named(name, arguments) if name == "List" => {
                let storage = StoragePlan::classify(ty, &self.definitions, |_| Some(0));
                let admitted = matches!(
                    storage,
                    StoragePlan::RetainedOwnerList(_) | StoragePlan::RetainedOpaqueAggregateList(_)
                        | StoragePlan::MoveOnlyList(_),
                ) || arguments.len() == 1 && StoragePlan::classify(
                    &arguments[0], &self.definitions, |_| Some(0),
                ).list_element_disposition(&arguments[0]).is_some();
                if !admitted {
                    if matches!(storage, StoragePlan::Refused(StorageRefusal::UnprovedAuthority)) {
                        return Err(CanonicalLayoutError::UnsupportedType(format!("capability-bearing {ty:?}")));
                    }
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
        self.finish_with_families().map(|(bundle, _, _, _, _)| bundle)
    }

    fn finish_with_families(self) -> Result<(
        LayoutBundle,
        HashMap<Type, ClosureFamilyLayout>,
        HashMap<Type, OpaqueAggregateFamilyLayout>,
        Option<SharedIntReferenceLayout>,
        Vec<RootedSliceLayout>,
    ), CanonicalLayoutError> {
        let memo_definitions = std::rc::Rc::clone(&self.definitions);
        witchy_canonical_source::aggregates::with_shape_memo(&memo_definitions, move || self.finish_with_families_scoped())
    }

    fn finish_with_families_scoped(self) -> Result<(
        LayoutBundle,
        HashMap<Type, ClosureFamilyLayout>,
        HashMap<Type, OpaqueAggregateFamilyLayout>,
        Option<SharedIntReferenceLayout>,
        Vec<RootedSliceLayout>,
    ), CanonicalLayoutError> {
        let mut interner = LayoutInterner::new();
        let mut roots = BTreeSet::new();
        // Reserve every closure environment identity, and its two-field
        // closure record, before constructing any body. A capture may itself
        // be a function from the same family (or a mutually recursive
        // family), so constructing one environment before reserving the
        // others would recurse through physical bodies and lose the checked
        // callable identity.
        let mut keyed_closures = self.closures.into_iter().map(|(signature, variants)| {
            let key = SourceTypeKey { source: &signature }.digest().ok_or_else(|| {
                CanonicalLayoutError::InvalidLayout("closure family type key exceeds bounds".into())
            })?;
            Ok((key, signature, variants))
        }).collect::<Result<Vec<_>, CanonicalLayoutError>>()?;
        keyed_closures.sort_by_key(|(key, _, _)| *key);
        if keyed_closures.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(CanonicalLayoutError::InvalidLayout("closure family type key collision".into()));
        }
        let closure_specs = keyed_closures.into_iter().map(|(_, signature, variants)| {
            (signature, variants)
        }).collect::<Vec<_>>();
        // A family has one value ABI, including its named adapters. Only
        // families whose checked captures require opaque storage change ABI.
        // These ids identify candidate types; the independent checker binds
        // the complete family schema back to checked source before admission.
        let opaque_evidence = OpaqueOwnerEvidence::from_checked_source(
            &self.definitions, &self.closure_witnesses,
        ).ok_or_else(|| CanonicalLayoutError::InvalidLayout(
            "checked closure owner evidence exceeds bounds".into(),
        ))?;
        let aggregate_specs = self.visited.iter().filter_map(|ty| {
            let source = canonical_layout_type(ty, &self.definitions);
            // Direct and optional host tokens already have a checked externref
            // ABI. Their source shape must not also mint an aggregate family.
            if witchy_canonical_source::capabilities::representation(&source, &self.definitions).is_some() {
                return None;
            }
            let shape = scalar_aggregate_shape(&source, &self.definitions)?;
            (!shape.packed && matches!(source.unqualified(), Type::Named(..) | Type::Tuple(..)))
                .then_some((source, shape))
        }).collect::<Vec<_>>();
        let mut keyed_aggregate_specs = aggregate_specs.into_iter().map(|(source, shape)| {
            let key = SourceTypeKey { source: &source }.digest().ok_or_else(|| {
                CanonicalLayoutError::InvalidLayout("aggregate family type key exceeds bounds".into())
            })?;
            Ok((key, source, shape))
        }).collect::<Result<Vec<_>, CanonicalLayoutError>>()?;
        keyed_aggregate_specs.sort_by_key(|(key, _, _)| *key);
        if keyed_aggregate_specs.windows(2).any(|pair| {
            pair[0].0 == pair[1].0 && (pair[0].1 != pair[1].1 || pair[0].2 != pair[1].2)
        }) {
            return Err(CanonicalLayoutError::InvalidLayout("aggregate family type key collision".into()));
        }
        keyed_aggregate_specs.dedup_by(|left, right| left.1 == right.1);
        let aggregate_specs = keyed_aggregate_specs.into_iter().map(|(_, source, shape)| {
            (source, shape)
        }).collect::<Vec<_>>();
        let opaque_signatures = closure_specs.iter()
            .filter(|(signature, _)| opaque_evidence.owner_class(signature) == Some(OpaqueOwnerClass::Callable))
            .map(|(signature, _)| signature.clone()).collect::<HashSet<_>>();
        let opaque_aggregates = aggregate_specs.iter()
            .filter(|(source, _)| opaque_evidence.owner_class(source) == Some(OpaqueOwnerClass::Aggregate))
            .map(|(source, _)| source.clone()).collect::<HashSet<_>>();
        let mut owner_types = HashMap::new();
        let mut closure_family_keys = HashMap::new();
        let mut allocated_closure_ids = HashSet::new();
        for (signature, variants) in &closure_specs {
            let owner_fields = variants.iter().map(|(_, fields)| {
                fields.iter().map(|field| opaque_evidence.owner_class(field).is_some())
                    .collect::<Vec<_>>()
            }).collect::<Vec<_>>();
            let digest = ClosureOwnerKey { signature, variants, owner_fields: &owner_fields }.digest()
                .ok_or_else(|| CanonicalLayoutError::InvalidLayout("closure identity exceeds bounds".into()))?;
            closure_family_keys.insert(signature.clone(), digest);
            if opaque_signatures.contains(signature) {
                let lower = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
                let id = 0xc100_0000 | ((lower & 0x007f_ffff) << 1);
                if !allocated_closure_ids.insert(id) {
                    return Err(CanonicalLayoutError::InvalidLayout("opaque closure owner ID collision".into()));
                }
                owner_types.insert(signature.clone(), GcTypeId::new(id));
            }
        }
        let opaque_aggregate_specs = aggregate_specs.into_iter()
            .filter(|(source, _)| opaque_aggregates.contains(source))
            .collect::<Vec<_>>();
        let mut aggregate_owner_types = HashMap::new();
        let mut aggregate_family_keys = HashMap::new();
        let mut allocated_aggregate_ids = HashSet::new();
        for (source, shape) in &opaque_aggregate_specs {
            let owner_fields = shape.fields.iter().map(|field| opaque_evidence.owner_class(field).is_some())
                .collect::<Vec<_>>();
            let digest = AggregateOwnerKey { source, shape, owner_fields: &owner_fields }.digest()
                .ok_or_else(|| CanonicalLayoutError::InvalidLayout("opaque aggregate identity exceeds bounds".into()))?;
            aggregate_family_keys.insert(source.clone(), digest);
            let lower = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
            let id = 0xc200_0000 | ((lower & 0x007f_ffff) << 1);
            if !allocated_aggregate_ids.insert(id) {
                return Err(CanonicalLayoutError::InvalidLayout("opaque aggregate owner ID collision".into()));
            }
            aggregate_owner_types.insert(source.clone(), GcTypeId::new(id));
        }
        let mut closure_environments = HashMap::new();
        for (signature, _) in &closure_specs {
            let mut digest = Sha256::new();
            digest.update(b"witchy/canonical-layout/closure-environment-v2\0");
            digest.update(closure_family_keys[signature]);
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
        let mut aggregate_environments = HashMap::new();
        for (source, _) in &opaque_aggregate_specs {
            let mut digest = Sha256::new();
            digest.update(b"witchy/canonical-layout/opaque-aggregate-environment-v2\0");
            digest.update(aggregate_family_keys[source]);
            let environment = interner.intern_opaque_nominal(digest.finalize().into())
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            aggregate_environments.insert(source.clone(), environment);
        }
        let mut recursive_types = recursive_heap_types(&self.roots, &self.definitions);
        recursive_types.retain(|ty| !opaque_aggregates.contains(ty));
        // Reserve all nominal cycle breakers before constructing any body.
        // A structural root such as List(Tree) may be visited before Tree;
        // following its pointee must not recursively rebuild that list while
        // the list's content-derived descriptor is still being constructed.
        let mut recursive = recursive_types.iter().map(|ty| {
            let key = SourceTypeKey { source: ty }.digest().ok_or_else(|| {
                CanonicalLayoutError::InvalidLayout("recursive nominal identity exceeds bounds".into())
            })?;
            Ok((key, ty))
        }).collect::<Result<Vec<_>, CanonicalLayoutError>>()?;
        recursive.sort_by_key(|(key, _)| *key);
        if recursive.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(CanonicalLayoutError::InvalidLayout("recursive nominal identity collision".into()));
        }
        for (_, ty) in &recursive {
            intern_recursive_nominal(&mut interner, ty)?;
        }
        for (_, ty) in recursive {
            let nominal = intern_recursive_nominal(&mut interner, ty)?;
            let body = intern_v1_layout_fields(
                &mut interner, ty, &self.definitions, &recursive_types,
                &mut HashSet::new(), &closure_layouts,
            )?;
            interner.bind_nominal(nominal, body)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        }
        for ty in &self.roots {
            let identity = canonical_layout_type(ty, &self.definitions);
            if matches!(opaque_evidence.owner_class(&identity),
                Some(OpaqueOwnerClass::Aggregate | OpaqueOwnerClass::Collection)) {
                // Opaque owner lists have typed reference storage, not a
                // linear-memory layout with address-sized element slots.
                continue;
            }
            let root = intern_v1_layout(
                &mut interner, ty, &self.definitions, &recursive_types, &closure_layouts,
            )?;
            roots.insert(root);
        }
        // A `&[Int]` carrier forms `&Int` element views (`&xs[i]`, and the
        // by-value read `xs[i]` through one), so it needs the shared Int
        // reference family even where no `&Int` is written.
        let mut rooted_owner_ids = HashSet::new();
        let shared_int_reference = if self.rooted_slices_requested.contains(&CarrierElement::Scalar(witchy_canonical_source::abi::CanonicalScalarKind::Int)) || self.roots.iter().any(|ty| {
            matches!(ty.unqualified(), Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty())
        }) {
            let scalar_type = Type::Named("Int".into(), Vec::new());
            let scalar_root = intern_v1_layout(
                &mut interner, &scalar_type, &self.definitions, &recursive_types, &closure_layouts,
            )?;
            let tag = interner.intern_scalar(ScalarKind::Int)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let scalar_slot = interner.intern_runtime_pointer_value_slot(scalar_root, PointerSlot::OwnedNullable)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let mut children = vec![tag, scalar_slot];
            let list_type = Type::Named("List".into(), vec![scalar_type]);
            let list_root = if self.roots.iter().any(|ty| canonical_layout_type(ty, &self.definitions) == list_type) {
                let root = intern_v1_layout(
                    &mut interner, &list_type, &self.definitions, &recursive_types, &closure_layouts,
                )?;
                let slot = interner.intern_runtime_pointer_value_slot(root, PointerSlot::OwnedNullable)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
                let index = interner.intern_scalar(ScalarKind::U32)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
                children.extend([slot, index]);
                Some(root)
            } else {
                None
            };
            let environment = interner.intern_packed_record(children)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            roots.insert(environment);
            let owner_type = GcTypeId::new(RootedViewStorage::ScalarCell(CanonicalScalarKind::Int)
                .owner_family_id().ok_or_else(|| CanonicalLayoutError::InvalidLayout("unproved scalar-view owner family".into()))?);
            if !rooted_owner_ids.insert(owner_type.get()) {
                return Err(CanonicalLayoutError::InvalidLayout("rooted view owner structural ID collision".into()));
            }
            Some(SharedIntReferenceLayout { owner_type, environment, scalar_root, list_root })
        } else {
            None
        };
        let mut rooted_slices = Vec::new();
        for element in &self.rooted_slices_requested {
            let element_type = rooted_slice_element_type(element);
            let list_type = Type::Named("List".into(), vec![element_type]);
            if !self.roots.iter().any(|ty| canonical_layout_type(ty, &self.definitions) == list_type) {
                continue;
            }
            let owner_type = GcTypeId::new(RootedViewStorage::RetainedProjection(element.clone())
                .owner_family_id().ok_or_else(|| CanonicalLayoutError::InvalidLayout("unproved slice-view owner family".into()))?);
            if !rooted_owner_ids.insert(owner_type.get()) {
                return Err(CanonicalLayoutError::InvalidLayout("rooted view owner structural ID collision".into()));
            }
            let list_root = intern_v1_layout(
                &mut interner, &list_type, &self.definitions, &recursive_types, &closure_layouts,
            )?;
            let tag = interner.intern_scalar(ScalarKind::Int)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let root = interner.intern_runtime_pointer_value_slot(list_root, PointerSlot::OwnedNullable)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let start = interner.intern_scalar(ScalarKind::U32)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let length = interner.intern_scalar(ScalarKind::U32)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let environment = interner.intern_packed_record(vec![tag, root, start, length])
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            roots.insert(environment);
            rooted_slices.push(RootedSliceLayout { element: element.clone(), owner_type, environment, list_root });
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
                &opaque_evidence,
            )?;
            interner.bind_nominal(environment, environment_body)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            let closure = closure_layouts[&signature];
            roots.insert(environment);
            roots.insert(closure);
            let owner_type = owner_types.get(&signature).copied();
            families.insert(signature, ClosureFamilyLayout { closure, environment, variants, owner_type });
        }
        let mut aggregate_families = HashMap::new();
        for (source, shape) in opaque_aggregate_specs {
            let environment = aggregate_environments[&source];
            let (environment_body, variants) = intern_opaque_aggregate_environment(
                &mut interner,
                &shape,
                &self.definitions,
                &recursive_types,
                &closure_layouts,
                &opaque_evidence,
            )?;
            interner.bind_nominal(environment, environment_body)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
            roots.insert(environment);
            let owner_type = aggregate_owner_types[&source];
            let list_type = GcTypeId::new(owner_type.get().checked_add(1).ok_or_else(|| {
                CanonicalLayoutError::InvalidLayout("opaque aggregate list type overflow".into())
            })?);
            aggregate_families.insert(source.clone(), OpaqueAggregateFamilyLayout {
                source, environment, variants, owner_type, list_type,
            });
        }
        let bundle = LayoutBundle::from_interner(&interner, roots)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        Ok((bundle, families, aggregate_families, shared_int_reference, rooted_slices))
    }
}

fn rooted_slice_element(ty: &Type) -> Option<&Type> {
    match ty.unqualified() {
        Type::Slice(element) => Some(element),
        Type::Named(name, arguments) if name == "Slice" && arguments.len() == 1 => {
            Some(&arguments[0])
        }
        _ => None,
    }
}

fn intern_closure_environment(
    interner: &mut LayoutInterner,
    witnesses: Vec<(u32, Vec<Type>)>,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
    opaque_evidence: &OpaqueOwnerEvidence<'_>,
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
        storage: Vec::new(),
    }];
    for (witness, fields) in witnesses {
        let first_field = children.len();
        let mut storage = Vec::with_capacity(fields.len());
        let mut owner_field = 2u32;
        for field in &fields {
            if opaque_evidence.owner_class(field).is_some() {
                storage.push(CaptureStorage::OwnerField(owner_field));
                owner_field = owner_field.checked_add(1).ok_or_else(||
                    CanonicalLayoutError::InvalidLayout("opaque capture field overflow".into()))?;
                continue;
            }
            storage.push(CaptureStorage::EnvironmentField(u32::try_from(children.len())
                .map_err(|_| CanonicalLayoutError::InvalidLayout("closure field overflow".into()))?));
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
        variants.push(ClosureEnvironmentVariant { witness: Some(witness), fields, first_field, storage });
    }
    let layout = interner
        .intern_packed_record(children)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    Ok((layout, variants))
}

#[allow(clippy::too_many_arguments)]
fn intern_opaque_aggregate_environment(
    interner: &mut LayoutInterner,
    shape: &witchy_canonical_source::aggregates::ScalarAggregateShape,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
    opaque_evidence: &OpaqueOwnerEvidence<'_>,
) -> Result<(LayoutId, Vec<OpaqueAggregateVariant>), CanonicalLayoutError> {
    let tag = interner.intern_scalar(ScalarKind::Int)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
    let mut children = vec![tag];
    let mut owner_field = 2u32;
    let mut variants = Vec::with_capacity(shape.variants.len());
    for (tag_value, variant) in shape.variants.iter().enumerate() {
        let fields = shape.fields[variant.first_field..variant.first_field + variant.arity].to_vec();
        let first_field = children.len();
        let mut storage = Vec::with_capacity(fields.len());
        for field in &fields {
            if opaque_evidence.owner_class(field).is_some() {
                storage.push(CaptureStorage::OwnerField(owner_field));
                owner_field = owner_field.checked_add(1).ok_or_else(|| {
                    CanonicalLayoutError::InvalidLayout("opaque aggregate owner field overflow".into())
                })?;
                continue;
            }
            storage.push(CaptureStorage::EnvironmentField(u32::try_from(children.len())
                .map_err(|_| CanonicalLayoutError::InvalidLayout("opaque aggregate field overflow".into()))?));
            let child = if let Some(scalar) = scalar_kind_of(field) {
                interner.intern_scalar(scalar)
                    .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            } else {
                let pointee = intern_v1_layout(
                    interner, field, definitions, recursive_types, closure_layouts,
                )?;
                interner.intern_runtime_pointer_value_slot(
                    pointee,
                    if shape.tagged { PointerSlot::OwnedNullable } else { PointerSlot::Owned },
                ).map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
            };
            children.push(child);
        }
        variants.push(OpaqueAggregateVariant {
            name: variant.name.clone(),
            fields,
            first_field,
            storage,
            environment_tag: i64::try_from(tag_value).map_err(|_| {
                CanonicalLayoutError::InvalidLayout("opaque aggregate tag overflow".into())
            })?,
        });
    }
    let layout = interner.intern_packed_record(children)
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
    if witchy_canonical_source::capabilities::representation(ty, definitions).is_some() {
        return Err(CanonicalLayoutError::UnsupportedType(format!("opaque capability has no linear layout: {ty:?}")));
    }
    let key = canonical_layout_type(ty, definitions);
    if matches!(key.unqualified(), Type::Fn(..)) {
        return closure_layouts.get(&canonical_closure_key(&key, definitions)).copied().ok_or_else(|| {
            CanonicalLayoutError::UnsupportedType(format!("unregistered closure family {key:?}"))
        });
    }
    if recursive_types.contains(&key) {
        // Bodies are bound separately by finish. References use only the
        // reserved identity, regardless of whether its body is already bound.
        return intern_recursive_nominal(interner, &key);
    }
    // An affine physical record may share its field layout with a tagged sum
    // whose inactive fields may be absent. Keep the source identity on the
    // layout so allocation and ABI binding select the same presence policy.
    if scalar_aggregate_shape(&key, definitions).is_some_and(|shape| !shape.packed)
        && affine_storage_shape(&key, definitions).is_some()
    {
        if !active.insert(key.clone()) {
            return Err(CanonicalLayoutError::UnsupportedType(format!(
                "cyclic affine layout {key:?}"
            )));
        }
        let nominal = intern_recursive_nominal(interner, &key)?;
        let body = intern_v1_layout_fields(
            interner, &key, definitions, recursive_types, active, closure_layouts,
        )?;
        active.remove(&key);
        interner.bind_nominal(nominal, body)
            .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?;
        return Ok(nominal);
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
        let element_storage = StoragePlan::classify(&arguments[0], definitions, |_| Some(0));
        if element_storage.list_element_disposition(&arguments[0]).is_none() {
            return Err(CanonicalLayoutError::UnsupportedType(format!("{ty:?}")));
        }
        let inline = matches!(element_storage,
            StoragePlan::InlineScalar(_) | StoragePlan::Owned(CarrierElement::Scalar(_)));
        let element = if inline {
            let scalar = scalar_kind_of(&arguments[0]).ok_or_else(|| {
                CanonicalLayoutError::UnsupportedType(format!("{ty:?}"))
            })?;
            interner
                .intern_scalar(scalar)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
        } else {
            intern_v1_layout_checked(
                interner, &arguments[0], definitions, recursive_types, active, closure_layouts,
            )?
        };
        let element = if !inline {
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
    if let Some(tuple) = witchy_canonical_source::aggregates::dictionary_as_tuple(ty) {
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
        let nullable_fields = aggregate_uses_nullable_owned_fields(ty, definitions);
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
                        if nullable_fields {
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
    aggregate_key(ty).ok_or_else(|| {
        CanonicalLayoutError::UnsupportedType(format!("recursive nominal layout {ty:?}"))
    })?;
    let key = witchy_canonical_source::structural_identity::recursive_nominal_layout_key(ty).ok_or_else(|| {
        CanonicalLayoutError::InvalidLayout("recursive nominal identity exceeds bounds".into())
    })?;
    interner
        .intern_opaque_nominal(key)
        .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))
}

/// Derive the catalog's exact nominal ID without granting any layout authority,
/// from the checked aggregate identity rather than a declaration-local spelling.
/// Consumers must still find this ID and its binding in the checked catalog.
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
    canonical_owner_aggregate_identity(ty, definitions).unwrap_or_else(|| ty.unqualified().clone())
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
    if let Some(tuple) = witchy_canonical_source::aggregates::dictionary_as_tuple(ty) {
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

fn layout_field_is_inline(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    matches!(StoragePlan::classify(ty, definitions, |_| Some(0)),
        StoragePlan::InlineScalar(_)
            | StoragePlan::RootedView(
                witchy_canonical_source::rooted_view::RootedViewStorage::InlineScalar(_)))
}

fn scalar_kind_of(ty: &Type) -> Option<ScalarKind> {
    let StoragePlan::InlineScalar(kind) = StoragePlan::classify(ty.unqualified(), &BTreeMap::new(), |_| None) else {
        return None;
    };
    Some(match kind {
        CanonicalScalarKind::Unit => ScalarKind::Unit,
        CanonicalScalarKind::Int => ScalarKind::Int,
        CanonicalScalarKind::Float => ScalarKind::Float,
        CanonicalScalarKind::Duration => ScalarKind::Duration,
        CanonicalScalarKind::Bool => ScalarKind::Bool,
    })
}

fn is_byte_buffer_type(ty: &Type) -> bool {
    matches!(StoragePlan::classify(ty.unqualified(), &BTreeMap::new(), |_| None),
        StoragePlan::Owned(CarrierElement::String | CarrierElement::Bytes))
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
            "Nil" | "Unit" => Some(ResolvedNamed::Scalar(ScalarKind::Unit)),
            "List" => Some(ResolvedNamed::PackedList { rc: RcHeader::Required }),
            name => {
                // A keyed instantiation (`Option(Int)`) arrives as its own
                // name; a user type arrives bare.
                let key = aggregate_key(&Type::Named(name.to_owned(), _arguments.to_vec()));
                self.packed_views
                    .get(name)
                    .or_else(|| key.as_deref().and_then(|key| self.packed_views.get(key)))
                    .or_else(|| self.definitions.get(name).filter(|definition| is_inline_scalar_record(definition)))
                    .map(ResolvedNamed::PackedRecord)
            }
        }
    }
}

/// Layout invariants that need private layout internals, compiled always so
/// the crate needs no `cfg(test)` build (`[lib] test = false`). Driven by
/// `tests/codegen/canonical_layouts.rs`; each panics on a violated invariant.
#[doc(hidden)]
pub mod self_checks {
    use super::{CarrierElement, LayoutProjection, Type};

    fn named(name: &str) -> Type {
        Type::Named(name.to_owned(), Vec::new())
    }

    pub fn noncompletion_still_requires_checked_memory_bearing_descendants() {
        use witchy_syntax::ast::{Expr, Item, Stmt, visit::visit_block};
        let module = witchy_syntax::parser::parse_module(
            "fn choose(flag: Bool) -> List(String):\n    match flag:\n        true ->\n            return [\"left\"]\n        false ->\n            return [\"right\"]\n    [\"unreachable\"]\n",
        ).expect("source parses");
        let typed = witchy_types::typeck::annotate_checked(module).expect("source checks");
        let Item::Function(function) = &typed.module().items[0] else { panic!("function") };
        let Stmt::Expr(parent @ Expr::Match { arms, .. }) = &function.body.stmts[0] else {
            panic!("all-returning match")
        };
        let Expr::Block(arm) = &arms[0].body else { panic!("returning arm") };
        let Stmt::Return(Some(child @ Expr::List(_))) = &arm.stmts[0] else {
            panic!("returned list")
        };
        assert!(typed.table().is_non_completing(parent));
        assert!(typed.table().type_of(parent).is_none());
        assert!(!typed.table().is_non_completing(child));
        let list_string = Type::Named("List".into(), vec![named("String")]);
        assert_eq!(typed.table().type_of(child).and_then(witchy_types::typeck::ty_to_ast), Some(list_string.clone()));

        let mut positive = LayoutProjection::default();
        let mut visited_child = false;
        visit_block(&function.body, &mut |expression| {
            visited_child |= std::ptr::eq(expression, child);
            positive.visit_expression(expression, typed.table().type_of(expression), typed.table().is_non_completing(expression))
        }).expect("checked descendants still project");
        assert!(visited_child);
        assert!(positive.roots.contains(&list_string));
        assert!(positive.roots.contains(&named("String")));
        positive.finish().expect("valid owned string-list layout");

        let mut negative = LayoutProjection::default();
        let mut visited_parent = false;
        let mut withheld = 0;
        let error = visit_block(&function.body, &mut |expression| {
            visited_parent |= std::ptr::eq(expression, parent);
            let supplied = if std::ptr::eq(expression, child) {
                withheld += 1;
                None
            } else {
                typed.table().type_of(expression)
            };
            negative.visit_expression(expression, supplied, typed.table().is_non_completing(expression))
        }).expect_err("parent noncompletion cannot hide a missing child type");
        assert!(visited_parent);
        assert_eq!(withheld, 1);
        assert_eq!(error, super::CanonicalLayoutError::MissingCheckedExpressionType(format!("{child:?}")));
    }

    pub fn rooted_slice_layout_requires_explicit_view_slice_request() {
        for forged in ["List(Int)", "List(Float)", "List(Bool)", "List(Duration)"] {
            assert_eq!(CarrierElement::from_checked_type(&named(forged)), None,
                "only a structural List child mints a rooted-slice family");
        }
        let list = Type::Named("List".into(), vec![named("Int")]);
        let mut ordinary = LayoutProjection::default();
        ordinary.visit_type(&list).expect("ordinary List(Int) projects");
        let (_, _, _, _, slice) = ordinary.finish_with_families()
            .expect("ordinary List(Int) catalog finishes");
        assert!(slice.is_empty(), "an owned List(Int) must not mint a rooted-slice family");

        for element in ["Int", "Float", "String"] {
            let view_slice = Type::Qualified(
                witchy_syntax::ast::TypeQual::View,
                Box::new(Type::Slice(Box::new(named(element)))),
            );
            let mut explicit = LayoutProjection::default();
            explicit.visit_type(&view_slice).expect("explicit &[T] projects");
            let (bundle, _, _, _, slices) = explicit.finish_with_families()
                .expect("explicit &[T] catalog finishes");
            let [slice] = slices.as_slice() else { panic!("only explicit &[{element}] mints its rooted-slice layout") };
            assert_eq!(slice.element, CarrierElement::from_checked_type(&named(element)).expect("checked element"));
            assert!(bundle.roots().contains(&slice.environment));
            assert!(bundle.roots().contains(&slice.list_root));
        }

        // Recursive list elements use the same source and ownership relation.
        let nested = Type::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(Type::Slice(Box::new(Type::Named("List".into(), vec![Type::Named("List".into(), vec![named("Int")])])))),
        );
        let mut recursive = LayoutProjection::default();
        recursive.visit_type(&nested).expect("recursive list slice projects its list layout");
        let (_, _, _, _, slices) = recursive.finish_with_families().expect("catalog finishes");
        assert_eq!(slices.len(), 1);
    }

    fn heap_definition(name: &str, fields: Vec<Type>) -> witchy_syntax::ast::TypeDef {
        let mut definition = witchy_canonical_source::aggregates::ScalarAggregateShape {
            name: name.into(), fields, variants: Vec::new(), tagged: false, packed: false,
        }.packed_view();
        definition.packed = false;
        definition
    }

    pub fn direct_and_nested_capabilities_never_acquire_linear_slots() {
        let definitions = std::collections::BTreeMap::from([
            ("Inner".into(), heap_definition("Inner", vec![named("Console")])),
            ("Outer".into(), heap_definition("Outer", vec![named("Inner"), named("String")])),
        ]);
        for source in [
            named("Console"),
            named("Inner"),
            named("Outer"),
            Type::Tuple(vec![named("Console"), named("String")]),
            Type::Named("List".into(), vec![named("Console")]),
            Type::Named("List".into(), vec![named("Inner")]),
        ] {
            let error = super::intern_v1_layout(
                &mut witchy_wir::layout::LayoutInterner::new(), &source, &definitions,
                &std::collections::HashSet::new(), &std::collections::HashMap::new(),
            ).expect_err("a capability cannot be interned as a linear value or pointer pointee");
            assert!(matches!(error, super::CanonicalLayoutError::UnsupportedType(ref message)
                if message.contains("opaque capability has no linear layout")
                    || matches!(&source, Type::Named(name, _) if name == "List")),
                "{source:?}: {error:?}");
        }

        let mut projection = LayoutProjection::default();
        projection.visit_type(&named("Console")).unwrap();
        let (bundle, closures, aggregates, _, _) = projection.finish_with_families().unwrap();
        assert!(bundle.roots().is_empty(), "a direct capability projects no linear root");
        assert!(closures.is_empty() && aggregates.is_empty());

        let mut packed = heap_definition("Packed", vec![named("Console")]);
        packed.packed = true;
        let mut projection = LayoutProjection::default();
        std::rc::Rc::make_mut(&mut projection.definitions).insert("Packed".into(), packed);
        assert!(projection.visit_type(&named("Packed")).and_then(|_| projection.finish()).is_err(),
            "an inline packed record cannot hide a capability field");
    }

}
