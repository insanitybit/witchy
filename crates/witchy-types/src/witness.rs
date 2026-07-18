//! RFC-0081 deterministic existential witness metadata.
//!
//! This is the shared contract between interpreter values and compiled payload
//! boxes. It assigns closed-program witness IDs and typed method slots without
//! choosing either backend's runtime representation.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use witchy_syntax::ast::{Convention, ImplDef, Item, MethodSig, Module, TraitDef, Type};

use crate::traits::{
    bind_ast_type_vars, expected_method_type, impl_self_type, monomorphic_impl_method_name,
    ret_type, subst_trait_params,
};

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

/// One authenticated runtime conversion from a source existential witness to
/// the witness for one of its transitive supertraits. Both IDs describe the
/// same opaque concrete payload.
#[derive(Clone, Debug, PartialEq)]
pub struct WitnessUpcast {
    pub source: u32,
    pub target: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WitnessPlan {
    pub witnesses: Vec<Witness>,
    pub upcasts: Vec<WitnessUpcast>,
}

/// Stable dense table addressing for a closed witness plan.
///
/// A runtime receives only a witness ID and the compiler-owned static slot. The
/// table index therefore cannot depend on source names or concrete payload
/// types. Every valid pair maps to `witness_id * stride + slot`; unused cells
/// belong to witnesses for smaller existential layouts and are never selected
/// by well-typed code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitnessDispatchIndex {
    stride: u32,
}

/// Trait and impl declarations preserved across trait lowering.
///
/// Runtime preparation runs after monomorphization, when the executable module
/// has concrete expression types but no longer contains these declarations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WitnessCatalog {
    traits: Vec<TraitDef>,
    impls: Vec<ImplDef>,
}

impl WitnessCatalog {
    pub fn from_module(module: &Module) -> Self {
        let mut catalog = Self::default();
        let trait_methods = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Trait(definition) => {
                    Some((definition.name.clone(), definition.methods.clone()))
                }
                _ => None,
            })
            .collect();
        for item in &module.items {
            match item {
                Item::Trait(definition) => catalog.traits.push(definition.clone()),
                Item::Impl(definition) if definition.trait_name.is_some() => {
                    catalog.impls.push(definition.clone());
                }
                _ => {}
            }
        }
        catalog
            .impls
            .extend(crate::traits::synthesize_anon_union_impls(
                &module.items,
                &trait_methods,
            ));
        catalog
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExistentialSlot {
    pub owner_trait: String,
    pub method: String,
    pub params: Vec<Type>,
    pub result: Type,
    /// Receiver first, followed by the explicit method arguments.
    pub conventions: Vec<Convention>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExistentialLayout {
    pub existential: Type,
    pub slots: Vec<ExistentialSlot>,
}

impl ExistentialLayout {
    pub fn slot(&self, owner_trait: &str, method: &str) -> Option<(u32, &ExistentialSlot)> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.owner_trait == owner_trait && slot.method == method)
            .and_then(|(index, slot)| u32::try_from(index).ok().map(|index| (index, slot)))
    }
}

impl WitnessPlan {
    pub fn by_id(&self, id: u32) -> Option<&Witness> {
        self.witnesses
            .get(usize::try_from(id).ok()?)
            .filter(|witness| witness.id == id)
    }

    pub fn get(&self, existential: &Type, concrete: &Type) -> Option<&Witness> {
        self.witnesses
            .iter()
            .find(|w| &w.existential == existential && &w.concrete == concrete)
    }

    pub fn dispatch_index(&self) -> Result<WitnessDispatchIndex, String> {
        let stride = self
            .witnesses
            .iter()
            .map(|witness| witness.slots.len())
            .max()
            .unwrap_or(0);
        let stride = u32::try_from(stride)
            .map_err(|_| "existential witness layout exceeds u32 slots".to_string())?;
        for (position, witness) in self.witnesses.iter().enumerate() {
            if witness.id != u32::try_from(position).unwrap_or(u32::MAX) {
                return Err(
                    "existential witness IDs must be dense and ordered for runtime dispatch"
                        .to_string(),
                );
            }
        }
        Ok(WitnessDispatchIndex { stride })
    }

    pub fn upcast(&self, source: u32, target: &Type) -> Option<u32> {
        self.upcasts.iter().find_map(|upcast| {
            (upcast.source == source)
                .then(|| self.by_id(upcast.target))
                .flatten()
                .filter(|witness| &witness.existential == target)
                .map(|witness| witness.id)
        })
    }
}

impl WitnessDispatchIndex {
    pub fn stride(self) -> u32 {
        self.stride
    }

    pub fn table_index(self, witness: &Witness, slot: u32) -> Option<u32> {
        if slot >= u32::try_from(witness.slots.len()).ok()? || self.stride == 0 {
            return None;
        }
        witness.id.checked_mul(self.stride)?.checked_add(slot)
    }

    pub fn table_len(self, witness_count: usize) -> Option<u32> {
        u32::try_from(witness_count).ok()?.checked_mul(self.stride)
    }
}

impl Witness {
    pub fn slot(&self, owner_trait: &str, method: &str) -> Option<(u32, &WitnessSlot)> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.owner_trait == owner_trait && slot.method == method)
            .and_then(|(index, slot)| u32::try_from(index).ok().map(|index| (index, slot)))
    }
}

/// Resolve the method-slot surface from the static existential type alone.
///
/// Every concrete witness consumes this layout, so method lowering can select a
/// slot before runtime witness identity is known and cannot drift from the
/// adapter table built for a concrete payload.
pub fn layout(module: &Module, existential: &Type) -> Result<ExistentialLayout, String> {
    layout_from_catalog(&WitnessCatalog::from_module(module), existential)
}

pub fn layout_from_catalog(
    catalog: &WitnessCatalog,
    existential: &Type,
) -> Result<ExistentialLayout, String> {
    let Type::Dyn(trait_name, trait_args) = existential else {
        return Err(format!(
            "existential layout must name `dyn Trait`, got `{}`",
            type_key(existential)
        ));
    };
    if trait_args.iter().any(has_free_type_variable) {
        return Err(format!(
            "existential layout must be fully substituted, got `{}`",
            type_key(existential)
        ));
    }
    let traits: HashMap<&str, &TraitDef> = catalog
        .traits
        .iter()
        .map(|definition| (definition.name.as_str(), definition))
        .collect();
    let root = traits
        .get(trait_name.as_str())
        .ok_or_else(|| format!("existential layout references unknown trait `{trait_name}`"))?;
    if root.typarams.len() != trait_args.len() {
        return Err(format!(
            "trait `{trait_name}` expects {} type argument(s), got {}",
            root.typarams.len(),
            trait_args.len()
        ));
    }

    let mut order = Vec::new();
    linearize_trait(trait_name, &traits, &mut HashSet::new(), &mut order)?;
    let mut slots = Vec::new();
    for owner in order {
        let definition = traits
            .get(owner.as_str())
            .ok_or_else(|| format!("existential layout references unknown trait `{owner}`"))?;
        let owner_args = if owner == *trait_name {
            trait_args.as_slice()
        } else {
            &[]
        };
        if definition.typarams.len() != owner_args.len() {
            return Err(format!(
                "supertrait `{owner}` requires type arguments that Witchy's supertrait syntax cannot supply"
            ));
        }
        let vars: HashMap<String, Type> = definition
            .typarams
            .iter()
            .cloned()
            .zip(owner_args.iter().cloned())
            .collect();
        for method in &definition.methods {
            if method.params.first().is_none_or(|param| param.name != "self") {
                return Err(format!(
                    "trait method `{}.{}` has no receiver and cannot occupy an existential slot",
                    definition.name, method.name
                ));
            }
            let params = method
                .params
                .iter()
                .skip(1)
                .map(|param| {
                    param
                        .ty
                        .as_ref()
                        .map(|ty| subst_trait_params(ty, &vars))
                        .ok_or_else(|| {
                            format!(
                                "trait method `{}.{}` parameter `{}` has no type; existential dispatch signatures must be fully typed",
                                definition.name, method.name, param.name
                            )
                        })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let result = method
                .ret
                .as_ref()
                .map(|ty| subst_trait_params(ty, &vars))
                .unwrap_or_else(|| Type::Named("Nil".into(), Vec::new()));
            slots.push(ExistentialSlot {
                owner_trait: definition.name.clone(),
                method: method.name.clone(),
                params,
                result,
                conventions: method.params.iter().map(|param| param.convention).collect(),
            });
        }
    }
    Ok(ExistentialLayout {
        existential: existential.clone(),
        slots,
    })
}

/// Build the closed-program witness plan for the concrete-to-existential
/// conversions selected by type checking. Impl declarations are templates:
/// runtime identities are assigned only to these closed construction requests.
pub fn build(
    module: &Module,
    requests: impl IntoIterator<Item = (Type, Type)>,
) -> Result<WitnessPlan, String> {
    build_from_catalog(&WitnessCatalog::from_module(module), requests)
}

pub fn build_from_catalog(
    catalog: &WitnessCatalog,
    requests: impl IntoIterator<Item = (Type, Type)>,
) -> Result<WitnessPlan, String> {
    build_from_catalog_with_upcasts(catalog, requests, std::iter::empty())
}

/// Build a deterministic plan plus the directed supertrait conversions needed
/// by compiler-owned `ExistentialUpcast` nodes.
pub fn build_from_catalog_with_upcasts(
    catalog: &WitnessCatalog,
    requests: impl IntoIterator<Item = (Type, Type)>,
    upcast_requests: impl IntoIterator<Item = (Type, Type)>,
) -> Result<WitnessPlan, String> {
    let traits: HashMap<&str, &TraitDef> = catalog
        .traits
        .iter()
        .map(|definition| (definition.name.as_str(), definition))
        .collect();
    let impls: Vec<&ImplDef> = catalog
        .impls
        .iter()
        .collect();

    let mut entries = requests
        .into_iter()
        .map(|(existential, concrete)| {
            (
                type_key(&existential),
                type_key(&concrete),
                existential,
                concrete,
            )
        })
        .collect::<Vec<_>>();
    let upcast_requests = upcast_requests.into_iter().collect::<Vec<_>>();
    for (target, source) in &upcast_requests {
        let (Type::Dyn(target_name, target_args), Type::Dyn(source_name, source_args)) =
            (target, source)
        else {
            return Err("existential upcast requests must convert `dyn Trait` values".to_string());
        };
        if !target_args.is_empty()
            || !source_args.is_empty()
            || !catalog_has_supertrait(catalog, source_name, target_name)
        {
            return Err(format!(
                "invalid existential upcast request `{source_name}` to `{target_name}`"
            ));
        }
    }
    // An outer upcast can consume the result of an inner one. Close the
    // request set before assigning IDs so source order cannot affect the plan.
    loop {
        let mut additions = Vec::new();
        for (target, source) in &upcast_requests {
            for (_, _, existential, concrete) in &entries {
                if existential == source
                    && !entries.iter().any(|(_, _, existing, candidate)| {
                        existing == target && candidate == concrete
                    })
                {
                    additions.push((
                        type_key(target),
                        type_key(concrete),
                        target.clone(),
                        concrete.clone(),
                    ));
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        entries.extend(additions);
    }
    entries.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    entries.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);

    let mut witnesses = Vec::with_capacity(entries.len());
    for (_, _, existential, concrete) in entries {
        let Type::Dyn(trait_name, trait_args) = &existential else {
            return Err(format!(
                "existential witness request must name `dyn Trait`, got `{}`",
                type_key(&existential)
            ));
        };
        if trait_args.iter().any(has_free_type_variable) || has_free_type_variable(&concrete) {
            return Err(format!(
                "existential witness requests must be fully substituted, got `{} -> {}`",
                type_key(&concrete),
                type_key(&existential)
            ));
        }
        let (implementation, bindings) =
            resolve_impl(&impls, trait_name, trait_args, &concrete)?;
        let static_layout = layout_from_catalog(catalog, &existential)?;
        let mut slots = Vec::new();
        for dispatch in &static_layout.slots {
            let owner = &dispatch.owner_trait;
            let owner_trait = traits
                .get(owner.as_str())
                .ok_or_else(|| format!("existential witness references unknown trait `{owner}`"))?;
            let (owner_impl, owner_bindings) = if owner == trait_name {
                (implementation, bindings.clone())
            } else {
                resolve_impl(&impls, owner, &[], &concrete).map_err(|_| {
                    format!(
                        "`{}` implements `{trait_name}` but has no witness impl for supertrait `{owner}`",
                        type_key(&concrete)
                    )
                })?
            };
            let concrete_impl = instantiate_impl(owner_impl, &owner_bindings);
            let vars: HashMap<String, Type> = owner_trait
                .typarams
                .iter()
                .cloned()
                .zip(concrete_impl.trait_args.iter().cloned())
                .collect();
            let method = owner_trait
                .methods
                .iter()
                .find(|method| method.name == dispatch.method)
                .ok_or_else(|| {
                    format!(
                        "existential layout lost method `{}.{}`",
                        dispatch.owner_trait, dispatch.method
                    )
                })?;
            let concrete_slot = slot(
                owner_trait,
                owner_impl,
                &concrete_impl,
                method,
                &vars,
                &owner_bindings,
            )?;
            let concrete_conventions = std::iter::once(concrete_slot.receiver)
                .chain(concrete_slot.conventions.iter().copied())
                .collect::<Vec<_>>();
            if concrete_slot.params != dispatch.params
                || concrete_slot.result != dispatch.result
                || concrete_conventions != dispatch.conventions
            {
                return Err(format!(
                    "existential witness adapter ABI for `{}.{}` drifted from its static slot layout",
                    dispatch.owner_trait, dispatch.method
                ));
            }
            slots.push(concrete_slot);
        }
        witnesses.push(Witness {
            id: u32::try_from(witnesses.len())
                .map_err(|_| "existential witness table exceeds u32 IDs".to_string())?,
            existential,
            concrete,
            slots,
        });
    }
    let mut upcasts = Vec::new();
    for (target, source) in upcast_requests {
        for witness in witnesses.iter().filter(|witness| witness.existential == source) {
            let target_witness = witnesses
                .iter()
                .find(|candidate| {
                    candidate.existential == target && candidate.concrete == witness.concrete
                })
                .ok_or_else(|| {
                    format!(
                        "existential witness plan lost supertrait target `{} -> {}` for `{}`",
                        type_key(&source),
                        type_key(&target),
                        type_key(&witness.concrete)
                    )
                })?;
            upcasts.push(WitnessUpcast {
                source: witness.id,
                target: target_witness.id,
            });
        }
    }
    upcasts.sort_by_key(|upcast| (upcast.source, upcast.target));
    upcasts.dedup_by_key(|upcast| (upcast.source, upcast.target));
    Ok(WitnessPlan { witnesses, upcasts })
}

fn catalog_has_supertrait(catalog: &WitnessCatalog, child: &str, target: &str) -> bool {
    if child == target {
        return false;
    }
    let Some(definition) = catalog.traits.iter().find(|definition| definition.name == child)
    else {
        return false;
    };
    definition.supertraits.iter().any(|supertrait| {
        supertrait == target || catalog_has_supertrait(catalog, supertrait, target)
    })
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

fn resolve_impl<'a>(
    impls: &'a [&ImplDef],
    trait_name: &str,
    trait_args: &[Type],
    concrete: &Type,
) -> Result<(&'a ImplDef, HashMap<String, Type>), String> {
    let mut matches = Vec::new();
    for candidate in impls {
        let Some(bindings) = match_impl_head(candidate, trait_name, trait_args, concrete) else {
            continue;
        };
        let mut visiting = HashSet::new();
        if impl_bounds_hold(impls, candidate, &bindings, &mut visiting) {
            matches.push((*candidate, bindings));
        }
    }
    let Some(found) = matches.first().cloned() else {
        return Err(format!(
            "no linked `impl {}` for `{}` can construct `dyn {}`",
            trait_name,
            type_key(concrete),
            trait_name
        ));
    };
    if matches.len() > 1 {
        return Err(format!(
            "multiple linked `impl {}` declarations match `{}`",
            trait_name,
            type_key(concrete)
        ));
    }
    Ok(found)
}

fn match_impl_head(
    candidate: &ImplDef,
    trait_name: &str,
    trait_args: &[Type],
    concrete: &Type,
) -> Option<HashMap<String, Type>> {
    if candidate.trait_name.as_deref() != Some(trait_name)
        || candidate.trait_args.len() != trait_args.len()
    {
        return None;
    }
    let mut bindings = HashMap::new();
    if !bind_ast_type_vars(&impl_self_type(candidate), concrete, &mut bindings)
        || !candidate
            .trait_args
            .iter()
            .zip(trait_args)
            .all(|(pattern, concrete)| bind_ast_type_vars(pattern, concrete, &mut bindings))
    {
        return None;
    }
    Some(bindings)
}

fn impl_bounds_hold(
    impls: &[&ImplDef],
    candidate: &ImplDef,
    bindings: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> bool {
    candidate.bounds.iter().all(|(variable, trait_name, trait_args)| {
        let Some(target) = bindings.get(variable) else {
            return false;
        };
        let trait_args = trait_args
            .iter()
            .map(|arg| subst_trait_params(arg, bindings))
            .collect::<Vec<_>>();
        has_applicable_impl(impls, trait_name, &trait_args, target, visiting)
    })
}

fn has_applicable_impl(
    impls: &[&ImplDef],
    trait_name: &str,
    trait_args: &[Type],
    concrete: &Type,
    visiting: &mut HashSet<String>,
) -> bool {
    let key = format!(
        "{trait_name}({}):{}",
        trait_args.iter().map(type_key).collect::<Vec<_>>().join(","),
        type_key(concrete)
    );
    if !visiting.insert(key.clone()) {
        return false;
    }
    let found = impls.iter().any(|candidate| {
        let Some(bindings) = match_impl_head(candidate, trait_name, trait_args, concrete) else {
            return false;
        };
        impl_bounds_hold(impls, candidate, &bindings, visiting)
    });
    visiting.remove(&key);
    found
}

fn instantiate_impl(implementation: &ImplDef, bindings: &HashMap<String, Type>) -> ImplDef {
    let mut concrete = implementation.clone();
    concrete.trait_args = concrete
        .trait_args
        .iter()
        .map(|ty| subst_trait_params(ty, bindings))
        .collect();
    concrete.target_args = concrete
        .target_args
        .iter()
        .map(|ty| subst_trait_params(ty, bindings))
        .collect();
    concrete
}

fn slot(
    owner: &TraitDef,
    implementation: &ImplDef,
    concrete_implementation: &ImplDef,
    method: &MethodSig,
    vars: &HashMap<String, Type>,
    bindings: &HashMap<String, Type>,
) -> Result<WitnessSlot, String> {
    if method.params.first().is_none_or(|param| param.name != "self") {
        return Err(format!(
            "trait method `{}.{}` has no receiver and cannot occupy an existential witness slot",
            owner.name, method.name
        ));
    }
    let template_trait_params: HashMap<String, Type> = owner
        .typarams
        .iter()
        .cloned()
        .zip(implementation.trait_args.iter().cloned())
        .collect();
    Ok(WitnessSlot {
        owner_trait: owner.name.clone(),
        method: method.name.clone(),
        adapter: monomorphic_impl_method_name(
            &owner.name,
            implementation,
            method,
            &template_trait_params,
            bindings,
        )?,
        receiver: method.params[0].convention,
        params: method
            .params
            .iter()
            .skip(1)
            .map(|param| {
                let ty = param.ty.as_ref().ok_or_else(|| {
                    format!(
                        "trait method `{}.{}` parameter `{}` has no type; existential adapter signatures must be fully typed",
                        owner.name, method.name, param.name
                    )
                })?;
                Ok(expected_method_type(ty, concrete_implementation, vars))
            })
            .collect::<Result<Vec<_>, String>>()?,
        result: ret_type(&method.ret, concrete_implementation, vars),
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
        Type::Named(name, args) => format!(
            "named:{name}({})",
            args.iter().map(type_key).collect::<Vec<_>>().join(",")
        ),
        Type::Dyn(name, args) => format!(
            "dyn:{name}({})",
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

fn has_free_type_variable(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) => {
            (args.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !name.contains('.'))
                || args.iter().any(has_free_type_variable)
        }
        Type::Dyn(_, args) | Type::Tuple(args) => args.iter().any(has_free_type_variable),
        Type::Fn(params, result, _) => {
            params.iter().any(has_free_type_variable) || has_free_type_variable(result)
        }
        Type::Qualified(_, inner) => has_free_type_variable(inner),
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
        let plan = build(
            &module,
            [(
                Type::Dyn("Render".into(), Vec::new()),
                Type::Named("Label".into(), Vec::new()),
            )],
        )
        .expect("witness plan");
        let layout = layout(&module, &Type::Dyn("Render".into(), Vec::new()))
            .expect("static existential layout");
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
        assert_eq!(
            layout
                .slots
                .iter()
                .map(|slot| (slot.owner_trait.as_str(), slot.method.as_str()))
                .collect::<Vec<_>>(),
            [("Base", "id"), ("Render", "render"), ("Render", "replace")]
        );
        assert_eq!(
            layout.slots[2].conventions,
            [Convention::Var, Convention::Let]
        );
        assert_eq!(
            layout.slot("Render", "replace").map(|(index, _)| index),
            Some(2)
        );
        assert_eq!(
            witness.slot("Render", "replace").map(|(index, _)| index),
            Some(2)
        );
        assert_eq!(plan.by_id(witness.id), Some(witness));
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
        let requests = [
            (
                Type::Dyn("Show".into(), Vec::new()),
                Type::Named("B".into(), Vec::new()),
            ),
            (
                Type::Dyn("Show".into(), Vec::new()),
                Type::Named("A".into(), Vec::new()),
            ),
        ];
        let left = build(&source(&format!("{a}{b}")), requests.clone()).expect("left");
        let right = build(&source(&format!("{b}{a}")), requests).expect("right");
        assert_eq!(left, right);
    }

    #[test]
    fn supertrait_upcasts_keep_the_same_concrete_payload() {
        let module = parser::parse_module(
            "trait Base:\n\
             \x20   fn base(self) -> Int\n\n\
             trait Render: Base:\n\
             \x20   fn render(self) -> Int\n\n\
             type Label:\n\
             \x20   Label(Int)\n\n\
             impl Base for Label:\n\
             \x20   fn base(self) -> Int:\n\
             \x20       1\n\n\
             impl Render for Label:\n\
             \x20   fn render(self) -> Int:\n\
             \x20       2\n",
        )
        .expect("parse");
        let render = Type::Dyn("Render".into(), Vec::new());
        let base = Type::Dyn("Base".into(), Vec::new());
        let label = Type::Named("Label".into(), Vec::new());
        let plan = build_from_catalog_with_upcasts(
            &WitnessCatalog::from_module(&module),
            [(render.clone(), label.clone())],
            [(base.clone(), render.clone())],
        )
        .expect("build supertrait witnesses");

        let render_id = plan.get(&render, &label).expect("render witness").id;
        let base_id = plan.upcast(render_id, &base).expect("base upcast");
        assert_eq!(plan.by_id(base_id).expect("base witness").concrete, label);
        assert_eq!(plan.by_id(base_id).expect("base witness").existential, base);
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
        let plan = build(
            &module,
            [
                (
                    Type::Dyn(
                        "Convert".into(),
                        vec![Type::Named("Int".into(), Vec::new())],
                    ),
                    Type::Named("Box".into(), Vec::new()),
                ),
                (
                    Type::Dyn(
                        "Convert".into(),
                        vec![Type::Named("String".into(), Vec::new())],
                    ),
                    Type::Named("Box".into(), Vec::new()),
                ),
            ],
        )
        .expect("witness plan");
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

    #[test]
    fn conditional_impl_bounds_gate_witness_selection() {
        let module = parser::parse_module(
            "trait Show:\n\
             \x20   fn show(self) -> String\n\n\
             trait Render:\n\
             \x20   fn render(self) -> String\n\n\
             type HasShow:\n\
             \x20   HasShow\n\n\
             type NoShow:\n\
             \x20   NoShow\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Show for HasShow:\n\
             \x20   fn show(self) -> String:\n\
             \x20       \"shown\"\n\n\
             impl Render for Box(a) where a: Show:\n\
             \x20   fn render(self) -> String:\n\
             \x20       \"rendered\"\n",
        )
        .expect("parse conditional impl");
        let existential = Type::Dyn("Render".into(), Vec::new());
        let boxed = |element: &str| {
            Type::Named(
                "Box".into(),
                vec![Type::Named(element.into(), Vec::new())],
            )
        };

        let plan = build(
            &module,
            [(existential.clone(), boxed("HasShow"))],
        )
        .expect("satisfied bound selects witness");
        assert_eq!(plan.witnesses.len(), 1);

        let error = build(&module, [(existential, boxed("NoShow"))])
            .expect_err("unsatisfied bound must reject witness construction");
        assert!(error.contains("no linked `impl Render`"), "{error}");
    }

    #[test]
    fn catalog_includes_synthesized_anonymous_union_impls() {
        let module = parser::parse_module(
            "trait Show:\n\
             \x20   fn show(self) -> String\n\n\
             impl Show for Int:\n\
             \x20   fn show(self) -> String:\n\
             \x20       \"int\"\n\n\
             impl Show for String:\n\
             \x20   fn show(self) -> String:\n\
             \x20       self\n\n\
             fn describe(value: .[Count(Int) | Text(String)]) -> String:\n\
             \x20   \"value\"\n",
        )
        .expect("parse anonymous union");
        let catalog = WitnessCatalog::from_module(&module);
        let implementation = catalog
            .impls
            .iter()
            .find(|implementation| {
                implementation.trait_name.as_deref() == Some("Show")
                    && implementation.type_name.starts_with("__union")
            })
            .expect("anonymous union Show impl");
        let concrete = Type::Named(
            implementation.type_name.clone(),
            vec![
                Type::Named("Int".into(), Vec::new()),
                Type::Named("String".into(), Vec::new()),
            ],
        );
        let plan = build_from_catalog(
            &catalog,
            [(Type::Dyn("Show".into(), Vec::new()), concrete.clone())],
        )
        .expect("synthesized impl constructs a witness");
        assert_eq!(plan.witnesses[0].concrete, concrete);
    }

    #[test]
    fn provided_methods_use_the_instantiated_trait_abi() {
        let module = parser::parse_module(
            "trait Convert(t):\n\
             \x20   fn convert(self, value: t) -> t\n\n\
             type Box:\n\
             \x20   Box(Int)\n\n\
             impl Convert(Int) for Box:\n\
             \x20   fn convert(self, value: t) -> t:\n\
             \x20       value\n",
        )
        .expect("parse");
        let plan = build(
            &module,
            [(
                Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                Type::Named("Box".into(), Vec::new()),
            )],
        )
        .expect("provided-method witness");

        assert_eq!(
            plan.witnesses[0].slots[0].adapter,
            "Convert__Int__Box__convert"
        );
        assert_eq!(
            plan.witnesses[0].slots[0].params,
            [Type::Named("Int".into(), Vec::new())]
        );
    }

    #[test]
    fn parameterized_trait_defaults_use_the_existing_specialization_symbol() {
        let module = parser::parse_module(
            "trait Convert(t):\n\
             \x20   fn convert(self, value: t) -> t:\n\
             \x20       value\n\n\
             type Box:\n\
             \x20   Box(Int)\n\n\
             impl Convert(Int) for Box\n",
        )
        .expect("parse");
        let plan = build(
            &module,
            [(
                Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                Type::Named("Box".into(), Vec::new()),
            )],
        )
        .expect("default-method witness");

        assert_eq!(
            plan.witnesses[0].slots[0].adapter,
            "Convert__Int__Box__convert"
        );
    }

    #[test]
    fn receiver_binding_wins_a_same_spelled_trait_parameter_for_default_symbols() {
        let module = parser::parse_module(
            "trait Tag(t):\n\
             \x20   fn tag(self) -> Int:\n\
             \x20       1\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Tag(Int) for Box(t)\n",
        )
        .expect("parse");
        let plan = build(
            &module,
            [(
                Type::Dyn(
                    "Tag".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                Type::Named(
                    "Box".into(),
                    vec![Type::Named("String".into(), Vec::new())],
                ),
            )],
        )
        .expect("same-spelled generic namespaces");

        assert_eq!(
            plan.witnesses[0].slots[0].adapter,
            "Tag__Int__Box__tag__String"
        );
    }

    #[test]
    fn default_symbols_separate_trait_and_target_generic_namespaces() {
        let module = parser::parse_module(
            "trait Convert(t):\n\
             \x20   fn convert(self, value: t) -> t:\n\
             \x20       value\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Convert(a) for Box(t)\n",
        )
        .expect("parse");
        let plan = build(
            &module,
            [(
                Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                Type::Named(
                    "Box".into(),
                    vec![Type::Named("String".into(), Vec::new())],
                ),
            )],
        )
        .expect("separate default-method generic namespaces");

        assert_eq!(
            plan.witnesses[0].slots[0].adapter,
            "Convert__a__Box__convert__String__Int"
        );
    }

    #[test]
    fn provided_symbols_separate_trait_and_target_generic_namespaces() {
        let module = parser::parse_module(
            "trait Convert(t):\n\
             \x20   fn convert(self, value: t) -> t\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Convert(a) for Box(t):\n\
             \x20   fn convert(self, value: a) -> a:\n\
             \x20       value\n",
        )
        .expect("parse");
        let plan = build(
            &module,
            [(
                Type::Dyn(
                    "Convert".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                Type::Named(
                    "Box".into(),
                    vec![Type::Named("String".into(), Vec::new())],
                ),
            )],
        )
        .expect("provided-method generic namespaces");

        assert_eq!(
            plan.witnesses[0].slots[0].adapter,
            "Convert__a__Box__convert__String__Int"
        );
    }

    #[test]
    fn witness_slots_reject_untyped_explicit_arguments() {
        let module = parser::parse_module(
            "trait Bad:\n\
             \x20   fn call(self, value) -> Int\n\n\
             type Value:\n\
             \x20   Value(Int)\n\n\
             impl Bad for Value:\n\
             \x20   fn call(self, value) -> Int:\n\
             \x20       0\n",
        )
        .expect("parse");
        let error = build(
            &module,
            [(
                Type::Dyn("Bad".into(), Vec::new()),
                Type::Named("Value".into(), Vec::new()),
            )],
        )
        .expect_err("witness ABI may not guess an argument type");
        assert!(
            error.contains("parameter `value` has no type")
                && error.contains("must be fully typed"),
            "{error}"
        );
    }

    #[test]
    fn generic_impls_are_instantiated_for_closed_witness_requests() {
        let module = parser::parse_module(
            "trait Show:\n\
             \x20   fn show(self) -> String\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Show for Box(a):\n\
             \x20   fn show(self) -> String:\n\
             \x20       \"box\"\n",
        )
        .expect("parse");
        let concrete = Type::Named(
            "Box".into(),
            vec![Type::Named("Int".into(), Vec::new())],
        );
        let plan = build(
            &module,
            [(Type::Dyn("Show".into(), Vec::new()), concrete.clone())],
        )
        .expect("closed generic witness");
        let witness = plan
            .get(&Type::Dyn("Show".into(), Vec::new()), &concrete)
            .expect("Box(Int) witness");

        assert_eq!(witness.concrete, concrete);
        assert_eq!(witness.slots[0].adapter, "Show__Box__show__Int");
    }

    #[test]
    fn dispatch_index_is_dense_by_witness_id_and_static_slot() {
        let slot = |adapter: &str| WitnessSlot {
            owner_trait: "Show".to_string(),
            method: "show".to_string(),
            adapter: adapter.to_string(),
            receiver: Convention::Let,
            params: Vec::new(),
            result: Type::Named("String".to_string(), Vec::new()),
            conventions: Vec::new(),
        };
        let existential = Type::Dyn("Show".to_string(), Vec::new());
        let plan = WitnessPlan {
            witnesses: vec![
                Witness {
                    id: 0,
                    existential: existential.clone(),
                    concrete: Type::Named("Left".to_string(), Vec::new()),
                    slots: vec![slot("show_left")],
                },
                Witness {
                    id: 1,
                    existential,
                    concrete: Type::Named("Right".to_string(), Vec::new()),
                    slots: vec![slot("show_right"), slot("debug_right")],
                },
            ],
            upcasts: Vec::new(),
        };
        let index = plan.dispatch_index().expect("dense index");
        assert_eq!(index.stride(), 2);
        assert_eq!(index.table_index(&plan.witnesses[0], 0), Some(0));
        assert_eq!(index.table_index(&plan.witnesses[1], 0), Some(2));
        assert_eq!(index.table_index(&plan.witnesses[1], 1), Some(3));
        assert_eq!(index.table_index(&plan.witnesses[0], 1), None);
    }

    #[test]
    fn open_generic_requests_never_receive_runtime_witness_ids() {
        let module = parser::parse_module(
            "trait Show:\n\
             \x20   fn show(self) -> String\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Show for Box(a):\n\
             \x20   fn show(self) -> String:\n\
             \x20       \"box\"\n",
        )
        .expect("parse");
        let error = build(
            &module,
            [(
                Type::Dyn("Show".into(), Vec::new()),
                Type::Named(
                    "Box".into(),
                    vec![Type::Named("a".into(), Vec::new())],
                ),
            )],
        )
        .expect_err("open impl template is not a runtime witness");

        assert!(error.contains("must be fully substituted"), "{error}");
    }

    #[test]
    fn generic_supertrait_impls_match_after_substitution_not_variable_spelling() {
        let module = parser::parse_module(
            "trait Base:\n\
             \x20   fn base(self) -> Int\n\n\
             trait Render: Base:\n\
             \x20   fn render(self) -> String\n\n\
             type Box(a):\n\
             \x20   Box(a)\n\n\
             impl Base for Box(a):\n\
             \x20   fn base(self) -> Int:\n\
             \x20       1\n\n\
             impl Render for Box(b):\n\
             \x20   fn render(self) -> String:\n\
             \x20       \"box\"\n",
        )
        .expect("parse");
        let concrete = Type::Named(
            "Box".into(),
            vec![Type::Named("Int".into(), Vec::new())],
        );
        let plan = build(
            &module,
            [(Type::Dyn("Render".into(), Vec::new()), concrete)],
        )
        .expect("generic supertrait witness");

        assert_eq!(
            plan.witnesses[0]
                .slots
                .iter()
                .map(|slot| slot.adapter.as_str())
                .collect::<Vec<_>>(),
            ["Base__Box__base__Int", "Render__Box__render__Int"]
        );
    }
}
