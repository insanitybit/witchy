//! Scalar aggregate shapes shared by the canonical layout catalog, the
//! canonical lowering, and the ABI authority.
//!
//! Shapes describe checked source fields, including opaque capabilities and
//! managed owners. They do not authorize linear-memory pointer slots: the
//! catalog must select typed owner storage for opaque fields. A linear
//! aggregate lives behind one reference to a
//! fixed layout with one slot per field. A sum type (`type Shape: Circle(Int)
//! | Square(Int)`, `Option(Int)`, `Result(Int, Bool)`) carries an `Int` tag
//! in slot 0 followed by every variant's fields in declaration order, so each
//! payload has one static slot and a construction initializes every slot.
//! Three consumers must agree on this shape byte for byte: the layout
//! catalog derives the field layout from it, the lowering stores and loads
//! its slots, and the ABI authority binds a nominal source type to a
//! candidate reference whose layout carries exactly these fields.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{effective_type_def_params, Item, Module, Type, TypeDef, Variant};

/// One constructor of a scalar aggregate: its payload occupies `arity`
/// consecutive slots starting at `first_field`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarVariant {
    pub name: String,
    pub first_field: usize,
    pub arity: usize,
}

/// The slot layout of a scalar aggregate type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarAggregateShape {
    /// The nominal name the layout resolver sees (`Point`, `Option`).
    pub name: String,
    /// Every slot's scalar type; slot 0 is the tag of a tagged shape.
    pub fields: Vec<Type>,
    pub variants: Vec<ScalarVariant>,
    /// A sum type carries a tag; a one-variant record does not.
    pub tagged: bool,
    /// A `packed` one-variant record is an inline aggregate rather than a
    /// heap object; its shape is the same.
    pub packed: bool,
}

impl ScalarAggregateShape {
    pub fn variant(&self, name: &str) -> Option<(usize, &ScalarVariant)> {
        self.variants.iter().enumerate().find(|(_, variant)| variant.name == name)
    }

    /// The one-variant packed-record definition the layout interner accepts
    /// for this shape.
    pub fn packed_view(&self) -> TypeDef {
        TypeDef {
            name: self.name.clone(),
            must_consume: false,
            params: Vec::new(),
            variants: vec![Variant {
                name: self.name.clone(),
                line: 0,
                fields: self.fields.clone(),
                field_names: (0..self.fields.len()).map(|index| format!("slot{index}")).collect(),
                field_lines: vec![0; self.fields.len()],
            }],
            derives: Vec::new(),
            sealed: false,
            is_capability: false,
            grantable: false,
            packed: true,
            partial_eq_derived: false,
            public_state_derived: false,
        }
    }
}

pub fn is_scalar_type(ty: &Type) -> bool {
    match ty.unqualified() {
        Type::Tuple(items) => items.is_empty(),
        Type::Named(name, arguments) => {
            arguments.is_empty()
                && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit")
        }
        _ => false,
    }
}

/// A v1 list element: a scalar (a `Bool` element is one byte), a string,
/// a list of those, or an aggregate.
pub fn is_v1_list_element_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    field_type_ok(ty, definitions, &mut BTreeSet::new())
}

/// A field an aggregate may carry: a scalar in its slot, or a string,
/// list, tuple, aggregate, or checked function. The catalog chooses linear
/// pointer slots or typed opaque-owner fields; this predicate grants neither.
pub fn is_v1_field_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    field_type_ok(ty, definitions, &mut BTreeSet::new())
}

/// The canonical representation of `Dict(k, v)`: a tuple of the key list and
/// the value list, kept in insertion order, so every dictionary rule is a
/// tuple-and-list rule. `None` for any other type.
pub fn dictionary_as_tuple(ty: &Type) -> Option<Type> {
    let Type::Named(name, arguments) = ty.unqualified() else { return None };
    if name != "Dict" || arguments.len() != 2 {
        return None;
    }
    Some(Type::Tuple(vec![
        Type::Named("List".into(), vec![arguments[0].clone()]),
        Type::Named("List".into(), vec![arguments[1].clone()]),
    ]))
}

const MAX_AGGREGATE_SHAPE_EXPANSION: usize = 64;
/// Total instantiated AST nodes examined while recognizing one shape. This
/// bounds changing generic recursion before it clones its next expanded field.
const MAX_AGGREGATE_SHAPE_NODES: usize = 4_096;

// Per-query proofs over immutable definitions. A proof records its additional
// expansion height and can be reused wherever that height still fits, with
// every recursion assumption it actually used still active.
// Extra active owners can shorten a traversal, but cannot grant an assumption
// the original proof required. Packed back-edges are rejected before lookup.
#[derive(Default)]
struct ShapeMemo {
    completed: BTreeMap<String, Vec<(usize, BTreeSet<String>)>>,
    packed_keys: BTreeSet<String>,
}

impl ShapeMemo {
    fn new() -> Self {
        Self::default()
    }
}

fn field_type_ok(ty: &Type, definitions: &BTreeMap<String, TypeDef>, active: &mut BTreeSet<String>) -> bool {
    let mut budget = MAX_AGGREGATE_SHAPE_NODES;
    field_type_ok_at(ty, definitions, active, 0, &mut budget, &mut ShapeMemo::new())
}

fn field_type_ok_at(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
    completed: &mut ShapeMemo,
) -> bool {
    let mut observed_depth = depth;
    field_type_ok_at_with_dependencies(
        ty,
        definitions,
        active,
        depth,
        budget,
        completed,
        &mut BTreeSet::new(),
        &mut observed_depth,
    )
}

fn field_type_ok_at_with_dependencies(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
    completed: &mut ShapeMemo,
    required: &mut BTreeSet<String>,
    observed_depth: &mut usize,
) -> bool {
    *observed_depth = (*observed_depth).max(depth);
    if depth == MAX_AGGREGATE_SHAPE_EXPANSION || !consume_shape_nodes(budget, 1) {
        return false;
    }
    // A direct immutable scalar view is not a scalar slot.  It is an
    // authenticated rooted-owner field selected by the canonical layout
    // catalog.  Admit only the installed `&Int` carrier here so an aggregate
    // can describe that field without granting a representation to arbitrary
    // views (in particular, not `&mut`, region, or projected views). A
    // `&[T]` over an admitted element is the rooted-slice owner field.
    if is_direct_rooted_int_view(ty) || owner_carried_view_identity(ty).is_some() {
        return true;
    }
    // This recognizes source fields, not linear-memory slots. Capability
    // fields require the catalog's checked opaque-owner representation.
    match crate::canonical_capabilities::representation(ty, definitions) {
        Some(crate::canonical_capabilities::CapabilityRepresentation::Direct(_)) => return true,
        Some(crate::canonical_capabilities::CapabilityRepresentation::Nullable(_)) => return false,
        None => {}
    }
    if let Some(tuple) = dictionary_as_tuple(ty) {
        return field_type_ok_at_with_dependencies(&tuple, definitions, active, depth + 1, budget, completed, required, observed_depth);
    }
    match ty.unqualified() {
        _ if is_scalar_type(ty) => true,
        Type::Named(name, arguments) if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty() => true,
        // An anonymous union carries its complete, checked constructor shape
        // in the synthetic head: the type arguments are the flattened
        // payload lanes in tag order.  It has no declaration to find in
        // `definitions`, but it is otherwise an ordinary finite aggregate.
        Type::Named(name, arguments)
            if crate::typeck::anon_union_synthetic_variants(name)
                .is_some_and(|variants| variants.iter().map(|(_, arity)| *arity).sum::<usize>() == arguments.len()) =>
        {
            arguments.iter().all(|argument| {
                field_type_ok_at_with_dependencies(
                    argument, definitions, active, depth + 1, budget, completed, required, observed_depth,
                )
            })
        }
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            field_type_ok_at_with_dependencies(&arguments[0], definitions, active, depth + 1, budget, completed, required, observed_depth)
        }
        Type::Named(name, arguments) if matches!(name.as_str(), "Option" | "Result") => {
            arguments.len() == if name == "Option" { 1 } else { 2 }
                && arguments.iter().all(|argument| field_type_ok_at_with_dependencies(argument, definitions, active, depth + 1, budget, completed, required, observed_depth))
        }
        Type::Tuple(items) => !items.is_empty() && items.iter().all(|item| field_type_ok_at_with_dependencies(item, definitions, active, depth + 1, budget, completed, required, observed_depth)),
        Type::Fn(parameters, result, conventions, _) => {
            // The closure occupies an owned pointer slot. Its signature still
            // has to close over supported concrete types; a nominal back-edge
            // uses the same instantiation-sensitive guard as ordinary fields.
            (conventions.is_empty() || conventions.len() == parameters.len())
                && parameters.iter().chain(std::iter::once(result.as_ref())).all(|lane| {
                    if is_callable_only_lane(lane, definitions) {
                        consume_shape_nodes(budget, 1)
                    } else {
                        field_type_ok_at_with_dependencies(lane, definitions, active, depth + 1, budget, completed, required, observed_depth)
                    }
                })
        }
        Type::Named(name, arguments) => {
            definitions.get(name).is_some_and(|definition| {
                definition_fields_ok(definition, arguments, definitions, active, depth + 1, budget, completed, required, observed_depth)
            })
        }
        _ => false,
    }
}

// Capability parameters/results have callable ABI lanes without granting a
// linear-memory field representation. Unit also has its own scalar layout.
fn is_callable_only_lane(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    crate::pipeline::is_unit_type(ty)
        || crate::canonical_capabilities::representation(ty, definitions).is_some()
}

fn definition_fields_ok(
    definition: &TypeDef,
    arguments: &[Type],
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
    completed: &mut ShapeMemo,
    required: &mut BTreeSet<String>,
    observed_depth: &mut usize,
) -> bool {
    let parameters = effective_type_def_params(definition);
    if parameters.len() != arguments.len()
        || !is_aggregate_definition(definition)
        || definition.variants.is_empty()
        || depth == MAX_AGGREGATE_SHAPE_EXPANSION
    {
        return false;
    }
    // Arguments are part of the concrete instantiation even when a phantom
    // parameter has no field occurrence. Do not let an unused `a` smuggle an
    // unresolved or unsupported type through shape recognition.
    let mut local_required = BTreeSet::new();
    let mut local_max_depth = depth;
    if !arguments.iter().all(|argument| field_type_ok_at_with_dependencies(argument, definitions, active, depth + 1, budget, completed, &mut local_required, &mut local_max_depth)) {
        return false;
    }
    // Validate arguments before cloning/rendering a key: memo lookup must not
    // bypass the depth and work limits for an oversized generic argument.
    // `aggregate_key` on a `Type::Named` is exactly `render_type` of it, so
    // build the key straight from the parts instead of materializing a
    // `Type::Named` (a name clone plus a copy of every argument) that nothing
    // else reads.
    let key = {
        let mut key = String::new();
        if !render_named_into(&definition.name, arguments, &mut key) {
            return false;
        }
        key
    };
    // This recognizes a finite source shape graph, not allocation authority.
    // Recursive occurrences need an owned nominal binding in the layout layer;
    // an inline packed occurrence cannot close the graph this way.
    if definition.packed {
        completed.packed_keys.insert(key.clone());
    }
    if active.contains(&key) {
        local_required.insert(key);
        required.extend(local_required);
        *observed_depth = (*observed_depth).max(local_max_depth);
        return !definition.packed;
    }
    // Additional active owners can close more finite graph edges, but an
    // additional packed ancestor can invalidate a descendant hidden by reuse.
    // Every packed key encountered while deriving a proof is retained here.
    if let Some((height, assumptions)) = completed.completed.get(&key).and_then(|proofs| proofs.iter().find_map(|(height, assumptions)| {
        (depth.checked_add(*height).is_some_and(|maximum| maximum < MAX_AGGREGATE_SHAPE_EXPANSION)
            && assumptions.is_subset(active)
            && active.difference(assumptions).all(|key| !completed.packed_keys.contains(key)))
            .then(|| (*height, assumptions.clone()))
    })) {
        local_required.extend(assumptions);
        required.extend(local_required);
        *observed_depth = (*observed_depth).max(depth + height);
        return true;
    }
    active.insert(key.clone());
    // Borrow the parameters and arguments instead of cloning every argument's
    // whole type tree into the map. `substitute_type` still clones the one
    // argument it actually substitutes, which is the only copy that has to
    // exist; building the map no longer copies arguments a field never names.
    let substitutions: BTreeMap<&str, &Type> =
        parameters.iter().map(String::as_str).zip(arguments.iter()).collect();
    let valid = definition.variants.iter().all(|variant| {
        variant.fields.iter().all(|field| {
            let Some(nodes) = substituted_node_count(field, &substitutions, *budget) else { return false };
            if !consume_shape_nodes(budget, nodes) {
                return false;
            }
            // An empty substitution is the identity. Keep the same node
            // accounting and recursive checks, but borrow the immutable
            // declaration instead of cloning its complete field-type tree.
            if substitutions.is_empty() {
                field_type_ok_at_with_dependencies(field, definitions, active, depth + 1, budget, completed, &mut local_required, &mut local_max_depth)
            } else {
                substitute_type(field, &substitutions)
                    .is_some_and(|field| field_type_ok_at_with_dependencies(&field, definitions, active, depth + 1, budget, completed, &mut local_required, &mut local_max_depth))
            }
        })
    });
    active.remove(&key);
    local_required.remove(&key);
    if valid { completed.completed.entry(key).or_default().push((local_max_depth.saturating_sub(depth), local_required.clone())); }
    required.extend(local_required);
    *observed_depth = (*observed_depth).max(local_max_depth);
    valid
}

fn consume_shape_nodes(budget: &mut usize, nodes: usize) -> bool {
    if nodes > *budget {
        return false;
    }
    *budget -= nodes;
    true
}

/// Count the result of substitution without constructing it. `None` means the
/// result exceeds the remaining global work budget or arithmetic overflowed.
fn substituted_node_count(ty: &Type, substitutions: &BTreeMap<&str, &Type>, limit: usize) -> Option<usize> {
    let count = match ty {
        Type::Named(name, arguments) if arguments.is_empty() && substitutions.contains_key(name.as_str()) => {
            type_node_count(substitutions[name.as_str()], limit)?
        }
        Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) => {
            arguments.iter().try_fold(1usize, |count, argument| {
                count.checked_add(substituted_node_count(argument, substitutions, limit.saturating_sub(count))?)
            })?
        }
        Type::RecordCompose { base, fields } => fields.iter().try_fold(
            1usize.checked_add(substituted_node_count(base, substitutions, limit.saturating_sub(1))?)?,
            |count, (_, field)| count.checked_add(substituted_node_count(field, substitutions, limit.saturating_sub(count))?),
        )?,
        Type::Fn(parameters, result, _, _) => parameters.iter().try_fold(
            1usize,
            |count, parameter| count.checked_add(substituted_node_count(parameter, substitutions, limit.saturating_sub(count))?),
        )?.checked_add(substituted_node_count(result, substitutions, limit)?)?,
        Type::Qualified(_, inner) | Type::Slice(inner) => {
            1usize.checked_add(substituted_node_count(inner, substitutions, limit.saturating_sub(1))?)?
        }
    };
    (count <= limit).then_some(count)
}

fn type_node_count(ty: &Type, limit: usize) -> Option<usize> {
    substituted_node_count(ty, &BTreeMap::new(), limit)
}

/// Substitute a declaration's type parameters through the whole source type.
/// An unbound parameter is left intact so the normal field recognizer rejects
/// it as an unknown concrete aggregate rather than inventing a representation.
fn substitute_type(ty: &Type, substitutions: &BTreeMap<&str, &Type>) -> Option<Type> {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() && substitutions.contains_key(name.as_str()) => {
            Some(substitutions[name.as_str()].clone())
        }
        Type::Named(name, arguments) => Some(Type::Named(
            name.clone(),
            arguments.iter().map(|argument| substitute_type(argument, substitutions)).collect::<Option<Vec<_>>>()?,
        )),
        Type::Tuple(items) => Some(Type::Tuple(
            items.iter().map(|item| substitute_type(item, substitutions)).collect::<Option<Vec<_>>>()?,
        )),
        Type::RecordCompose { base, fields } => Some(Type::RecordCompose {
            base: Box::new(substitute_type(base, substitutions)?),
            fields: fields.iter().map(|(name, field)| Some((name.clone(), substitute_type(field, substitutions)?))).collect::<Option<Vec<_>>>()?,
        }),
        Type::Fn(parameters, result, conventions, qualifiers) => Some(Type::Fn(
            parameters.iter().map(|parameter| substitute_type(parameter, substitutions)).collect::<Option<Vec<_>>>()?,
            Box::new(substitute_type(result, substitutions)?),
            conventions.clone(),
            qualifiers.clone(),
        )),
        Type::Dyn(name, arguments) => Some(Type::Dyn(
            name.clone(),
            arguments.iter().map(|argument| substitute_type(argument, substitutions)).collect::<Option<Vec<_>>>()?,
        )),
        Type::Qualified(qualifier, inner) => Some(Type::Qualified(qualifier.clone(), Box::new(substitute_type(inner, substitutions)?))),
        Type::Slice(element) => Some(Type::Slice(Box::new(substitute_type(element, substitutions)?)),),
    }
}

fn render_type(ty: &Type) -> Option<String> {
    let mut rendered = String::new();
    render_into(ty, &mut rendered).then_some(rendered)
}

/// Render into ONE buffer instead of returning a fresh `String` per node.
///
/// The recursive form allocated a `String` for every nested argument, pushed
/// it into its parent and dropped it immediately. On an async `check` that
/// showed up as the dominant allocator traffic under
/// `field_type_ok_at_with_dependencies`, which is itself the hottest frame in
/// the initial SafeWIR check. The bytes produced are identical -- these keys
/// are compared against memo entries written by the same code, so they must
/// not drift.
///
/// A refused node leaves partial text in the buffer. Every caller discards the
/// buffer on `false`, so no partial key can reach a memo.
fn render_into(ty: &Type, out: &mut String) -> bool {
    match ty.unqualified() {
        Type::Tuple(items) => {
            out.push('(');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                if !render_into(item, out) {
                    return false;
                }
            }
            out.push(')');
            true
        }
        Type::Named(name, arguments) => render_named_into(name, arguments, out),
        // Keep the complete checked signature in generic-instantiation keys,
        // including conventions and qualifiers, rather than an erased Fn name.
        Type::Fn(..) => {
            use std::fmt::Write as _;
            let _ = write!(out, "{:?}", ty.unqualified());
            true
        }
        // A `&[T]` argument (`Task(&[Int])`); the rooted-slice owner.
        Type::Slice(element) => {
            out.push('[');
            if !render_into(element, out) {
                return false;
            }
            out.push(']');
            true
        }
        _ => false,
    }
}

/// Render a named type from its parts, so a caller holding a name and an
/// argument slice does not have to build a `Type::Named` (cloning the name and
/// the whole argument vector) purely to produce a memo key.
fn render_named_into(name: &str, arguments: &[Type], out: &mut String) -> bool {
    out.push_str(name);
    if arguments.is_empty() {
        return true;
    }
    out.push('(');
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        if !render_into(argument, out) {
            return false;
        }
    }
    out.push(')');
    true
}

fn int_type() -> Type {
    Type::Named("Int".into(), Vec::new())
}

/// The key a source type's aggregate shape is filed under: the bare name of
/// a user type, or the name with its scalar arguments (`Option(Int)`).
pub fn aggregate_key(ty: &Type) -> Option<String> {
    match ty.unqualified() {
        Type::Named(..) | Type::Tuple(..) => render_type(ty),
        _ => None,
    }
}

/// Normalize checked declaration names and concrete arguments for layout
/// identity. This does not validate or expand bodies: the bounded aggregate
/// shape checker owns that separate obligation.
pub fn concrete_aggregate_identity(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    if !shape_memo::worth_memoizing(ty, definitions) {
        return concrete_identity_at(ty, definitions, 0, &mut MAX_AGGREGATE_SHAPE_NODES);
    }
    if let Some(known) = shape_memo::identity(definitions, ty) {
        return known;
    }
    let identity = concrete_identity_at(ty, definitions, 0, &mut MAX_AGGREGATE_SHAPE_NODES);
    shape_memo::remember_identity(definitions, ty, &identity);
    identity
}

/// Run `body` with the shape and identity judgments for `definitions`
/// memoized on this thread.
///
/// Both judgments are pure functions of a type and the definitions, but the
/// proof behind them (`definition_fields_ok`) starts from an empty memo on
/// every call and explores the type graph within its budget, and a lowering
/// asks for the same few shapes thousands of times (every list update of the
/// std executor rebuilds an aggregate). The scope is keyed by the address of
/// the borrowed map, which cannot move or change while the borrow lives, and
/// it is popped when `body` returns, so a map created afterwards at the same
/// address never sees its entries. A call with a map that is not the scoped
/// one is answered without the memo.
pub fn with_shape_memo<R>(definitions: &BTreeMap<String, TypeDef>, body: impl FnOnce() -> R) -> R {
    shape_memo::scoped(definitions, body)
}

mod shape_memo {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use foldhash::HashMap;
    use witchy_syntax::ast::{Type, TypeDef};

    use super::ScalarAggregateShape;

    struct Scope {
        definitions: usize,
        shapes: HashMap<Type, Option<ScalarAggregateShape>>,
        identities: HashMap<Type, Option<Type>>,
        owner_identities: HashMap<Type, Option<Type>>,
        derived: HashMap<&'static str, HashMap<Type, Type>>,
    }

    thread_local! {
        static SCOPES: RefCell<Vec<Scope>> = const { RefCell::new(Vec::new()) };
    }

    /// Only a user-defined aggregate reaches the shape proof; a scalar, a
    /// builtin container, or a tuple is judged structurally in less time
    /// than rendering a key would take.
    pub(super) fn worth_memoizing(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
        matches!(ty.unqualified(), Type::Named(name, _) if definitions.contains_key(name))
    }

    // The memo keys on the `Type` itself. It was keyed on `format!("{ty:?}")`,
    // which allocated and rendered a fresh string on EVERY probe: measured
    // 140,000+ probes on one async compile at ~32 bytes each. `Type` already
    // derives `Hash`, `Eq` and `Clone`, so hashing it structurally is both
    // cheaper and exact, and only an insert now clones.
    fn address(definitions: &BTreeMap<String, TypeDef>) -> usize {
        std::ptr::from_ref(definitions) as usize
    }

    pub(super) fn scoped<R>(definitions: &BTreeMap<String, TypeDef>, body: impl FnOnce() -> R) -> R {
        // A scope for this map is already open further up the stack: its
        // borrow is still live, so it is the same map, and its entries stay
        // valid. Share them rather than starting a cold nested scope.
        let wanted = address(definitions);
        if SCOPES.with(|scopes| scopes.borrow().iter().any(|scope| scope.definitions == wanted)) {
            return body();
        }
        SCOPES.with(|scopes| {
            scopes.borrow_mut().push(Scope {
                definitions: wanted,
                shapes: HashMap::default(),
                identities: HashMap::default(),
                owner_identities: HashMap::default(),
                derived: HashMap::default(),
            });
        });
        struct Pop;
        impl Drop for Pop {
            fn drop(&mut self) {
                SCOPES.with(|scopes| {
                    scopes.borrow_mut().pop();
                });
            }
        }
        let _pop = Pop;
        body()
    }

    fn with_scope<R>(definitions: &BTreeMap<String, TypeDef>, read: impl FnOnce(&mut Scope) -> R) -> Option<R> {
        let wanted = address(definitions);
        SCOPES.with(|scopes| {
            let mut scopes = scopes.borrow_mut();
            let scope = scopes.iter_mut().rev().find(|scope| scope.definitions == wanted)?;
            Some(read(scope))
        })
    }

    pub(super) fn shape(definitions: &BTreeMap<String, TypeDef>, ty: &Type) -> Option<Option<ScalarAggregateShape>> {
        with_scope(definitions, |scope| scope.shapes.get(ty).cloned()).flatten()
    }

    pub(super) fn remember_shape(definitions: &BTreeMap<String, TypeDef>, ty: &Type, shape: &Option<ScalarAggregateShape>) {
        with_scope(definitions, |scope| {
            scope.shapes.insert(ty.clone(), shape.clone());
        });
    }

    pub(super) fn identity(definitions: &BTreeMap<String, TypeDef>, ty: &Type) -> Option<Option<Type>> {
        with_scope(definitions, |scope| scope.identities.get(ty).cloned()).flatten()
    }

    pub(super) fn remember_identity(definitions: &BTreeMap<String, TypeDef>, ty: &Type, identity: &Option<Type>) {
        with_scope(definitions, |scope| {
            scope.identities.insert(ty.clone(), identity.clone());
        });
    }

    pub(super) fn derived(definitions: &BTreeMap<String, TypeDef>, kind: &'static str, ty: &Type) -> Option<Type> {
        with_scope(definitions, |scope| scope.derived.get(kind).and_then(|by_type| by_type.get(ty)).cloned()).flatten()
    }

    pub(super) fn remember_derived(definitions: &BTreeMap<String, TypeDef>, kind: &'static str, ty: &Type, derived: &Type) {
        with_scope(definitions, |scope| {
            scope.derived.entry(kind).or_default().insert(ty.clone(), derived.clone());
        });
    }

    pub(super) fn owner_identity(definitions: &BTreeMap<String, TypeDef>, ty: &Type) -> Option<Option<Type>> {
        with_scope(definitions, |scope| scope.owner_identities.get(ty).cloned()).flatten()
    }

    pub(super) fn remember_owner_identity(definitions: &BTreeMap<String, TypeDef>, ty: &Type, identity: &Option<Type>) {
        with_scope(definitions, |scope| {
            scope.owner_identities.insert(ty.clone(), identity.clone());
        });
    }
}

/// Memoize `derive(ty)` in the open shape-memo scope for `definitions`,
/// under a caller-chosen `kind` that names the derivation. `derive` must be
/// a pure function of `ty` and `definitions`; outside a scope for this map
/// it simply runs.
pub fn memoize_derived_type(
    kind: &'static str,
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    derive: impl FnOnce() -> Type,
) -> Type {
    if let Some(known) = shape_memo::derived(definitions, kind, ty) {
        return known;
    }
    let derived = derive();
    shape_memo::remember_derived(definitions, kind, ty, &derived);
    derived
}

/// The aggregate identity that authorizes typed opaque owner fields.
///
/// Ordinary aggregates use the concrete physical identity.  A non-packed
/// aggregate with a direct `&Int` or exact `&[Int]` field is the bounded
/// exception: that field carries an authenticated rooted-view owner, so erasing the nested `View`
/// qualifier would change the carrier into linear storage.  Preserve the
/// qualifier at direct aggregate arguments and through `Option` nesting of
/// such a carrier (`Option(Option(&Int))`), whose every shell is itself an
/// opaque owner; other nested carriers remain outside this ABI until they
/// have their own provenance contract.
pub fn canonical_owner_aggregate_identity(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    // Layout projection, lowering and both authority checks ask this for the
    // same few types thousands of times; the view walk behind it is not
    // itself memoized.
    if !matches!(ty.unqualified(), Type::Named(..) | Type::Tuple(_)) {
        return canonical_owner_aggregate_identity_uncached(ty, definitions);
    }
    if let Some(known) = shape_memo::owner_identity(definitions, ty) {
        return known;
    }
    let identity = canonical_owner_aggregate_identity_uncached(ty, definitions);
    shape_memo::remember_owner_identity(definitions, ty, &identity);
    identity
}

fn canonical_owner_aggregate_identity_uncached(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    let identity = concrete_aggregate_identity(ty, definitions)?;
    let carries_direct_rooted_view = scalar_aggregate_shape(ty, definitions).is_some_and(|shape| {
        !shape.packed && shape.fields.iter().any(|field| is_rooted_int_view_carrier(field, definitions))
    });
    if !carries_direct_rooted_view {
        return Some(identity);
    }
    match (ty.unqualified(), identity) {
        (Type::Named(_, source_arguments), Type::Named(name, arguments))
            if source_arguments.len() == arguments.len() =>
        {
            Some(Type::Named(name, source_arguments.iter().zip(arguments).map(|(source, identity)| {
                if is_rooted_int_view_carrier(source, definitions) { source.clone() } else { identity }
            }).collect()))
        }
        (Type::Tuple(source_items), Type::Tuple(items)) if source_items.len() == items.len() => {
            Some(Type::Tuple(source_items.iter().zip(items).map(|(source, identity)| {
                if is_rooted_int_view_carrier(source, definitions) { source.clone() } else { identity }
            }).collect()))
        }
        // A nominal declaration can carry a direct rooted view in one of its
        // fields even though its outer identity has no type argument.  The
        // normalized declaration identity is already exact in that case.
        (_, identity) => Some(identity),
    }
}

/// A `&Float`, `&Bool`, or `&Duration` view (`ScalarValue`): carried as the
/// immutable scalar value itself.
pub fn is_scalar_value_view_payload(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Named(name, arguments)
        if arguments.is_empty() && matches!(name.as_str(), "Float" | "Bool" | "Duration"))
}

/// A whole `&List(T)` view (`ListRoot`): elements are one scalar kind or
/// String, so the list holds no view, owner, or capability.
pub fn is_list_root_payload(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Named(name, arguments)
        if name == "List" && matches!(arguments.as_slice(), [element]
            if matches!(element.unqualified(), Type::Named(element, element_arguments)
                if element_arguments.is_empty()
                    && matches!(element.as_str(), "Int" | "Float" | "Duration" | "Bool" | "String"))))
}

/// A whole view of this value is a `RecordRoot`: the retained aggregate
/// object is the snapshot.  Admitted for a declared user record or sum, or
/// builtin `Option`/`Result`, with a non-packed heap shape that holds no view
/// (a view inside would need its own carrier).
pub fn is_record_root_payload(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    let Type::Named(name, arguments) = ty.unqualified() else { return false };
    let head = matches!((name.as_str(), arguments.len()), ("Option", 1) | ("Result", 2))
        || definitions.contains_key(name);
    head && crate::canonical_capabilities::representation(ty, definitions).is_none()
        && scalar_aggregate_shape(ty, definitions).is_some_and(|shape| !shape.packed)
        && !contains_view(ty, definitions, &mut BTreeSet::new())
}

/// Nesting bound for view carriers. A recursive nominal cannot make the
/// carrier judgment diverge; anything deeper simply is not a carrier.
const MAX_VIEW_CARRIER_DEPTH: usize = 8;

/// A value that carries authenticated `&Int` or exact `&[Int]` views as opaque owners: the
/// direct view itself, or a non-packed aggregate (tuple, record, sum,
/// `Option`, `Result`) each of whose fields is either such a carrier or holds
/// no view at all, with at least one carrier field. Every shell on the path is
/// its own opaque owner whose carrier fields are the next owner inward, so a
/// view is never stored in linear memory.
pub fn is_rooted_int_view_carrier(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    may_carry_view(ty, definitions) && view_carrier_at(ty, definitions, 0)
}

/// [`is_rooted_int_view_carrier`] minus the bare view: an aggregate owner.
pub fn is_rooted_int_view_aggregate(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    may_carry_view(ty, definitions) && view_aggregate_at(ty, definitions, 0)
}

fn view_carrier_at(ty: &Type, definitions: &BTreeMap<String, TypeDef>, depth: usize) -> bool {
    is_direct_rooted_int_view(ty) || is_direct_rooted_int_slice_view(ty)
        || view_aggregate_at(ty, definitions, depth)
}

/// Every layout identity asks this question, and almost no type holds a
/// view. Answer those with one syntactic walk before expanding any shape.
fn may_carry_view(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    contains_view(ty, definitions, &mut BTreeSet::new())
}

fn view_aggregate_at(ty: &Type, definitions: &BTreeMap<String, TypeDef>, depth: usize) -> bool {
    if depth >= MAX_VIEW_CARRIER_DEPTH
        || !matches!(ty.unqualified(), Type::Named(..) | Type::Tuple(_))
    {
        return false;
    }
    let Some(shape) = scalar_aggregate_shape(ty, definitions) else { return false };
    if shape.packed { return false; }
    let mut carriers = 0usize;
    let admitted = shape.fields.iter().all(|field| {
        if !may_carry_view(field, definitions) {
            true
        } else if view_carrier_at(field, definitions, depth + 1) {
            carriers += 1;
            true
        } else {
            false
        }
    });
    admitted && carriers > 0
}

/// Whether any view hides in `ty`, through generic arguments, tuples, named
/// declarations, and callable signatures.
fn contains_view(ty: &Type, definitions: &BTreeMap<String, TypeDef>, active: &mut BTreeSet<String>) -> bool {
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, _) => true,
        Type::Qualified(_, inner) | Type::Slice(inner) => contains_view(inner, definitions, active),
        Type::Named(name, arguments) | Type::Dyn(name, arguments) => {
            arguments.iter().any(|argument| contains_view(argument, definitions, active))
                || definitions.get(name).is_some_and(|definition| {
                    if !active.insert(name.clone()) { return false; }
                    let found = definition.variants.iter().any(|variant| {
                        variant.fields.iter().any(|field| contains_view(field, definitions, active))
                    });
                    active.remove(name);
                    found
                })
        }
        Type::Tuple(items) => items.iter().any(|item| contains_view(item, definitions, active)),
        Type::RecordCompose { base, fields } => contains_view(base, definitions, active)
            || fields.iter().any(|(_, field)| contains_view(field, definitions, active)),
        Type::Fn(parameters, result, _, _) => parameters.iter().any(|parameter| contains_view(parameter, definitions, active))
            || contains_view(result, definitions, active),
        _ => false,
    }
}

fn is_direct_rooted_int_view(ty: &Type) -> bool {
    matches!(ty, Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
        if matches!(inner.unqualified(), Type::Named(name, arguments)
            if name == "Int" && arguments.is_empty()))
}

fn is_direct_rooted_int_slice_view(ty: &Type) -> bool {
    let Type::Qualified(witchy_syntax::ast::TypeQual::View, payload) = ty else {
        return false;
    };
    let element = match payload.as_ref() {
        Type::Slice(element) => element.as_ref(),
        // Type checking normalizes surface `[Int]` to its private nominal.
        Type::Named(name, arguments) if name == "Slice" && arguments.len() == 1 => &arguments[0],
        _ => return false,
    };
    matches!(element, Type::Named(name, arguments)
        if name == "Int" && arguments.is_empty())
}

fn aggregate_declaration<'a>(
    name: &str,
    definitions: &'a BTreeMap<String, TypeDef>,
) -> Option<(&'a str, &'a TypeDef)> {
    if name.contains('.') {
        return definitions.get_key_value(name).map(|(key, definition)| (key.as_str(), definition));
    }
    let bare = definitions.get_key_value(name);
    let mut qualified = definitions.iter().filter(|(key, _)| {
        key.contains('.') && key.rsplit('.').next() == Some(name)
    });
    let first = qualified.next();
    if qualified.next().is_some() {
        return None;
    }
    match (bare, first) {
        (Some((_, bare)), Some((key, definition))) if bare == definition => Some((key.as_str(), definition)),
        (None, Some((key, definition))) => Some((key.as_str(), definition)),
        (Some((key, definition)), None) => Some((key.as_str(), definition)),
        _ => None,
    }
}

fn concrete_identity_at(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    depth: usize,
    budget: &mut usize,
) -> Option<Type> {
    if depth >= MAX_AGGREGATE_SHAPE_EXPANSION || !consume_shape_nodes(budget, 1) {
        return None;
    }
    if matches!(crate::canonical_capabilities::representation(ty, definitions),
        Some(crate::canonical_capabilities::CapabilityRepresentation::Direct(_))) {
        return Some(ty.unqualified().clone());
    }
    // An owner-carried view (`&Int`, `&[T]`) has its own opaque GC family,
    // never its payload's representation, so erasing it here would give
    // `Task(&Int)` and `Task(Int)` (and their callables) one identity with two
    // physical carriers. Token-carried views share their payload's layout.
    if let Some(view) = owner_carried_view_identity(ty) {
        return Some(view);
    }
    match ty.unqualified() {
        ty if is_scalar_type(ty) => Some(ty.clone()),
        Type::Tuple(items) => Some(Type::Tuple(items.iter()
            .map(|item| concrete_identity_at(item, definitions, depth + 1, budget))
            .collect::<Option<Vec<_>>>()?)),
        Type::Fn(parameters, result, conventions, qualifiers) => {
            if !conventions.is_empty() && conventions.len() != parameters.len() {
                return None;
            }
            Some(Type::Fn(
                parameters.iter().map(|parameter| {
                    concrete_callable_lane_identity_at(parameter, definitions, depth + 1, budget)
                }).collect::<Option<Vec<_>>>()?,
                Box::new(concrete_callable_lane_identity_at(result, definitions, depth + 1, budget)?),
                conventions.clone(),
                *qualifiers,
            ))
        }
        Type::Named(name, arguments) => {
            let (canonical_name, arity) = match name.as_str() {
                "String" | "Bytes" => (name.as_str(), 0),
                "List" | "Option" => (name.as_str(), 1),
                "Result" | "Dict" => (name.as_str(), 2),
                _ => {
                    let (key, definition) = aggregate_declaration(name, definitions)?;
                    if !is_aggregate_definition(definition) {
                        return None;
                    }
                    (key, effective_type_def_params(definition).len())
                }
            };
            if arguments.len() != arity {
                return None;
            }
            Some(Type::Named(canonical_name.to_owned(), arguments.iter()
                .map(|argument| concrete_identity_at(argument, definitions, depth + 1, budget))
                .collect::<Option<Vec<_>>>()?))
        }
        _ => None,
    }
}

/// The identity of a direct `&Int` or `&[T]` (admitted element) view, with
/// the checker's private `Slice(T)` nominal spelled as the surface `[T]`.
/// Every normalizer that erases qualifiers for a physical identity (aggregate
/// identity, closure family keys in lowering and in authority) keeps exactly
/// these views: each has its own opaque owner family, so erasing one would
/// alias two distinct physical carriers under one identity.
pub fn owner_carried_view_identity(ty: &Type) -> Option<Type> {
    let Type::Qualified(witchy_syntax::ast::TypeQual::View, inner) = ty else { return None };
    // Exactly the admitted slice elements (`ROOTED_SLICE_ELEMENTS`), matched
    // through `frozen` (the same immutable payload) but through no other
    // nested qualifier.
    let payload_of = |ty: &Type| match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::Frozen, inner) => inner.as_ref().clone(),
        other => other.clone(),
    };
    let scalar = |ty: &Type| matches!(payload_of(ty), Type::Named(ref name, ref arguments)
        if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool" | "Duration" | "String"));
    let payload = match &payload_of(inner) {
        Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => payload_of(inner),
        Type::Slice(element) if scalar(element) => Type::Slice(Box::new(payload_of(element))),
        Type::Named(name, arguments) if name == "Slice" && arguments.len() == 1 && scalar(&arguments[0]) => {
            Type::Slice(Box::new(payload_of(&arguments[0])))
        }
        _ => return None,
    };
    Some(Type::Qualified(witchy_syntax::ast::TypeQual::View, Box::new(payload)))
}

fn concrete_callable_lane_identity_at(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    depth: usize,
    budget: &mut usize,
) -> Option<Type> {
    if depth >= MAX_AGGREGATE_SHAPE_EXPANSION || !consume_shape_nodes(budget, 1) {
        return None;
    }
    if let Some(view) = owner_carried_view_identity(ty) {
        return Some(view);
    }
    if let Type::Qualified(qualifier, inner) = ty {
        return Some(Type::Qualified(qualifier.clone(), Box::new(
            concrete_callable_lane_identity_at(inner, definitions, depth + 1, budget)?,
        )));
    }
    if is_callable_only_lane(ty, definitions) {
        return Some(ty.clone());
    }
    concrete_identity_at(ty, definitions, depth, budget)
}

/// The module's type definitions keyed by bare and entry-qualified names.
pub fn type_definitions(module: &Module) -> BTreeMap<String, TypeDef> {
    let mut definitions = BTreeMap::new();
    for item in &module.items {
        let Item::Type(definition) = item else { continue };
        if let Some(entry) = module.linked_entry.as_deref()
            && !definition.name.contains('.')
        {
            definitions.insert(format!("{entry}.{}", definition.name), definition.clone());
        }
        definitions.insert(definition.name.clone(), definition.clone());
    }
    definitions
}

/// Carried-state capabilities retain their sealed nominal declaration. They
/// are aggregate owners, never newly invented host capability kinds.
fn is_aggregate_definition(definition: &TypeDef) -> bool {
    !definition.is_capability || (definition.sealed && !definition.packed
        && definition.variants.len() == 1
        && !definition.variants[0].fields.is_empty()
        && definition.variants[0].field_names.len() == definition.variants[0].fields.len())
}

/// The shape of a user-declared type whose every variant carries only
/// scalar fields, or `None` for any other definition.
pub fn shape_from_definition(definition: &TypeDef, definitions: &BTreeMap<String, TypeDef>) -> Option<ScalarAggregateShape> {
    shape_from_instantiation(definition, &[], definitions)
}

fn shape_from_instantiation(
    definition: &TypeDef,
    arguments: &[Type],
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ScalarAggregateShape> {
    let mut budget = MAX_AGGREGATE_SHAPE_NODES;
    let mut observed_depth = 0;
    if !definition_fields_ok(
        definition,
        arguments,
        definitions,
        &mut BTreeSet::new(),
        0,
        &mut budget,
        &mut ShapeMemo::new(),
        &mut BTreeSet::new(),
        &mut observed_depth,
    ) {
        return None;
    }
    let parameters = effective_type_def_params(definition);
    let substitutions: BTreeMap<&str, &Type> =
        parameters.iter().map(String::as_str).zip(arguments.iter()).collect();
    let name = definition.name.rsplit('.').next().unwrap_or(&definition.name).to_owned();
    if definition.variants.len() == 1 {
        let variant = &definition.variants[0];
        return Some(ScalarAggregateShape {
            name,
            fields: variant.fields.iter().map(|field| substitute_type(field, &substitutions)).collect::<Option<Vec<_>>>()?,
            variants: vec![ScalarVariant { name: variant.name.clone(), first_field: 0, arity: variant.fields.len() }],
            tagged: false,
            packed: definition.packed,
        });
    }
    let mut fields = vec![int_type()];
    let mut variants = Vec::with_capacity(definition.variants.len());
    for variant in &definition.variants {
        variants.push(ScalarVariant { name: variant.name.clone(), first_field: fields.len(), arity: variant.fields.len() });
        fields.extend(variant.fields.iter().map(|field| substitute_type(field, &substitutions)).collect::<Option<Vec<_>>>()?);
    }
    Some(ScalarAggregateShape { name, fields, variants, tagged: true, packed: false })
}

#[cfg(test)]
mod shape_memo_tests {
    use super::*;

    fn record(name: &str, fields: Vec<Type>) -> TypeDef {
        ScalarAggregateShape { name: name.into(), fields, variants: Vec::new(), tagged: false, packed: false }
            .packed_view()
    }

    #[test]
    fn scoped_memo_answers_like_the_uncached_judgment_and_ignores_other_maps() {
        let point = Type::Named("Point".into(), Vec::new());
        let mut definitions = BTreeMap::new();
        definitions.insert("Point".to_string(), record("Point", vec![int_type(), int_type()]));
        let mut other = BTreeMap::new();
        other.insert("Point".to_string(), record("Point", vec![int_type()]));

        let uncached = scalar_aggregate_shape(&point, &definitions);
        let uncached_other = scalar_aggregate_shape(&point, &other);
        assert_ne!(uncached, uncached_other);
        with_shape_memo(&definitions, || {
            assert_eq!(scalar_aggregate_shape(&point, &definitions), uncached);
            assert_eq!(scalar_aggregate_shape(&point, &definitions), uncached);
            assert_eq!(concrete_aggregate_identity(&point, &definitions), Some(point.clone()));
            // A map the scope does not cover is judged afresh, not from the memo.
            assert_eq!(scalar_aggregate_shape(&point, &other), uncached_other);
            with_shape_memo(&other, || {
                assert_eq!(scalar_aggregate_shape(&point, &other), uncached_other);
                assert_eq!(scalar_aggregate_shape(&point, &definitions), uncached);
            });
            assert_eq!(scalar_aggregate_shape(&point, &definitions), uncached);
        });
        assert_eq!(scalar_aggregate_shape(&point, &definitions), uncached);
    }
}

#[cfg(test)]
mod recursive_shape_tests {
    use super::*;

    fn named(name: &str) -> Type {
        Type::Named(name.into(), Vec::new())
    }

    fn definition(name: &str, fields: Vec<Type>, packed: bool) -> TypeDef {
        let mut definition = ScalarAggregateShape {
            name: name.into(), fields, variants: Vec::new(), tagged: false, packed,
        }.packed_view();
        definition.packed = packed;
        definition
    }

    fn generic_definition(name: &str, parameters: &[&str], fields: Vec<Type>, packed: bool) -> TypeDef {
        let mut definition = definition(name, fields, packed);
        definition.params = parameters.iter().map(|parameter| (*parameter).to_owned()).collect();
        definition
    }

    #[test]
    fn record_slice_carrier_stays_exactly_int() {
        let view_slice = |element| Type::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(Type::Slice(Box::new(element))),
        );
        let int_slice = view_slice(named("Int"));
        let packet = named("Packet");
        let definitions = BTreeMap::from([(
            "Packet".into(), definition("Packet", vec![int_slice.clone()], false),
        )]);
        assert!(is_rooted_int_view_carrier(&int_slice, &definitions));
        assert!(is_rooted_int_view_aggregate(&packet, &definitions));
        let normalized_int_slice = Type::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(Type::Named("Slice".into(), vec![named("Int")])),
        );
        assert!(is_rooted_int_view_carrier(&normalized_int_slice, &definitions));

        for element in [
            named("Float"),
            Type::Named("List".into(), vec![named("Int")]),
            Type::Qualified(witchy_syntax::ast::TypeQual::View, Box::new(named("Int"))),
            Type::Qualified(witchy_syntax::ast::TypeQual::Unique, Box::new(named("Int"))),
        ] {
            let other = view_slice(element);
            let definitions = BTreeMap::from([(
                "Packet".into(), definition("Packet", vec![other.clone()], false),
            )]);
            assert!(!is_rooted_int_view_carrier(&other, &definitions));
            assert!(!is_rooted_int_view_aggregate(&packet, &definitions));
        }
        let qualified_payload = Type::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(Type::Qualified(
                witchy_syntax::ast::TypeQual::Frozen,
                Box::new(Type::Slice(Box::new(named("Int")))),
            )),
        );
        assert!(!is_rooted_int_view_carrier(&qualified_payload, &BTreeMap::new()));
    }

    #[test]
    fn unit_payloads_preserve_generic_and_sum_shapes() {
        let payload = generic_definition("Payload", &["a"], vec![named("a"), named("Int")], false);
        let definitions = BTreeMap::from([("Payload".into(), payload)]);
        for unit in [named("Nil"), named("Unit"), Type::Tuple(Vec::new())] {
            let applied = Type::Named("Payload".into(), vec![unit.clone()]);
            assert_eq!(scalar_aggregate_shape(&applied, &definitions).unwrap().fields,
                vec![unit.clone(), named("Int")]);
            let option = Type::Named("Option".into(), vec![unit.clone()]);
            assert_eq!(scalar_aggregate_shape(&option, &definitions).unwrap().fields,
                vec![named("Int"), unit.clone()]);
            assert!(is_v1_list_element_type(&unit, &definitions));
        }
        assert!(!is_v1_field_type(&named("Missing"), &definitions));
        assert!(is_v1_field_type(&named("Console"), &definitions));
    }

    #[test]
    fn recursive_shapes_preserve_fields_without_expanding_cycles() {
        let tree = definition("Tree", vec![named("Int"), named("Tree")], false);
        let definitions = BTreeMap::from([("Tree".into(), tree.clone())]);
        let shape = scalar_aggregate_shape(&named("Tree"), &definitions).unwrap();
        assert_eq!(shape.fields, tree.variants[0].fields);
        assert!(is_v1_field_type(&Type::Named("Option".into(), vec![named("Tree")]), &definitions));
        assert!(is_v1_list_element_type(&named("Tree"), &definitions));
    }

    #[test]
    fn recursive_shapes_check_fields_after_a_cycle_and_refuse_inline_cycles() {
        let mut tree = definition("Tree", vec![named("Tree"), named("Missing")], false);
        let mut definitions = BTreeMap::from([("Tree".into(), tree.clone())]);
        assert!(scalar_aggregate_shape(&named("Tree"), &definitions).is_none());
        tree.variants[0].fields.pop();
        tree.packed = true;
        definitions.insert("Tree".into(), tree.clone());
        assert!(scalar_aggregate_shape(&named("Tree"), &definitions).is_none());
        tree.packed = false;
        tree.is_capability = true;
        definitions.insert("Tree".into(), tree);
        assert!(scalar_aggregate_shape(&named("Tree"), &definitions).is_none());
    }

    #[test]
    fn shape_memo_reuse_requires_all_recursion_assumptions() {
        let definitions = BTreeMap::from([
            ("A".into(), definition("A", vec![named("B"), named("Missing")], false)),
            ("B".into(), definition("B", vec![named("A")], false)),
        ]);
        let mut completed = ShapeMemo::new();
        assert!(field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::from(["A".into()]), 0,
            &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed));
        // One node pays for the lookup, so success requires proof reuse. The
        // additional active owner does not remove the original A assumption.
        assert!(field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::from(["A".into(), "Extra".into()]), 0,
            &mut 1, &mut completed));
        assert!(!field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::new(), 0,
            &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed),
            "the cached B proof cannot hide A's unsupported field without its assumption");
    }

    #[test]
    fn shape_memo_never_waives_depth_or_packed_backedge_rejection() {
        let definitions = BTreeMap::from([
            ("Leaf".into(), definition("Leaf", vec![named("Int")], false)),
            ("Packed".into(), definition("Packed", vec![named("Owner")], true)),
            ("Owner".into(), definition("Owner", vec![named("Packed")], false)),
        ]);
        let mut completed = ShapeMemo::new();
        assert!(field_type_ok_at(&named("Leaf"), &definitions, &mut BTreeSet::new(),
            MAX_AGGREGATE_SHAPE_EXPANSION - 3, &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed));
        assert!(!field_type_ok_at(&named("Leaf"), &definitions, &mut BTreeSet::new(),
            MAX_AGGREGATE_SHAPE_EXPANSION - 2, &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed));
        assert!(field_type_ok_at(&named("Packed"), &definitions,
            &mut BTreeSet::from(["Owner".into()]), 0,
            &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed));
        assert!(!field_type_ok_at(&named("Packed"), &definitions,
            &mut BTreeSet::from(["Owner".into(), "Packed".into()]), 0,
            &mut MAX_AGGREGATE_SHAPE_NODES, &mut completed));
    }

    #[test]
    fn shape_memo_rejects_newly_active_packed_descendants() {
        let definitions = BTreeMap::from([
            ("B".into(), definition("B", vec![named("P")], false)),
            ("P".into(), definition("P", vec![named("B")], true)),
        ]);
        let mut completed = ShapeMemo::new();
        let mut budget = MAX_AGGREGATE_SHAPE_NODES;
        assert!(field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::new(), 0, &mut budget, &mut completed));
        // Reuse with no new assumptions is still a one-node lookup.
        assert!(field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::new(), 0, &mut 1, &mut completed));
        let mut budget = MAX_AGGREGATE_SHAPE_NODES;
        assert!(!field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::from(["P".into()]), 0, &mut budget, &mut ShapeMemo::new()),
            "uncached B -> P must reject the packed backedge");
        let mut budget = MAX_AGGREGATE_SHAPE_NODES;
        assert!(!field_type_ok_at(&named("B"), &definitions,
            &mut BTreeSet::from(["P".into()]), 0, &mut budget, &mut completed),
            "cached B must not hide its newly active packed descendant P");
    }

    #[test]
    fn recursive_shapes_handle_mutual_cycles_and_exact_builtin_arity() {
        let definitions = BTreeMap::from([
            ("A".into(), definition("A", vec![named("B")], false)),
            ("B".into(), definition("B", vec![named("A"), named("String")], false)),
        ]);
        assert!(scalar_aggregate_shape(&named("A"), &definitions).is_some());
        assert!(scalar_aggregate_shape(&named("B"), &definitions).is_some());
        assert!(!is_v1_field_type(&Type::Named("Option".into(), vec![named("A"), named("B")]), &definitions));
        assert!(!is_v1_field_type(&Type::Named("Result".into(), vec![named("A")]), &definitions));
    }

    #[test]
    fn generic_shapes_substitute_concrete_fields_and_key_recursion_by_instantiation() {
        let pair = generic_definition("Pair", &["a", "b"], vec![named("a"), named("b")], false);
        let node = generic_definition(
            "Node",
            &["a"],
            vec![Type::Named("Node".into(), vec![named("a")]), named("a")],
            false,
        );
        let definitions = BTreeMap::from([("Pair".into(), pair), ("Node".into(), node)]);
        let pair = Type::Named("Pair".into(), vec![named("Int"), named("String")]);
        let shape = scalar_aggregate_shape(&pair, &definitions).expect("concrete Pair shape");
        assert_eq!(shape.fields, vec![named("Int"), named("String")]);
        assert!(scalar_aggregate_shape(&Type::Named("Node".into(), vec![named("Int")]), &definitions).is_some());
    }

    #[test]
    fn generic_shapes_refuse_unresolved_wrong_arity_and_expanding_or_inline_cycles() {
        let box_ = generic_definition("Box", &["a"], vec![named("a")], false);
        let grow = generic_definition(
            "Grow",
            &["a"],
            vec![Type::Named("Grow".into(), vec![Type::Tuple(vec![named("a"), named("a")])])],
            false,
        );
        let phantom = generic_definition("Phantom", &["a"], vec![named("Int")], false);
        let packed = generic_definition(
            "PackedNode",
            &["a"],
            vec![Type::Named("PackedNode".into(), vec![named("a")])],
            true,
        );
        let definitions = BTreeMap::from([
            ("Box".into(), box_),
            ("Grow".into(), grow),
            ("Phantom".into(), phantom),
            ("PackedNode".into(), packed),
        ]);
        assert!(scalar_aggregate_shape(&Type::Named("Box".into(), Vec::new()), &definitions).is_none());
        assert!(scalar_aggregate_shape(&Type::Named("Box".into(), vec![named("a")]), &definitions).is_none());
        assert!(scalar_aggregate_shape(&Type::Named("Grow".into(), vec![named("Int")]), &definitions).is_none());
        assert!(scalar_aggregate_shape(&Type::Named("Phantom".into(), vec![named("Missing")]), &definitions).is_none());
        assert!(scalar_aggregate_shape(&Type::Named("PackedNode".into(), vec![named("Int")]), &definitions).is_none());
    }

    #[test]
    fn recursive_fn_shapes_preserve_signatures_and_reject_invalid_lanes() {
        use witchy_syntax::ast::{CallableQualifiers, Convention};

        let applied = |name: &str, argument: Type| Type::Named(name.into(), vec![argument]);
        let thunk = |result: Type| Type::Fn(Vec::new(), Box::new(result), Vec::new(), CallableQualifiers::default());
        let cursor = generic_definition("model.Cursor", &["a"], vec![thunk(applied("model.Step", named("a")))], false);
        let mut step = generic_definition("model.Step", &["a"], vec![named("a"), applied("model.Cursor", named("a"))], false);
        step.variants.push(Variant { name: "Empty".into(), line: 0, fields: Vec::new(), field_names: Vec::new(), field_lines: Vec::new() });
        let holder = generic_definition("model.Holder", &["f"], vec![named("f")], false);
        let mut definitions = BTreeMap::from([
            ("model.Cursor".into(), cursor), ("model.Step".into(), step), ("model.Holder".into(), holder),
        ]);
        let cursor = applied("model.Cursor", named("Int"));
        let signature = thunk(applied("model.Step", named("Int")));
        assert_eq!(scalar_aggregate_shape(&cursor, &definitions).unwrap().fields, vec![signature.clone()]);
        let holder = applied("model.Holder", signature.clone());
        assert_eq!(scalar_aggregate_shape(&holder, &definitions).unwrap().fields, vec![signature.clone()]);
        assert_eq!(concrete_aggregate_identity(&holder, &definitions), Some(holder.clone()));
        assert!(is_v1_list_element_type(&signature, &definitions));
        assert!(is_v1_field_type(&thunk(named("Nil")), &definitions));
        let consuming = Type::Fn(vec![cursor.clone()], Box::new(named("Int")), vec![Convention::Own], CallableQualifiers::new(true, false));
        let borrowing = Type::Fn(vec![cursor], Box::new(named("Int")), vec![Convention::Borrow], CallableQualifiers::default());
        for callable in [&consuming, &borrowing] {
            let generic = applied("model.Holder", callable.clone());
            assert_eq!(scalar_aggregate_shape(&generic, &definitions).unwrap().fields, vec![callable.clone()]);
            assert_eq!(concrete_aggregate_identity(&generic, &definitions), Some(generic));
        }
        assert_ne!(aggregate_key(&applied("model.Holder", consuming)), aggregate_key(&applied("model.Holder", borrowing)));

        for invalid in [
            thunk(named("Missing")),
            thunk(applied("model.Step", named("a"))),
            thunk(Type::Named("model.Step".into(), Vec::new())),
            Type::Fn(vec![named("Missing")], Box::new(named("Int")), Vec::new(), CallableQualifiers::default()),
            Type::Fn(vec![named("Int")], Box::new(named("Int")), vec![Convention::Let, Convention::Own], CallableQualifiers::default()),
        ] {
            assert!(!is_v1_field_type(&invalid, &definitions), "{invalid:?}");
            let generic = applied("model.Holder", invalid);
            assert!(scalar_aggregate_shape(&generic, &definitions).is_none());
            assert!(concrete_aggregate_identity(&generic, &definitions).is_none());
        }
        // A cycle does not waive another signature lane, even with a qualified
        // nominal of the same short name present in the declaration catalog.
        definitions.insert("other.Step".into(), definition("other.Step", vec![named("Missing")], false));
        definitions.get_mut("model.Cursor").unwrap().variants[0].fields = vec![
            Type::Fn(vec![named("other.Step")], Box::new(applied("model.Step", named("a"))), Vec::new(), CallableQualifiers::default()),
        ];
        assert!(scalar_aggregate_shape(&applied("model.Cursor", named("Int")), &definitions).is_none());
    }
}

/// The shape of a source type: a scalar `Option`/`Result` instantiation or
/// a declared scalar aggregate.
pub fn scalar_aggregate_shape(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ScalarAggregateShape> {
    if !shape_memo::worth_memoizing(ty, definitions) {
        return scalar_aggregate_shape_uncached(ty, definitions);
    }
    if let Some(known) = shape_memo::shape(definitions, ty) {
        return known;
    }
    let shape = scalar_aggregate_shape_uncached(ty, definitions);
    shape_memo::remember_shape(definitions, ty, &shape);
    shape
}

fn scalar_aggregate_shape_uncached(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ScalarAggregateShape> {
    // A dictionary is its canonical representation: a tuple of the key list
    // and the value list, in insertion order.
    if let Some(tuple) = dictionary_as_tuple(ty) {
        return scalar_aggregate_shape(&tuple, definitions);
    }
    // A tuple is an anonymous one-variant record of its items.
    if let Type::Tuple(items) = ty.unqualified() {
        if items.is_empty() || !items.iter().all(|item| is_v1_field_type(item, definitions)) {
            return None;
        }
        return Some(ScalarAggregateShape {
            name: render_type(ty)?,
            fields: items.clone(),
            variants: vec![ScalarVariant { name: String::new(), first_field: 0, arity: items.len() }],
            tagged: false,
            packed: false,
        });
    }
    let Type::Named(name, arguments) = ty.unqualified() else { return None };
    if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
        let expected = variants.iter().map(|(_, arity)| *arity).sum::<usize>();
        if expected != arguments.len()
            || !arguments.iter().all(|argument| is_v1_field_type(argument, definitions))
        {
            return None;
        }
        let mut fields = vec![int_type()];
        let mut shape_variants = Vec::with_capacity(variants.len());
        let mut offset = 0usize;
        for (tag, arity) in variants {
            let end = offset.checked_add(arity)?;
            shape_variants.push(ScalarVariant {
                name: tag,
                first_field: fields.len(),
                arity,
            });
            fields.extend_from_slice(&arguments[offset..end]);
            offset = end;
        }
        return Some(ScalarAggregateShape {
            // The synthetic head is the physical aggregate identity.  Two
            // source unions with the same tag spelling but different payload
            // schema must never share a layout.
            name: name.clone(),
            fields,
            variants: shape_variants,
            tagged: true,
            packed: false,
        });
    }
    match (name.as_str(), arguments.as_slice()) {
        ("Option", [payload]) if is_v1_field_type(payload, definitions) => Some(ScalarAggregateShape {
            name: "Option".into(),
            fields: vec![int_type(), payload.clone()],
            variants: vec![
                ScalarVariant { name: "Some".into(), first_field: 1, arity: 1 },
                ScalarVariant { name: "None".into(), first_field: 2, arity: 0 },
            ],
            tagged: true,
            packed: false,
        }),
        ("Result", [value, error]) if is_v1_field_type(value, definitions) && is_v1_field_type(error, definitions) => Some(ScalarAggregateShape {
            name: "Result".into(),
            fields: vec![int_type(), value.clone(), error.clone()],
            variants: vec![
                ScalarVariant { name: "Ok".into(), first_field: 1, arity: 1 },
                ScalarVariant { name: "Err".into(), first_field: 2, arity: 1 },
            ],
            tagged: true,
            packed: false,
        }),
        _ => definitions.get(name).and_then(|definition| shape_from_instantiation(definition, arguments, definitions)),
    }
}

#[cfg(test)]
mod scheduler_shape_tests {
    use super::*;
    use crate::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

    #[test]
    fn canonical_task_tuple_has_a_bounded_concrete_shape() {
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, "test/task-tuple-shape", "0.1.0").unwrap();
        let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
        owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
        }));
        let source = b"import chan\nasync fn next(n: Int) -> Int:\n    n + 1\nasync fn main(console: Console):\n    let values = chan.par_map([4], fn(n): next(n)).await\n    console.print(\"${values}\")\n";
        let checked = crate::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), source.to_vec()),
                ("task".into(), include_bytes!("../../../std/task.witchy").to_vec()),
                ("chan".into(), include_bytes!("../../../std/chan.witchy").to_vec())],
            "main", |_, _, _| Ok(Default::default()),
            &std::collections::HashSet::from(["main".into()]),
            AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap(),
        ).expect("the actual task-tuple source checks");
        let definitions = type_definitions(checked.canonical_source().unwrap().typed_module().module());
        let nil = Type::Named("Nil".into(), vec![]);
        let int = Type::Named("Int".into(), vec![]);
        let tuple = Type::Tuple(vec![
            Type::Named("List".into(), vec![Type::Named("task.Task".into(), vec![nil])]),
            Type::Named("List".into(), vec![Type::Named("chan.Receiver".into(), vec![int])]),
        ]);
        let task = Type::Named("task.Task".into(), vec![tuple.clone()]);
        assert!(is_v1_field_type(&tuple, &definitions), "the concrete tuple argument is supported");
        let mut budget = MAX_AGGREGATE_SHAPE_NODES;
        let valid = field_type_ok_at(&task, &definitions, &mut BTreeSet::new(), 0, &mut budget, &mut ShapeMemo::new());
        assert!(valid, "finite Task<Tuple> refused with {budget} shape nodes remaining");
        assert!(scalar_aggregate_shape(&task, &definitions).is_some());
    }

    #[test]
    fn canonical_scheduler_slot_is_a_finite_owned_graph() {
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, "test/scheduler-shape", "0.1.0").unwrap();
        let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
        owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
        }));
        let checked = crate::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), b"import task\nfn main():\n    task.run(task.done(()))\n".to_vec()),
                ("task".into(), include_bytes!("../../../std/task.witchy").to_vec())],
            "main", |_, _, _| Ok(Default::default()),
            &std::collections::HashSet::from(["main".into()]),
            AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap(),
        ).expect("actual source scheduler checks and messages are prepared");
        let definitions = type_definitions(checked.canonical_source().unwrap().typed_module().module());
        let slot = Type::Named("task.Slot".into(), vec![]);
        let definition = definitions.get("task.Slot").expect("actual scheduler slot declaration");
        let first_bad_field = definition.variants.iter().flat_map(|variant| &variant.fields)
            .find(|field| !is_v1_field_type(field, &definitions));
        let mut budget = MAX_AGGREGATE_SHAPE_NODES;
        let valid = field_type_ok_at(&slot, &definitions, &mut BTreeSet::new(), 0, &mut budget, &mut ShapeMemo::new());
        assert!(valid, "finite scheduler graph refused: remaining budget={budget}, independently unsupported field={first_bad_field:?}");

        let mut invalid = definitions.clone();
        invalid.get_mut("task.Slot").unwrap().variants[0].fields.push(Type::Named("Missing".into(), vec![]));
        assert!(!is_v1_field_type(&slot, &invalid), "an unknown field cannot acquire storage authority");
    }
}
