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

use std::collections::BTreeMap;

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
    field_type_ok(ty, definitions, true)
}

/// A field an aggregate may carry: a scalar in its slot, or a string,
/// list, tuple, or aggregate behind an owned pointer slot.
pub fn is_v1_field_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    field_type_ok(ty, definitions, true)
}

/// `allow_nominal` admits one level of declared aggregates: a nominal
/// field may not itself carry nominal fields, so no shape recurses.
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

fn field_type_ok(ty: &Type, definitions: &BTreeMap<String, TypeDef>, allow_nominal: bool) -> bool {
    if let Some(tuple) = dictionary_as_tuple(ty) {
        return field_type_ok(&tuple, definitions, allow_nominal);
    }
    match ty.unqualified() {
        _ if is_scalar_type(ty) => true,
        Type::Named(name, arguments) if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty() => true,
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            field_type_ok(&arguments[0], definitions, allow_nominal)
        }
        Type::Named(name, arguments) if matches!(name.as_str(), "Option" | "Result") => {
            !arguments.is_empty() && arguments.iter().all(|argument| field_type_ok(argument, definitions, false))
        }
        Type::Tuple(items) => !items.is_empty() && items.iter().all(|item| field_type_ok(item, definitions, allow_nominal)),
        Type::Named(name, arguments) if arguments.is_empty() && allow_nominal => {
            definitions.get(name).is_some_and(|definition| definition_fields_ok(definition, definitions, false))
        }
        _ => false,
    }
}

fn definition_fields_ok(definition: &TypeDef, definitions: &BTreeMap<String, TypeDef>, allow_nominal: bool) -> bool {
    definition.params.is_empty()
        && !definition.is_capability
        && !definition.variants.is_empty()
        && definition
            .variants
            .iter()
            .all(|variant| variant.fields.iter().all(|field| field_type_ok(field, definitions, allow_nominal)))
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
    if !definition_fields_ok(definition, definitions, true) {
        return None;
    }
    let name = definition.name.rsplit('.').next().unwrap_or(&definition.name).to_owned();
    if definition.variants.len() == 1 {
        let variant = &definition.variants[0];
        return Some(ScalarAggregateShape {
            name,
            fields: variant.fields.clone(),
            variants: vec![ScalarVariant { name: variant.name.clone(), first_field: 0, arity: variant.fields.len() }],
            tagged: false,
            packed: definition.packed,
        });
    }
    let mut fields = vec![int_type()];
    let mut variants = Vec::with_capacity(definition.variants.len());
    for variant in &definition.variants {
        variants.push(ScalarVariant { name: variant.name.clone(), first_field: fields.len(), arity: variant.fields.len() });
        fields.extend(variant.fields.iter().cloned());
    }
    Some(ScalarAggregateShape { name, fields, variants, tagged: true, packed: false })
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
        (_, []) => definitions.get(name).and_then(|definition| shape_from_definition(definition, definitions)),
        _ => None,
    }
}
