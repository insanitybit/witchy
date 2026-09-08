//! Scalar aggregate shapes shared by the canonical layout catalog, the
//! canonical lowering, and the ABI authority.
//!
//! A scalar aggregate is a record or sum type whose payloads are all scalars
//! (`Int`, `Float`, `Duration`, `Bool`). It lives behind one reference to a
//! fixed layout with one slot per field. A sum type (`type Shape: Circle(Int)
//! | Square(Int)`, `Option(Int)`, `Result(Int, Bool)`) carries an `Int` tag
//! in slot 0 followed by every variant's fields in declaration order, so each
//! payload has one static slot and a construction initializes every slot.
//! Three consumers must agree on this shape byte for byte: the layout
//! catalog derives the field layout from it, the lowering stores and loads
//! its slots, and the ABI authority binds a nominal source type to a
//! candidate reference whose layout carries exactly these fields.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Item, Module, Type, TypeDef, Variant};

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
    matches!(ty.unqualified(), Type::Named(name, arguments)
        if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool"))
}

/// A v1 list element: a scalar (a `Bool` element is one byte), a string,
/// a list of those, or an aggregate.
pub fn is_v1_list_element_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    field_type_ok(ty, definitions, &mut BTreeSet::new())
}

/// A field an aggregate may carry: a scalar in its slot, or a string,
/// list, tuple, or aggregate behind an owned pointer slot.
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

fn field_type_ok(ty: &Type, definitions: &BTreeMap<String, TypeDef>, active: &mut BTreeSet<String>) -> bool {
    let mut budget = MAX_AGGREGATE_SHAPE_NODES;
    field_type_ok_at(ty, definitions, active, 0, &mut budget)
}

fn field_type_ok_at(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
) -> bool {
    if depth == MAX_AGGREGATE_SHAPE_EXPANSION || !consume_shape_nodes(budget, 1) {
        return false;
    }
    if let Some(tuple) = dictionary_as_tuple(ty) {
        return field_type_ok_at(&tuple, definitions, active, depth + 1, budget);
    }
    match ty.unqualified() {
        _ if is_scalar_type(ty) => true,
        Type::Named(name, arguments) if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty() => true,
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            field_type_ok_at(&arguments[0], definitions, active, depth + 1, budget)
        }
        Type::Named(name, arguments) if matches!(name.as_str(), "Option" | "Result") => {
            arguments.len() == if name == "Option" { 1 } else { 2 }
                && arguments.iter().all(|argument| field_type_ok_at(argument, definitions, active, depth + 1, budget))
        }
        Type::Tuple(items) => !items.is_empty() && items.iter().all(|item| field_type_ok_at(item, definitions, active, depth + 1, budget)),
        Type::Named(name, arguments) => {
            definitions.get(name).is_some_and(|definition| {
                definition_fields_ok(definition, arguments, definitions, active, depth + 1, budget)
            })
        }
        _ => false,
    }
}

fn definition_fields_ok(
    definition: &TypeDef,
    arguments: &[Type],
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
) -> bool {
    if definition.params.len() != arguments.len()
        || definition.is_capability
        || definition.variants.is_empty()
        || depth == MAX_AGGREGATE_SHAPE_EXPANSION
    {
        return false;
    }
    // Arguments are part of the concrete instantiation even when a phantom
    // parameter has no field occurrence. Do not let an unused `a` smuggle an
    // unresolved or unsupported type through shape recognition.
    if !arguments.iter().all(|argument| field_type_ok_at(argument, definitions, active, depth + 1, budget)) {
        return false;
    }
    let instantiated = Type::Named(definition.name.clone(), arguments.to_vec());
    let Some(key) = aggregate_key(&instantiated) else { return false };
    // This recognizes a finite source shape graph, not allocation authority.
    // Recursive occurrences need an owned nominal binding in the layout layer;
    // an inline packed occurrence cannot close the graph this way.
    if !active.insert(key) {
        return !definition.packed;
    }
    let substitutions = definition.params.iter().cloned().zip(arguments.iter().cloned()).collect();
    let valid = definition.variants.iter().all(|variant| {
        variant.fields.iter().all(|field| {
            let Some(nodes) = substituted_node_count(field, &substitutions, *budget) else { return false };
            if !consume_shape_nodes(budget, nodes) {
                return false;
            }
            substitute_type(field, &substitutions)
                .is_some_and(|field| field_type_ok_at(&field, definitions, active, depth + 1, budget))
        })
    });
    active.remove(&aggregate_key(&instantiated).expect("named aggregate has a key"));
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
fn substituted_node_count(ty: &Type, substitutions: &BTreeMap<String, Type>, limit: usize) -> Option<usize> {
    let count = match ty {
        Type::Named(name, arguments) if arguments.is_empty() && substitutions.contains_key(name) => {
            type_node_count(&substitutions[name], limit)?
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
fn substitute_type(ty: &Type, substitutions: &BTreeMap<String, Type>) -> Option<Type> {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() && substitutions.contains_key(name) => {
            Some(substitutions[name].clone())
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
    match ty.unqualified() {
        Type::Tuple(items) => {
            let mut rendered = String::from("(");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    rendered.push_str(", ");
                }
                rendered.push_str(&render_type(item)?);
            }
            rendered.push(')');
            Some(rendered)
        }
        Type::Named(name, arguments) if arguments.is_empty() => Some(name.clone()),
        Type::Named(name, arguments) => {
            let mut rendered = format!("{name}(");
            for (index, argument) in arguments.iter().enumerate() {
                if index > 0 {
                    rendered.push_str(", ");
                }
                rendered.push_str(&render_type(argument)?);
            }
            rendered.push(')');
            Some(rendered)
        }
        _ => None,
    }
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
    if !definition_fields_ok(definition, arguments, definitions, &mut BTreeSet::new(), 0, &mut budget) {
        return None;
    }
    let substitutions = definition.params.iter().cloned().zip(arguments.iter().cloned()).collect();
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
}

/// The shape of a source type: a scalar `Option`/`Result` instantiation or
/// a declared scalar aggregate.
pub fn scalar_aggregate_shape(
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
