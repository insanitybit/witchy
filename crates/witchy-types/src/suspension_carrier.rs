//! Typed, backend-neutral ABI description for compiler-owned suspension state.
//!
//! Syntax lowering assigns stable state identities, but it runs before type
//! inference and therefore cannot decide whether a frame can use the scalar
//! Wasm carrier. This catalog is deliberately built from [`TypedModule`]: it
//! joins each generated callable with its finalized parameter signature and
//! flattens transparent, single-variant scalar wrappers into fixed-width lanes.
//! Async and generator lowering can target the same catalog without making a
//! generated function name or a closure layout part of the runtime contract.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use witchy_syntax::ast::{self, Convention, Function, Item, Type, TypeDef};
use witchy_syntax::suspension::{
    frame_state, FRAME_ENTRY_ATTRIBUTE, FRAME_FUNCTION_ATTRIBUTE,
};

use crate::typeck::TypedModule;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierLane {
    I64,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierStateKind {
    Entry,
    Segment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CarrierSlot {
    pub name: String,
    pub convention: Convention,
    pub ty: Type,
    /// `None` means the value needs the boxed fallback. `Some([])` is a
    /// zero-width `Nil`; all other `Some` values are fixed scalar columns.
    pub lanes: Option<Vec<CarrierLane>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CarrierState {
    pub id: usize,
    pub function: String,
    pub kind: CarrierStateKind,
    pub slots: Vec<CarrierSlot>,
}

impl CarrierState {
    pub fn is_flat_scalar(&self) -> bool {
        self.slots.iter().all(|slot| slot.lanes.is_some())
    }

    pub fn lane_width(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| slot.lanes.as_ref())
            .map(Vec::len)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuspensionCarrierCatalog {
    states: Vec<CarrierState>,
    max_lane_width: usize,
}

impl SuspensionCarrierCatalog {
    pub fn from_typed(typed: &TypedModule) -> Result<Self, String> {
        let definitions = type_definitions(typed.module());
        let mut states = Vec::new();
        let mut ids = HashSet::new();

        for item in &typed.module().items {
            let Item::Function(function) = item else { continue };
            let Some(id) = frame_state(function) else { continue };
            let kind = state_kind(function).ok_or_else(|| {
                format!(
                    "compiler suspension state {id} on `{}` has neither entry nor segment marker",
                    function.name
                )
            })?;
            if !ids.insert(id) {
                return Err(format!("duplicate compiler suspension state id {id}"));
            }

            let inferred = inferred_parameters(typed, function);
            let slots = function
                .params
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    let ty = inferred
                        .as_ref()
                        .and_then(|types| types.get(index))
                        .cloned()
                        .or_else(|| parameter.ty.clone())
                        .unwrap_or_else(|| Type::Named("__Unknown".into(), Vec::new()));
                    let lanes = scalar_lanes(&ty, &definitions, &HashMap::new(), &mut HashSet::new());
                    CarrierSlot {
                        name: parameter.name.clone(),
                        convention: parameter.convention,
                        ty,
                        lanes,
                    }
                })
                .collect();
            states.push(CarrierState { id, function: function.name.clone(), kind, slots });
        }

        states.sort_by_key(|state| state.id);
        let max_lane_width = states.iter().map(CarrierState::lane_width).max().unwrap_or(0);
        Ok(Self { states, max_lane_width })
    }

    pub fn states(&self) -> &[CarrierState] {
        &self.states
    }

    pub fn max_lane_width(&self) -> usize {
        self.max_lane_width
    }

    pub fn is_wholly_flat_scalar(&self) -> bool {
        !self.states.is_empty() && self.states.iter().all(CarrierState::is_flat_scalar)
    }
}

fn state_kind(function: &Function) -> Option<CarrierStateKind> {
    if function.attributes.iter().any(|attribute| attribute == FRAME_ENTRY_ATTRIBUTE) {
        Some(CarrierStateKind::Entry)
    } else if function.attributes.iter().any(|attribute| attribute == FRAME_FUNCTION_ATTRIBUTE) {
        Some(CarrierStateKind::Segment)
    } else {
        None
    }
}

fn inferred_parameters(typed: &TypedModule, function: &Function) -> Option<Vec<Type>> {
    let Type::Fn(parameters, _, _) = typed.table().function_type(&function.name)? else {
        return None;
    };
    Some(parameters)
}

fn type_definitions(module: &ast::Module) -> HashMap<String, &TypeDef> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) => Some((definition.name.clone(), definition)),
            _ => None,
        })
        .collect()
}

fn scalar_lanes(
    ty: &Type,
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    match ty {
        Type::Qualified(_, inner) => scalar_lanes(inner, definitions, substitutions, visiting),
        Type::Tuple(items) => flatten_all(items, definitions, substitutions, visiting),
        Type::Named(name, arguments) if arguments.is_empty() => {
            if let Some(actual) = substitutions.get(name) {
                return scalar_lanes(actual, definitions, substitutions, visiting);
            }
            match name.as_str() {
                "Int" | "Bool" | "Duration" => Some(vec![CarrierLane::I64]),
                "Float" => Some(vec![CarrierLane::F64]),
                "Nil" => Some(Vec::new()),
                _ => flatten_nominal(name, arguments, definitions, substitutions, visiting),
            }
        }
        Type::Named(name, arguments) => {
            flatten_nominal(name, arguments, definitions, substitutions, visiting)
        }
        Type::Fn(_, _, _) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
    }
}

fn flatten_all(
    fields: &[Type],
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    let mut lanes = Vec::new();
    for field in fields {
        lanes.extend(scalar_lanes(field, definitions, substitutions, visiting)?);
    }
    Some(lanes)
}

fn flatten_nominal(
    name: &str,
    arguments: &[Type],
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    let definition = definitions.get(name).copied().or_else(|| {
        let short = name.rsplit('.').next()?;
        definitions.get(short).copied()
    })?;
    // A sum needs a runtime tag and variant-dependent payload rules. The first
    // scalar executor slice intentionally admits only transparent products.
    if definition.variants.len() != 1 || !visiting.insert(definition.name.clone()) {
        return None;
    }
    let parameters = ast::effective_type_def_params(definition);
    if parameters.len() != arguments.len() {
        visiting.remove(&definition.name);
        return None;
    }
    let mut nested = substitutions.clone();
    nested.extend(parameters.into_iter().zip(arguments.iter().cloned()));
    let result = flatten_all(
        &definition.variants[0].fields,
        definitions,
        &nested,
        visiting,
    );
    visiting.remove(&definition.name);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_catalog_flattens_owned_scalar_wrappers_and_pins_state_order() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Id:\n    Id(Int)\n\nfn entry(id: Id, n: Int) -> Nil:\n    ()\n\nfn resume(own id: Id, own n: Int) -> Nil:\n    ()\n",
        )
        .expect("carrier fixture parses");
        let Item::Function(entry) = &mut module.items[1] else { panic!("entry") };
        entry.attributes.push(FRAME_ENTRY_ATTRIBUTE.into());
        entry
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(4));
        let Item::Function(segment) = &mut module.items[2] else { panic!("segment") };
        segment.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        segment
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(2));

        let typed = crate::typeck::annotate_checked(module).expect("fixture type checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");

        assert_eq!(catalog.states().iter().map(|state| state.id).collect::<Vec<_>>(), [2, 4]);
        assert!(catalog.is_wholly_flat_scalar());
        assert_eq!(catalog.max_lane_width(), 2);
        assert_eq!(catalog.states()[0].kind, CarrierStateKind::Segment);
        assert_eq!(catalog.states()[0].slots[0].convention, Convention::Own);
        assert_eq!(catalog.states()[0].slots[0].lanes, Some(vec![CarrierLane::I64]));
    }
}
