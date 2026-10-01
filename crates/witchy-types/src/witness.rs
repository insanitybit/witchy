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

/// Names derived from an actual retained PartialEq declaration and matching
/// implementation. Neither name is an authority token: preparation must find
/// or ordinarily specialize the executable body and recheck its signature.
pub(crate) struct EqualityMethod {
    pub template: String,
    pub instance: String,
    pub pure: bool,
}

impl WitnessCatalog {
    pub(crate) fn equality_method(&self, concrete: &Type) -> Result<Option<EqualityMethod>, String> {
        let mut declarations = self.traits.iter().filter(|definition| {
            crate::traits::is_standard_trait_identity(&definition.name, "cmp", "PartialEq")
        });
        let Some(declaration) = declarations.next() else { return Ok(None) };
        if declarations.next().is_some() {
            return Err("structural equality trait identity is ambiguous".into());
        }
        let method = declaration.methods.iter().find(|method| method.name == "eq")
            .ok_or("structural equality trait has no eq method")?;
        let impls = self.impls.iter().collect::<Vec<_>>();
        if !has_applicable_impl(&impls, &declaration.name, &[], concrete, &mut HashSet::new()) {
            return Ok(None);
        }
        let (implementation, bindings) = resolve_impl(&impls, &declaration.name, &[], concrete)?;
        let provided = implementation.methods.iter().find(|implementation| implementation.name == "eq");
        Ok(Some(EqualityMethod {
            template: crate::traits::mangle(Some(&declaration.name), &implementation.trait_args,
                &implementation.type_name, "eq"),
            instance: monomorphic_impl_method_name(&declaration.name, implementation, method,
                &HashMap::new(), &bindings)?,
            pure: provided.map_or(method.pure, |method| method.pure),
        }))
    }

    /// Reconstruct ordinary dispatch inputs in a disposable specialization
    /// module. The caller imports only newly reached executable functions, not
    /// these declarations or regenerated versions of existing checked bodies.
    pub(crate) fn equality_dispatch_module(&self, mut module: Module) -> Module {
        let mut lifted = HashSet::new();
        for implementation in &self.impls {
            for method in &implementation.methods {
                lifted.insert(crate::traits::mangle(implementation.trait_name.as_deref(),
                    &implementation.trait_args, &implementation.type_name, &method.name));
            }
            if let Some(declaration) = self.traits.iter().find(|declaration|
                implementation.trait_name.as_deref() == Some(declaration.name.as_str()))
            {
                for method in &declaration.methods {
                    lifted.insert(crate::traits::mangle(Some(&declaration.name),
                        &implementation.trait_args, &implementation.type_name, &method.name));
                }
            }
        }
        // Prefer already elaborated template bodies: their calls to inherent
        // methods have been resolved by the original checked link as well.
        let mut implementations = self.impls.clone();
        for implementation in &mut implementations {
            for method in &mut implementation.methods {
                let name = crate::traits::mangle(implementation.trait_name.as_deref(),
                    &implementation.trait_args, &implementation.type_name, &method.name);
                if let Some(function) = module.items.iter().find_map(|item| match item {
                    Item::Function(function) if function.name == name => Some(function),
                    _ => None,
                }) {
                    method.body = function.body.clone();
                }
            }
        }
        module.items.retain(|item| match item {
            Item::Trait(_) | Item::Impl(_) => false,
            Item::Function(function) => !lifted.contains(&function.name),
            _ => true,
        });
        module.items.extend(self.traits.iter().cloned().map(Item::Trait));
        module.items.extend(implementations.into_iter().map(Item::Impl));
        module
    }

    /// Resolve a dictionary comparator from retained declarations, never from
    /// a backend reconstruction of a mangled function name. The canonical
    /// frontend still checks the selected executable body and its signature.
    pub(crate) fn dictionary_equality(&self, concrete: &Type) -> Result<String, String> {
        let mut declarations = self.traits.iter().filter(|definition| {
            crate::traits::is_standard_trait_identity(&definition.name, "cmp", "PartialEq")
        });
        let declaration = declarations.next().ok_or("dictionary equality trait is absent")?;
        if declarations.next().is_some() {
            return Err("dictionary equality trait identity is ambiguous".into());
        }
        let method = declaration.methods.iter().find(|method| method.name == "eq")
            .ok_or("dictionary equality method is absent")?;
        let impls = self.impls.iter().collect::<Vec<_>>();
        let (implementation, bindings) = resolve_impl(&impls, &declaration.name, &[], concrete)?;
        monomorphic_impl_method_name(
            &declaration.name, implementation, method, &HashMap::new(), &bindings,
        )
    }

    /// Resolve the checked Hash implementation for a concrete dictionary key.
    pub(crate) fn dictionary_hash(&self, concrete: &Type) -> Result<String, String> {
        let mut declarations = self.traits.iter().filter(|definition| {
            crate::traits::is_standard_trait_identity(&definition.name, "cmp", "Hash")
        });
        let declaration = declarations.next().ok_or("dictionary Hash trait is absent")?;
        if declarations.next().is_some() {
            return Err("dictionary Hash trait identity is ambiguous".into());
        }
        let method = declaration.methods.iter().find(|method| method.name == "hash")
            .ok_or("dictionary Hash method is absent")?;
        let impls = self.impls.iter().collect::<Vec<_>>();
        let (implementation, bindings) = resolve_impl(&impls, &declaration.name, &[], concrete)?;
        monomorphic_impl_method_name(
            &declaration.name, implementation, method, &HashMap::new(), &bindings,
        )
    }

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
            .impls
            .extend(crate::traits::synthesize_anon_record_impls(
                &module.items,
                &trait_methods,
            ));
        catalog
    }

    /// Decide one closed concrete-to-trait relation using the same impl heads,
    /// conditional bounds, and qualified identities as witness construction.
    pub fn implements(&self, concrete: &Type, existential: &Type) -> Result<bool, String> {
        let Type::Dyn(trait_name, trait_args) = existential else {
            return Err("runtime trait query must name `dyn Trait`".to_string());
        };
        let definition = self
            .traits
            .iter()
            .find(|definition| definition.name == *trait_name)
            .ok_or_else(|| {
                format!("runtime trait query references unknown trait `{trait_name}`")
            })?;
        if definition.typarams.len() != trait_args.len() {
            return Err(format!(
                "trait `{trait_name}` expects {} type argument(s), got {}",
                definition.typarams.len(),
                trait_args.len()
            ));
        }
        if trait_args.iter().any(has_free_type_variable) || has_free_type_variable(concrete) {
            return Err("runtime trait queries must be fully substituted".to_string());
        }
        let impls = self.impls.iter().collect::<Vec<_>>();
        Ok(has_applicable_impl(
            &impls,
            trait_name,
            trait_args,
            concrete,
            &mut HashSet::new(),
        ))
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

pub(crate) fn layout_from_catalog(
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
            if method
                .params
                .first()
                .is_none_or(|param| param.name != "self")
            {
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
                .unwrap_or_else(|| Type::Tuple(Vec::new()));
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
    let impls: Vec<&ImplDef> = catalog.impls.iter().collect();

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
        let (implementation, bindings) = resolve_impl(&impls, trait_name, trait_args, &concrete)?;
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
        for witness in witnesses
            .iter()
            .filter(|witness| witness.existential == source)
        {
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
    let Some(definition) = catalog
        .traits
        .iter()
        .find(|definition| definition.name == child)
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
    candidate
        .bounds
        .iter()
        .all(|(variable, trait_name, trait_args)| {
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
        trait_args
            .iter()
            .map(type_key)
            .collect::<Vec<_>>()
            .join(","),
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
    if method
        .params
        .first()
        .is_none_or(|param| param.name != "self")
    {
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
        Type::Fn(params, result, conventions, qualifiers) => format!(
            "fn[{:?};{}{}]({})->{}",
            conventions,
            if qualifiers.pure { "pure" } else { "ordinary" },
            if qualifiers.once { ";once" } else { "" },
            params.iter().map(type_key).collect::<Vec<_>>().join(","),
            type_key(result)
        ),
        Type::Qualified(qualifier, inner) => format!("{qualifier:?}:{}", type_key(inner)),
        Type::Slice(inner) => format!("[{}]", type_key(inner)),
        Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: structural record composition reached witness type keys before records::lower normalized it"
        ),
    }
}

fn has_free_type_variable(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) => {
            (args.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !name.contains('.')
                && name != "str")
                || args.iter().any(has_free_type_variable)
        }
        Type::Dyn(_, args) | Type::Tuple(args) => args.iter().any(has_free_type_variable),
        Type::Fn(params, result, _, _) => {
            params.iter().any(has_free_type_variable) || has_free_type_variable(result)
        }
        Type::Qualified(_, inner) => has_free_type_variable(inner),
        Type::Slice(inner) => has_free_type_variable(inner),
        Type::RecordCompose { base, fields } => {
            has_free_type_variable(base) || fields.iter().any(|(_, ty)| has_free_type_variable(ty))
        }
    }
}
