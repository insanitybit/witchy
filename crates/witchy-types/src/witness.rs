//! RFC-0081 deterministic existential witness metadata.
//!
//! This is the shared contract between interpreter values and compiled payload
//! boxes. It assigns closed-program witness IDs and typed method slots without
//! choosing either backend's runtime representation.

use foldhash::{HashMap, HashSet, HashSetExt as _};
use witchy_syntax::ast::{Convention, ImplDef, Item, MethodSig, Module, TraitDef, Type};

use crate::traits::{expected_method_type, impl_self_type, mangle, ret_type};

#[derive(Clone, Debug, PartialEq)]
pub struct WitnessSlot {
    pub owner_trait: String,
    pub method: String,
    pub adapter: String,
    pub receiver: Convention,
    pub params: Vec<Type>,
    pub result: Type,
    pub conventions: Vec<Convention>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Witness {
    pub id: u32,
    pub existential: Type,
    pub concrete: Type,
    pub slots: Vec<WitnessSlot>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WitnessPlan {
    pub witnesses: Vec<Witness>,
}

impl WitnessPlan {
    pub fn get(&self, existential: &Type, concrete: &Type) -> Option<&Witness> {
        self.witnesses
            .iter()
            .find(|w| &w.existential == existential && &w.concrete == concrete)
    }
}

pub fn build(module: &Module) -> Result<WitnessPlan, String> {
    let traits: HashMap<&str, &TraitDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Trait(definition) => Some((definition.name.as_str(), definition)),
            _ => None,
        })
        .collect();
    let impls: Vec<&ImplDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(definition) if definition.trait_name.is_some() => Some(definition),
            _ => None,
        })
        .collect();

    let mut entries = impls
        .iter()
        .map(|implementation| {
            let trait_name = implementation
                .trait_name
                .as_deref()
                .expect("filtered trait impl");
            let existential =
                Type::Dyn(trait_name.to_string(), implementation.trait_args.clone());
            let concrete = impl_self_type(implementation);
            (type_key(&existential), type_key(&concrete), *implementation)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));

    let mut witnesses = Vec::with_capacity(entries.len());
    for (_, _, implementation) in entries {
        let trait_name = implementation
            .trait_name
            .as_deref()
            .expect("filtered trait impl");
        let mut order = Vec::new();
        linearize_trait(trait_name, &traits, &mut HashSet::new(), &mut order)?;
        let mut slots = Vec::new();
        for owner in order {
            let owner_trait = traits
                .get(owner.as_str())
                .ok_or_else(|| format!("existential witness references unknown trait `{owner}`"))?;
            let owner_impl = find_impl(&impls, &owner, implementation).ok_or_else(|| {
                format!(
                    "`{}` implements `{trait_name}` but has no witness impl for supertrait `{owner}`",
                    type_key(&impl_self_type(implementation))
                )
            })?;
            let vars: HashMap<String, Type> = owner_trait
                .typarams
                .iter()
                .cloned()
                .zip(owner_impl.trait_args.iter().cloned())
                .collect();
            for method in &owner_trait.methods {
                slots.push(slot(owner_trait, owner_impl, method, &vars)?);
            }
        }
        witnesses.push(Witness {
            id: u32::try_from(witnesses.len())
                .map_err(|_| "existential witness table exceeds u32 IDs".to_string())?,
            existential: Type::Dyn(trait_name.to_string(), implementation.trait_args.clone()),
            concrete: impl_self_type(implementation),
            slots,
        });
    }
    Ok(WitnessPlan { witnesses })
}

fn linearize_trait(
    name: &str,
    traits: &HashMap<&str, &TraitDef>,
    visiting: &mut HashSet<String>,
    order: &mut Vec<String>,
) -> Result<(), String> {
    if order.iter().any(|existing| existing == name) {
        return Ok(());
    }
    if !visiting.insert(name.to_string()) {
        return Err(format!("cyclic existential supertrait graph at `{name}`"));
    }
    let definition = traits
        .get(name)
        .ok_or_else(|| format!("existential witness references unknown trait `{name}`"))?;
    for supertrait in &definition.supertraits {
        linearize_trait(supertrait, traits, visiting, order)?;
    }
    visiting.remove(name);
    order.push(name.to_string());
    Ok(())
}

fn find_impl<'a>(
    impls: &'a [&ImplDef],
    trait_name: &str,
    concrete: &'a ImplDef,
) -> Option<&'a ImplDef> {
    if concrete.trait_name.as_deref() == Some(trait_name) {
        return Some(concrete);
    }
    impls.iter().copied().find(|candidate| {
        candidate.trait_name.as_deref() == Some(trait_name)
            && candidate.type_name == concrete.type_name
            && candidate.target_args == concrete.target_args
    })
}

fn slot(
    owner: &TraitDef,
    implementation: &ImplDef,
    method: &MethodSig,
    vars: &HashMap<String, Type>,
) -> Result<WitnessSlot, String> {
    if method.params.first().is_none_or(|param| param.name != "self") {
        return Err(format!(
            "trait method `{}.{}` has no receiver and cannot occupy an existential witness slot",
            owner.name, method.name
        ));
    }
    Ok(WitnessSlot {
        owner_trait: owner.name.clone(),
        method: method.name.clone(),
        adapter: mangle(
            Some(&owner.name),
            &implementation.trait_args,
            &implementation.type_name,
            &method.name,
        ),
        receiver: method.params[0].convention,
        params: method
            .params
            .iter()
            .skip(1)
            .map(|param| {
                expected_method_type(
                    param.ty.as_ref().unwrap_or(&Type::Named("Self".into(), Vec::new())),
                    implementation,
                    vars,
                )
            })
            .collect(),
        result: ret_type(&method.ret, implementation, vars),
        conventions: method
            .params
            .iter()
            .skip(1)
            .map(|param| param.convention)
            .collect(),
    })
}

fn type_key(ty: &Type) -> String {
    match ty {
        Type::Named(name, args) | Type::Dyn(name, args) => format!(
            "{}({})",
            name,
            args.iter().map(type_key).collect::<Vec<_>>().join(",")
        ),
        Type::Tuple(items) => {
            format!("({})", items.iter().map(type_key).collect::<Vec<_>>().join(","))
        }
        Type::Fn(params, result, conventions) => format!(
            "fn[{:?}]({})->{}",
            conventions,
            params.iter().map(type_key).collect::<Vec<_>>().join(","),
            type_key(result)
        ),
        Type::Qualified(qualifier, inner) => format!("{qualifier:?}:{}", type_key(inner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser;

    #[test]
    fn slots_linearize_supertraits_and_preserve_typed_conventions() {
        let module = parser::parse_module(
            "trait Base:\n\
             \x20   fn id(self) -> Int\n\n\
             trait Render: Base:\n\
             \x20   fn render(let self, prefix: String) -> String\n\
             \x20   fn replace(var self, value: String) -> String\n\n\
             type Label:\n\
             \x20   Label(String)\n\n\
             impl Base for Label:\n\
             \x20   fn id(self) -> Int:\n\
             \x20       1\n\n\
             impl Render for Label:\n\
             \x20   fn render(let self, prefix: String) -> String:\n\
             \x20       prefix\n\
             \x20   fn replace(var self, value: String) -> String:\n\
             \x20       value\n",
        )
        .expect("parse");
        let plan = build(&module).expect("witness plan");
        let witness = plan
            .get(
                &Type::Dyn("Render".into(), Vec::new()),
                &Type::Named("Label".into(), Vec::new()),
            )
            .expect("Render for Label witness");

        assert_eq!(
            witness
                .slots
                .iter()
                .map(|slot| (slot.owner_trait.as_str(), slot.method.as_str()))
                .collect::<Vec<_>>(),
            [("Base", "id"), ("Render", "render"), ("Render", "replace")]
        );
        assert_eq!(
            witness.slots[1].params,
            [Type::Named("String".into(), Vec::new())]
        );
        assert_eq!(witness.slots[2].receiver, Convention::Var);
        assert_eq!(
            witness.slots[2].conventions,
            [Convention::Let]
        );
    }

    #[test]
    fn witness_ids_do_not_depend_on_impl_source_order() {
        let source = |impls: &str| {
            parser::parse_module(&format!(
                "trait Show:\n    fn show(self) -> String\n\n\
                 type A:\n    A(Int)\n\n\
                 type B:\n    B(Int)\n\n{impls}"
            ))
            .expect("parse")
        };
        let a = "impl Show for A:\n    fn show(self) -> String:\n        \"a\"\n\n";
        let b = "impl Show for B:\n    fn show(self) -> String:\n        \"b\"\n\n";
        let left = build(&source(&format!("{a}{b}"))).expect("left");
        let right = build(&source(&format!("{b}{a}"))).expect("right");
        assert_eq!(left, right);
    }

    #[test]
    fn parameterized_trait_witnesses_select_the_exact_impl_signature() {
        let module = parser::parse_module(
            "trait Convert(a):\n\
             \x20   fn convert(self, value: a) -> a\n\n\
             type Box:\n\
             \x20   Box(Int)\n\n\
             impl Convert(Int) for Box:\n\
             \x20   fn convert(self, value: Int) -> Int:\n\
             \x20       value\n\n\
             impl Convert(String) for Box:\n\
             \x20   fn convert(self, value: String) -> String:\n\
             \x20       value\n",
        )
        .expect("parse");
        let plan = build(&module).expect("witness plan");
        let int = plan
            .get(
                &Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Type::Named("Box".into(), Vec::new()),
            )
            .expect("Int witness");
        let string = plan
            .get(
                &Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("String".into(), Vec::new())],
                ),
                &Type::Named("Box".into(), Vec::new()),
            )
            .expect("String witness");

        assert_eq!(
            int.slots[0].params,
            [Type::Named("Int".into(), Vec::new())]
        );
        assert_eq!(
            string.slots[0].params,
            [Type::Named("String".into(), Vec::new())]
        );
        assert_ne!(int.slots[0].adapter, string.slots[0].adapter);
    }
}
