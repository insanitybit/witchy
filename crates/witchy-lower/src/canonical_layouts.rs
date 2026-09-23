//! Canonical v1 layout projection from the checked source product.
//!
//! Layout authority is derived here from the typed module that owns the source
//! witness. Callers cannot provide a second layout graph and cannot make a
//! layout-bearing type appear supported by supplying a matching descriptor.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};
use witchy_types::canonical_aggregates::{aggregate_key, canonical_owner_aggregate_identity, concrete_aggregate_identity, is_scalar_type, is_v1_list_element_type, scalar_aggregate_shape, shape_from_definition};
use witchy_syntax::ast::{self, Item, Type, TypeDef};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck;
use witchy_wir::canonical_safewir::{GcTypeId, closure_owner_contract::CaptureStorage};
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, PointerSlot, RcHeader,
    ResolvedNamed, ScalarKind,
};

/// Stable opaque identity for the v1 shared `&Int` place carrier. The carrier
/// owns an authenticated root plus constructor-specific projection operands;
/// it is deliberately disjoint from closure and aggregate owner namespaces.
pub(crate) fn shared_int_reference_owner_type() -> GcTypeId {
    GcTypeId::new(0xc300_0000)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedIntReferenceLayout {
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
    /// One of `closure_owner_contract::ROOTED_SLICE_ELEMENTS`.
    pub element: &'static str,
    pub environment: LayoutId,
    pub list_root: LayoutId,
}

/// The stable opaque identity of the `&[T]` family for an admitted element
/// name; `&[Int]` keeps its original identity.  Each identity leaves its
/// successor free, like every owner family's list type.
pub(crate) fn rooted_slice_owner_type(element: &str) -> Option<GcTypeId> {
    let offset = match element {
        "Int" => 0x2,
        "Float" => 0x4,
        "Bool" => 0x6,
        "Duration" => 0x8,
        "String" => 0xa,
        _ => return None,
    };
    Some(GcTypeId::new(0xc300_0000 + offset))
}

pub(crate) fn rooted_int_slice_owner_type() -> GcTypeId {
    rooted_slice_owner_type("Int").expect("Int is an admitted slice element")
}

/// The admitted element name of a slice element type.
pub(crate) fn rooted_slice_element_name(element: &Type) -> Option<&'static str> {
    let Type::Named(name, arguments) = element.unqualified() else { return None };
    if !arguments.is_empty() {
        return None;
    }
    witchy_wir::canonical_safewir::closure_owner_contract::ROOTED_SLICE_ELEMENTS
        .into_iter().find(|admitted| admitted == name)
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

/// The closure identity in the same concrete declaration domain as physical
/// layout projection. Callable conventions remain part of the identity; only
/// nominal aliases and concrete aggregate arguments are normalized.
pub(crate) fn canonical_closure_key(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Type {
    // Lowering asks this of the same few `fn` and aggregate types for every
    // list update and record rebuild of the std executor; neither identity
    // normalization nor the key rewrite is otherwise memoized for `fn` types.
    witchy_types::canonical_aggregates::memoize_derived_type("canonical_closure_key", ty, definitions, || {
        let identity = concrete_aggregate_identity(ty, definitions)
            .unwrap_or_else(|| ty.unqualified().clone());
        closure_key(&identity)
    })
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
        let Some(fn_type) = typeck::ty_to_ast(witness.signature()) else { continue };
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
    }).collect::<BTreeMap<_, _>>();
    let definitions = std::rc::Rc::new(definitions);
    let capture_variants = closure_capture_variants(source, &definitions)?;
    let mut projection = LayoutProjection {
        roots: Vec::new(),
        rooted_slices_requested: BTreeSet::new(),
        definitions,
        closures: BTreeMap::new(),
        capture_variants,
        visiting: HashSet::new(),
        visited: HashSet::new(),
    };

    let memo_definitions = std::rc::Rc::clone(&projection.definitions);
    witchy_types::canonical_aggregates::with_shape_memo(&memo_definitions, || -> Result<(), CanonicalLayoutError> {
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
    rooted_slices_requested: BTreeSet<&'static str>,
    // Shared so the walk can hold a shape-memo scope on the map while it
    // mutates the rest of the projection (see `with_shape_memo`).
    definitions: std::rc::Rc<BTreeMap<String, TypeDef>>,
    /// Every `fn` type reached, keyed by its rendering, with its exact
    /// source-witness environment variants.
    closures: BTreeMap<String, (Type, Vec<(u32, Vec<Type>)>)>,
    capture_variants: HashMap<Type, Vec<(u32, Vec<Type>)>>,
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
            && let Some(element) = rooted_slice_element(&source_type).and_then(rooted_slice_element_name)
        {
            self.rooted_slices_requested.insert(element);
        }
        self.visit_type(&source_type)
    }

    fn visit_type(&mut self, ty: &Type) -> Result<(), CanonicalLayoutError> {
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
            if let Some(element) = rooted_slice_element_name(element) {
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
                let fn_type = canonical_closure_key(ty, &self.definitions);
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
                "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "Console" | "Env" | "BuildOut" | "Exec" => Ok(()),
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
                if arguments.len() != 1 || (!matches!(arguments[0].unqualified(), Type::Fn(..))
                    && !is_v1_list_element_type(&arguments[0], &self.definitions)) {
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
        witchy_types::canonical_aggregates::with_shape_memo(&memo_definitions, move || self.finish_with_families_scoped())
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
        let closure_specs = self.closures.into_values().collect::<Vec<_>>();
        // A family has one value ABI, including its named adapters. Only
        // families whose checked captures require opaque storage change ABI.
        // These ids identify candidate types; the independent checker binds
        // the complete family schema back to checked source before admission.
        let mut opaque_signatures = HashSet::new();
        let mut aggregate_specs = self.visited.iter().filter_map(|ty| {
            let source = canonical_layout_type(ty, &self.definitions);
            // Direct and optional host tokens already have a checked externref
            // ABI. Their source shape must not also mint an aggregate family.
            if witchy_types::canonical_capabilities::representation(&source, &self.definitions).is_some() {
                return None;
            }
            let shape = scalar_aggregate_shape(&source, &self.definitions)?;
            (!shape.packed && matches!(source.unqualified(), Type::Named(..) | Type::Tuple(..)))
                .then_some((source, shape))
        }).collect::<Vec<_>>();
        aggregate_specs.sort_by_key(|(ty, _)| format!("{ty:?}"));
        aggregate_specs.dedup_by(|(left, _), (right, _)| left == right);
        let mut opaque_aggregates = HashSet::new();
        loop {
            let mut changed = false;
            for (signature, variants) in &closure_specs {
                let requires_owner = variants.iter().flat_map(|(_, fields)| fields).any(|field| {
                    type_requires_opaque_owner(
                        field,
                        &self.definitions,
                        &opaque_signatures,
                        &opaque_aggregates,
                        &mut HashSet::new(),
                    )
                });
                if requires_owner && opaque_signatures.insert(signature.clone()) {
                    changed = true;
                }
            }
            for (source, shape) in &aggregate_specs {
                let requires_owner = shape.fields.iter().skip(usize::from(shape.tagged)).any(|field| {
                    type_requires_opaque_owner(
                        field,
                        &self.definitions,
                        &opaque_signatures,
                        &opaque_aggregates,
                        &mut HashSet::new(),
                    )
                });
                if requires_owner && opaque_aggregates.insert(source.clone()) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut owner_types = HashMap::new();
        for (ordinal, (signature, _)) in closure_specs.iter().enumerate() {
            if opaque_signatures.contains(signature) {
                let id = u32::try_from(ordinal).ok().and_then(|n| n.checked_mul(2))
                    .and_then(|n| n.checked_add(0xc100_0000))
                    .filter(|n| *n < u32::MAX)
                    .ok_or_else(|| CanonicalLayoutError::InvalidLayout("opaque closure type overflow".into()))?;
                owner_types.insert(signature.clone(), GcTypeId::new(id));
            }
        }
        let opaque_aggregate_specs = aggregate_specs.into_iter()
            .filter(|(source, _)| opaque_aggregates.contains(source))
            .collect::<Vec<_>>();
        let mut aggregate_owner_types = HashMap::new();
        for (ordinal, (source, _)) in opaque_aggregate_specs.iter().enumerate() {
            let id = u32::try_from(ordinal).ok().and_then(|n| n.checked_mul(2))
                .and_then(|n| n.checked_add(0xc200_0000))
                .filter(|n| *n < u32::MAX)
                .ok_or_else(|| CanonicalLayoutError::InvalidLayout("opaque aggregate type overflow".into()))?;
            aggregate_owner_types.insert(source.clone(), GcTypeId::new(id));
        }
        let mut closure_environments = HashMap::new();
        for (signature, _) in &closure_specs {
            let key = format!("{:?}", canonical_closure_key(signature, &self.definitions));
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
        let mut aggregate_environments = HashMap::new();
        for (source, _) in &opaque_aggregate_specs {
            let key = format!("{source:?}");
            let mut digest = Sha256::new();
            digest.update(b"witchy/canonical-layout/opaque-aggregate-environment-v1\0");
            digest.update((key.len() as u64).to_le_bytes());
            digest.update(key.as_bytes());
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
        for ty in &self.roots {
            let identity = canonical_layout_type(ty, &self.definitions);
            let opaque_aggregate = aggregate_owner_types.contains_key(&identity);
            let opaque_list_element = match identity.unqualified() {
                Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                    opaque_owner_type_for(
                        &arguments[0], &self.definitions, &owner_types, &aggregate_owner_types,
                    ).is_some()
                }
                _ => false,
            };
            if opaque_aggregate || opaque_list_element {
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
        let shared_int_reference = if self.rooted_slices_requested.contains("Int") || self.roots.iter().any(|ty| {
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
            Some(SharedIntReferenceLayout { environment, scalar_root, list_root })
        } else {
            None
        };
        let mut rooted_slices = Vec::new();
        for element in &self.rooted_slices_requested {
            let list_type = Type::Named("List".into(), vec![Type::Named((*element).into(), Vec::new())]);
            if !self.roots.iter().any(|ty| canonical_layout_type(ty, &self.definitions) == list_type) {
                continue;
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
            rooted_slices.push(RootedSliceLayout { element, environment, list_root });
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
                &owner_types,
                &aggregate_owner_types,
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
                &owner_types,
                &aggregate_owner_types,
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

fn type_requires_opaque_owner(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    opaque_signatures: &HashSet<Type>,
    opaque_aggregates: &HashSet<Type>,
    visiting: &mut HashSet<Type>,
) -> bool {
    // A canonical rooted view is an authenticated GC owner for its backing
    // root plus projection.  It cannot occupy an ordinary aggregate's linear
    // field: doing so would erase the retained-root contract.  Keep this
    // deliberately to the installed direct `&Int` carrier; other view
    // payloads remain outside canonical v1.
    if matches!(ty, Type::Qualified(ast::TypeQual::View, inner)
        if matches!(inner.unqualified(), Type::Named(name, arguments)
            if name == "Int" && arguments.is_empty()))
    {
        return true;
    }
    if witchy_types::canonical_capabilities::representation(ty, definitions).is_some() {
        return true;
    }
    if matches!(ty.unqualified(), Type::Fn(..)) {
        return opaque_signatures.contains(&canonical_closure_key(ty, definitions));
    }
    let identity = canonical_layout_type(ty, definitions);
    if opaque_aggregates.contains(&identity) {
        return true;
    }
    if !visiting.insert(identity.clone()) {
        return false;
    }
    let requires_owner = if let Some(tuple) =
        witchy_types::canonical_aggregates::dictionary_as_tuple(identity.unqualified())
    {
        type_requires_opaque_owner(
            &tuple, definitions, opaque_signatures, opaque_aggregates, visiting,
        )
    } else if let Some(shape) = scalar_aggregate_shape(&identity, definitions) {
        shape.fields.iter().any(|field| {
            type_requires_opaque_owner(
                field, definitions, opaque_signatures, opaque_aggregates, visiting,
            )
        })
    } else {
        match identity.unqualified() {
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                type_requires_opaque_owner(
                    &arguments[0], definitions, opaque_signatures, opaque_aggregates, visiting,
                )
            }
            Type::Tuple(fields) => fields.iter().any(|field| {
                type_requires_opaque_owner(
                    field, definitions, opaque_signatures, opaque_aggregates, visiting,
                )
            }),
            _ => false,
        }
    };
    visiting.remove(&identity);
    requires_owner
}

fn intern_closure_environment(
    interner: &mut LayoutInterner,
    witnesses: Vec<(u32, Vec<Type>)>,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
    owner_types: &HashMap<Type, GcTypeId>,
    aggregate_owner_types: &HashMap<Type, GcTypeId>,
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
            if witchy_types::canonical_capabilities::representation(field, definitions).is_some() {
                storage.push(CaptureStorage::OwnerField(owner_field));
                owner_field = owner_field.checked_add(1).ok_or_else(||
                    CanonicalLayoutError::InvalidLayout("opaque capture field overflow".into()))?;
                continue;
            }
            if opaque_owner_type_for(field, definitions, owner_types, aggregate_owner_types).is_some() {
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

fn opaque_owner_type_for(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    closure_owner_types: &HashMap<Type, GcTypeId>,
    aggregate_owner_types: &HashMap<Type, GcTypeId>,
) -> Option<GcTypeId> {
    if matches!(ty, Type::Qualified(ast::TypeQual::View, inner)
        if matches!(inner.unqualified(), Type::Named(name, arguments)
            if name == "Int" && arguments.is_empty()))
    {
        return Some(shared_int_reference_owner_type());
    }
    if matches!(ty.unqualified(), Type::Fn(..)) {
        return closure_owner_types.get(&canonical_closure_key(ty, definitions)).copied();
    }
    let identity = canonical_layout_type(ty, definitions);
    if let Some(owner) = aggregate_owner_types.get(&identity) {
        return Some(*owner);
    }
    let Type::Named(name, arguments) = identity.unqualified() else { return None };
    if name != "List" || arguments.len() != 1 {
        return None;
    }
    let element = &arguments[0];
    let owner = if matches!(element.unqualified(), Type::Fn(..)) {
        closure_owner_types.get(&canonical_closure_key(element, definitions))
    } else {
        aggregate_owner_types.get(&canonical_layout_type(element, definitions))
    }?;
    owner.get().checked_add(1).map(GcTypeId::new)
}

#[allow(clippy::too_many_arguments)]
fn intern_opaque_aggregate_environment(
    interner: &mut LayoutInterner,
    shape: &witchy_types::canonical_aggregates::ScalarAggregateShape,
    definitions: &BTreeMap<String, TypeDef>,
    recursive_types: &HashSet<Type>,
    closure_layouts: &HashMap<Type, LayoutId>,
    closure_owner_types: &HashMap<Type, GcTypeId>,
    aggregate_owner_types: &HashMap<Type, GcTypeId>,
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
            if witchy_types::canonical_capabilities::representation(field, definitions).is_some()
                || opaque_owner_type_for(
                    field, definitions, closure_owner_types, aggregate_owner_types,
                ).is_some()
            {
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
    if witchy_types::canonical_capabilities::representation(ty, definitions).is_some() {
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
        let element = if let Some(scalar) = scalar_kind_of(&arguments[0]) {
            interner
                .intern_scalar(scalar)
                .map_err(|error| CanonicalLayoutError::InvalidLayout(error.to_string()))?
        } else {
            intern_v1_layout_checked(
                interner, &arguments[0], definitions, recursive_types, active, closure_layouts,
            )?
        };
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
        // The parser/type checker retain `()` as an empty tuple, while the
        // canonical value ABI carries it as Unit. Closure and aggregate
        // environments must use that same scalar lane rather than a distinct
        // zero-field record layout.
        Type::Tuple(items) if items.is_empty() => Some(ScalarKind::Unit),
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" => Some(ScalarKind::Int),
            "Float" => Some(ScalarKind::Float),
            "Duration" => Some(ScalarKind::Duration),
            "Bool" => Some(ScalarKind::Bool),
            "Nil" | "Unit" => Some(ScalarKind::Unit),
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
            "Nil" | "Unit" => Some(ResolvedNamed::Scalar(ScalarKind::Unit)),
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
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit")
        ))
}

#[cfg(test)]
fn is_v1_packed_scalar_record(definition: &TypeDef) -> bool {
    definition.packed
        && definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit")
        ))
}

#[cfg(test)]
mod layout_memo_tests {
    use super::*;

    fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
        witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.as_bytes().to_vec())],
            "main", |_, _, _| Ok(Default::default()),
        ).expect("fixture checks")
    }

    const RECORD: &str = "type Pair:\n    a: Int\n    b: Int\n\nfn main() -> Int:\n    let p = Pair(1, 2)\n    p.a + p.b\n";
    const OTHER: &str = "type Triple:\n    x: Int\n    y: Int\n    z: Int\n\nfn main() -> Int:\n    let t = Triple(1, 2, 3)\n    t.x\n";

    #[test]
    fn scoped_projection_equals_the_unscoped_one() {
        let module = checked(RECORD);
        let source = module.canonical_source().expect("canonical source");
        let unscoped = derive_layout_bundle(source).expect("unscoped bundle");
        let (first, second, families) = with_layout_memo(source, || (
            derive_layout_bundle(source).expect("first scoped bundle"),
            derive_layout_bundle(source).expect("second (cached) bundle"),
            closure_families(source).expect("families from the same projection"),
        ));
        assert_eq!(first, unscoped, "a scoped projection must equal an unscoped one");
        assert_eq!(second, unscoped, "a cache hit must return the same bundle");
        assert_eq!(families, closure_families(source).expect("unscoped families"));
    }

    #[test]
    fn nested_scopes_for_different_sources_never_cross() {
        let outer_module = checked(RECORD);
        let inner_module = checked(OTHER);
        let outer = outer_module.canonical_source().expect("outer source");
        let inner = inner_module.canonical_source().expect("inner source");
        let (outer_before, inner_bundle, outer_after) = with_layout_memo(outer, || {
            let outer_before = derive_layout_bundle(outer).expect("outer");
            let inner_bundle = with_layout_memo(inner, || derive_layout_bundle(inner).expect("inner"));
            (outer_before, inner_bundle, derive_layout_bundle(outer).expect("outer again"))
        });
        assert_eq!(outer_before, derive_layout_bundle(outer).expect("outer unscoped"));
        assert_eq!(inner_bundle, derive_layout_bundle(inner).expect("inner unscoped"));
        assert_eq!(outer_after, outer_before, "the inner scope leaked into the outer one");
        assert_ne!(inner_bundle, outer_before, "fixtures must differ for this test to mean anything");
    }

    #[test]
    fn a_panicking_body_still_pops_its_scope() {
        let module = checked(RECORD);
        let source = module.canonical_source().expect("canonical source");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_layout_memo(source, || -> () { panic!("body fails") })
        }));
        assert!(result.is_err());
        LAYOUT_MEMO.with(|memo| assert!(memo.borrow().is_empty(), "a panic left a stale scope behind"));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{LayoutProjection, Type};
    use witchy_syntax::ast::{CallableQualifiers, Convention, TypeDef, Variant};
    use witchy_wir::canonical_safewir::closure_owner_contract::CaptureStorage;
    use witchy_wir::layout::{HeaderLayout, LayoutKind, PointerSlot, RcHeader, ScalarKind};

    fn named(name: &str) -> Type {
        Type::Named(name.to_owned(), Vec::new())
    }

    #[test]
    fn noncompletion_still_requires_checked_memory_bearing_descendants() {
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

    #[test]
    fn rooted_slice_layout_requires_explicit_view_slice_request() {
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
            assert_eq!(slice.element, element);
            assert!(bundle.roots().contains(&slice.environment));
            assert!(bundle.roots().contains(&slice.list_root));
        }

        // A slice of owners has no carrier and mints no family.
        let nested = Type::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(Type::Slice(Box::new(Type::Named("List".into(), vec![named("Int")])))),
        );
        let mut refused = LayoutProjection::default();
        refused.visit_type(&nested).expect("&[List(Int)] still projects its list layout");
        let (_, _, _, _, slices) = refused.finish_with_families().expect("catalog finishes");
        assert!(slices.is_empty(), "&[List(Int)] has no rooted-slice family");
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

    fn sum_definition(name: &str, variants: Vec<(&str, Vec<Type>)>) -> TypeDef {
        TypeDef {
            name: name.into(),
            must_consume: false,
            params: Vec::new(),
            variants: variants.into_iter().map(|(name, fields)| Variant {
                name: name.into(),
                line: 0,
                field_names: (0..fields.len()).map(|index| format!("field{index}")).collect(),
                field_lines: vec![0; fields.len()],
                fields,
            }).collect(),
            derives: Vec::new(),
            sealed: false,
            is_capability: false,
            grantable: false,
            packed: false,
            partial_eq_derived: false,
            public_state_derived: false,
        }
    }

    fn opaque_projection(
        definitions: Vec<TypeDef>,
        roots: &[Type],
    ) -> (witchy_wir::layout::LayoutBundle, std::collections::HashMap<Type, super::OpaqueAggregateFamilyLayout>) {
        let mut projection = LayoutProjection::default();
        for definition in definitions {
            std::rc::Rc::make_mut(&mut projection.definitions).insert(definition.name.clone(), definition);
        }
        for root in roots {
            projection.visit_type(root).expect("opaque aggregate source projects");
        }
        let (bundle, _, families, _, _) = projection.finish_with_families()
            .expect("opaque aggregate layouts finish");
        (bundle, families)
    }

    #[test]
    fn checked_task_tuple_catalog_preserves_owner_fields_and_nonnullable_storage() {
        use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, "test/task-tuple-catalog", "0.1.0").unwrap();
        let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
        owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
        }));
        let source = b"import chan\nasync fn next(n: Int) -> Int:\n    n + 1\nasync fn main(console: Console):\n    let values = chan.par_map([4], fn(n): next(n)).await\n    console.print(\"${values}\")\n";
        let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), source.to_vec()),
                ("task".into(), include_bytes!("../../../std/task.witchy").to_vec()),
                ("chan".into(), include_bytes!("../../../std/chan.witchy").to_vec())],
            "main", |_, _, _| Ok(Default::default()),
            &std::collections::HashSet::from(["main".into()]),
            AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap(),
        ).expect("the actual Task<Tuple> source checks");
        let tuple = Type::Tuple(vec![
            Type::Named("List".into(), vec![Type::Named("task.Task".into(), vec![Type::Tuple(Vec::new())])]),
            Type::Named("List".into(), vec![Type::Named("chan.Receiver".into(), vec![named("Int")])]),
        ]);
        let task = Type::Named("task.Task".into(), vec![tuple.clone()]);
        let (bundle, _, families, _, _) = super::project_layouts(checked.canonical_source().unwrap())
            .expect("exact checked Task<Tuple> layouts project")
            .finish_with_families().expect("exact checked catalog finishes");
        let tuple_family = families.get(&tuple).expect("exact concrete tuple owner identity");
        let task_family = families.get(&task).expect("exact concrete Task<Tuple> owner identity");
        assert_eq!(tuple_family.source, tuple);
        assert_eq!(task_family.source, task);
        assert_ne!(tuple_family.owner_type, task_family.owner_type);
        assert_eq!(task_family.variants.len(), 1);
        assert_eq!(task_family.variants[0].storage, vec![CaptureStorage::OwnerField(2)]);
        assert_eq!(tuple_family.variants.len(), 1);
        assert_eq!(tuple_family.variants[0].fields, match &tuple {
            Type::Tuple(fields) => fields.clone(),
            _ => unreachable!(),
        });
        assert_eq!(tuple_family.variants[0].storage, vec![
            CaptureStorage::OwnerField(2), CaptureStorage::EnvironmentField(1),
        ]);
        assert!(bundle.roots().contains(&task_family.environment));
        assert!(bundle.roots().contains(&tuple_family.environment));
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("exact Task<Tuple> catalog roundtrips");
        let task_body = layouts.nominal_target(task_family.environment).expect("Task environment binding");
        let LayoutKind::PackedRecord { fields } = layouts.get(task_body).unwrap().kind() else {
            panic!("Task environment is a packed record");
        };
        assert_eq!(fields.len(), 1, "the GC callback never occupies a linear slot");
        let tuple_body = layouts.nominal_target(tuple_family.environment).expect("tuple environment binding");
        let LayoutKind::PackedRecord { fields } = layouts.get(tuple_body).unwrap().kind() else {
            panic!("tuple environment is a packed record");
        };
        assert_eq!(fields.len(), 2, "the GC task list never occupies a linear slot");
        assert!(matches!(layouts.get(fields[1]).unwrap().kind(),
            LayoutKind::RuntimePointerValueSlot { semantics: PointerSlot::Owned, .. }),
            "the untagged tuple receiver-list field is owned, not nullable");
    }

    #[test]
    fn task_like_aggregate_uses_existing_opaque_closure_owner() {
        let callback = Type::Fn(
            vec![named("Int")],
            Box::new(named("Nil")),
            vec![Convention::Let],
            CallableQualifiers::default(),
        );
        let mut projection = LayoutProjection::default();
        std::rc::Rc::make_mut(&mut projection.definitions).insert(
            "Task".into(),
            sum_definition("Task", vec![("Ready", vec![callback.clone()]), ("Done", Vec::new())]),
        );
        let callback_key = super::canonical_closure_key(&callback, &projection.definitions);
        projection.capture_variants.insert(callback_key, vec![(7, vec![named("Console")])]);
        projection.visit_type(&named("Task")).expect("Task reaches its callback family");

        let (_, closures, aggregates, _, _) = projection.finish_with_families()
            .expect("Task-like family projects");
        assert!(closures.values().any(|family| family.owner_type.is_some()));
        let task = &aggregates[&named("Task")];
        assert_eq!(task.variants[0].name, "Ready");
        assert_eq!(task.variants[0].fields, vec![callback]);
        assert_eq!(task.variants[0].storage, vec![CaptureStorage::OwnerField(2)]);
        assert_eq!(task.variants[0].environment_tag, 0);
        assert_eq!(task.variants[1].environment_tag, 1);
    }

    #[test]
    fn closure_environment_uses_unit_scalar_for_empty_tuple_capture() {
        let unit = Type::Tuple(Vec::new());
        let callback = Type::Fn(
            Vec::new(),
            Box::new(unit.clone()),
            Vec::new(),
            CallableQualifiers::default(),
        );
        let mut projection = LayoutProjection::default();
        let key = super::canonical_closure_key(&callback, &projection.definitions);
        projection.capture_variants.insert(key.clone(), vec![(7, vec![unit])]);
        projection.visit_type(&callback).expect("Unit-capturing closure projects");

        let (bundle, closures, _, _, _) = projection.finish_with_families()
            .expect("Unit-capturing closure layout finishes");
        let family = &closures[&key];
        assert_eq!(family.variants[1].storage, vec![CaptureStorage::EnvironmentField(1)]);
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("closure environment catalog decodes");
        let body = layouts.nominal_target(family.environment).expect("closure environment is bound");
        let LayoutKind::PackedRecord { fields } = layouts.get(body).expect("environment descriptor").kind() else {
            panic!("closure environment is a packed record");
        };
        assert!(matches!(layouts.get(fields[1]).expect("Unit capture field").kind(),
            LayoutKind::Scalar(ScalarKind::Unit)));
    }

    #[test]
    fn nested_and_capability_aggregate_fields_are_owner_fields() {
        let definitions = vec![
            heap_definition("Inner", vec![named("Console")]),
            heap_definition("Outer", vec![named("Inner"), named("String")]),
        ];
        let (bundle, families) = opaque_projection(definitions, &[named("Outer")]);
        let inner = &families[&named("Inner")];
        assert_eq!(inner.variants[0].storage, vec![CaptureStorage::OwnerField(2)]);
        let outer = &families[&named("Outer")];
        assert_eq!(outer.variants[0].storage, vec![
            CaptureStorage::OwnerField(2),
            CaptureStorage::EnvironmentField(1),
        ]);
        assert_eq!(outer.variants[0].first_field, 1);
        assert!(bundle.roots().contains(&inner.environment));
        assert!(bundle.roots().contains(&outer.environment));

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("opaque aggregate catalog decodes");
        let body = layouts.nominal_target(outer.environment).expect("outer environment is bound");
        let LayoutKind::PackedRecord { fields } = layouts.get(body).expect("outer body").kind() else {
            panic!("opaque aggregate environment must be a packed record");
        };
        assert_eq!(fields.len(), 2, "owner field never occupies linear bytes");
    }

    #[test]
    fn direct_and_nested_capabilities_never_acquire_linear_slots() {
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
                if message.contains("opaque capability has no linear layout")), "{source:?}: {error:?}");
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

    #[test]
    fn ordinary_string_aggregate_stays_linear() {
        let (bundle, families) = opaque_projection(
            vec![heap_definition("Message", vec![named("String")])],
            &[named("Message")],
        );
        assert!(families.is_empty());
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("ordinary aggregate catalog decodes");
        assert!(bundle.roots().iter().any(|root| {
            matches!(layouts.get(*root).map(|layout| layout.kind()), Some(LayoutKind::PackedRecord { .. }))
        }));
    }

    #[test]
    fn opaque_aggregate_ids_are_deterministic_and_disjoint_from_closures() {
        fn families(order: &[&str]) -> std::collections::HashMap<Type, super::OpaqueAggregateFamilyLayout> {
            let definitions = vec![
                heap_definition("Alpha", vec![named("Console")]),
                heap_definition("Zulu", vec![named("Console")]),
            ];
            opaque_projection(definitions, &order.iter().map(|name| named(name)).collect::<Vec<_>>()).1
        }
        let first = families(&["Zulu", "Alpha"]);
        let second = families(&["Alpha", "Zulu"]);
        assert_eq!(first, second);
        assert_eq!(first[&named("Alpha")].owner_type.get(), 0xc200_0000);
        assert_eq!(first[&named("Alpha")].list_type.get(), 0xc200_0001);
        assert_eq!(first[&named("Zulu")].owner_type.get(), 0xc200_0002);
        assert!(first.values().all(|family| family.owner_type.get() >= 0xc200_0000));
    }

    #[test]
    fn public_projection_is_derived_from_checked_source() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"type HeldConsole:\n    console: Console\n\nfn use(value: HeldConsole) -> ():\n    ()\n\nfn main(console: Console):\n    use(HeldConsole(console))\n".to_vec())],
            "main",
            |_, _, _| Ok(Default::default()),
        ).expect("source checks");
        let source = checked.canonical_source().expect("canonical checked source");
        let families = super::opaque_aggregate_families(source)
            .expect("checked source determines opaque aggregate family");
        assert!(families.keys().any(|source| {
            matches!(source, Type::Named(name, _) if name.ends_with("HeldConsole"))
        }));
    }

    #[test]
    fn recursive_container_layouts_keep_structural_lists_and_stable_bindings() {
        fn bundle(list_first: bool) -> witchy_wir::layout::LayoutBundle {
            let list = Type::Named("List".into(), vec![named("Tree")]);
            let mut projection = LayoutProjection::default();
            std::rc::Rc::make_mut(&mut projection.definitions).insert("Tree".into(), heap_definition("Tree", vec![list.clone()]));
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
        std::rc::Rc::make_mut(&mut projection.definitions).insert("A".into(), heap_definition("A", vec![list_b.clone()]));
        std::rc::Rc::make_mut(&mut projection.definitions).insert("B".into(), heap_definition("B", vec![named("A"), list_b]));
        projection.visit_type(&named("A")).expect("mutual source graph");
        let bundle = projection.finish().expect("a body never descends into another nominal body");
        witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("all mutual bindings are closed");
    }

    #[test]
    fn recursive_layout_projection_roundtrips_finite_nominal_binding() {
        let mut projection = LayoutProjection::default();
        std::rc::Rc::make_mut(&mut projection.definitions).insert("Tree".into(), heap_definition("Tree", vec![named("Int"), named("Tree")]));
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
            std::rc::Rc::make_mut(&mut projection.definitions).insert("A".into(), heap_definition("A", vec![named("B")]));
            std::rc::Rc::make_mut(&mut projection.definitions).insert("B".into(), heap_definition("B", vec![named("A")]));
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
            std::rc::Rc::make_mut(&mut projection.definitions).insert("A".into(), heap_definition("A", vec![named("B"), named("C")]));
            std::rc::Rc::make_mut(&mut projection.definitions).insert("B".into(), heap_definition("B", vec![named("A")]));
            std::rc::Rc::make_mut(&mut projection.definitions).insert("C".into(), heap_definition("C", vec![named("B")]));
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
            std::rc::Rc::make_mut(&mut projection.definitions).insert(name.into(), heap_definition(name, vec![named(field)]));
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
            std::rc::Rc::make_mut(&mut projection.definitions).insert("Stack".into(), definition.clone());
            std::rc::Rc::make_mut(&mut projection.definitions).insert("main.Stack".into(), definition);
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
        std::rc::Rc::make_mut(&mut projection.definitions).insert("Stack".into(), definition.clone());
        std::rc::Rc::make_mut(&mut projection.definitions).insert("main.Stack".into(), definition);
        assert!(projection.visit_type(&Type::Named("main.Stack".into(), vec![named("a")])).is_err(),
            "unresolved generic arguments must not acquire a layout");
    }

    #[test]
    fn canonical_closure_key_normalizes_nominals_but_keeps_conventions() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"type Node:\n    value: Int\n\nfn main() -> Int:\n    0\n".to_vec())],
            "main", |_, _, _| Ok(Default::default()),
        ).unwrap();
        let definitions = witchy_types::canonical_aggregates::type_definitions(
            checked.canonical_source().unwrap().typed_module().module());
        let callable = |name: &str, convention| Type::Fn(
            vec![named(name)],
            Box::new(named(name)),
            vec![convention],
            CallableQualifiers::default(),
        );

        let bare_own = super::canonical_closure_key(&callable("Node", Convention::Own), &definitions);
        let qualified_own = super::canonical_closure_key(&callable("main.Node", Convention::Own), &definitions);
        let bare_let = super::canonical_closure_key(&callable("Node", Convention::Let), &definitions);

        assert_eq!(bare_own, qualified_own, "nominal spellings share one closure family");
        assert_ne!(bare_own, bare_let, "ownership convention remains part of closure identity");
    }
}
