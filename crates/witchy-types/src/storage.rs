//! Representation-neutral classification for values that transitively contain
//! a WebAssembly reference. Lowering uses this semantic fact to choose a
//! reference-safe aggregate representation; stage-specific support checks live
//! elsewhere and must not redefine the classification.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use witchy_cap_model::CapabilityKind;
use witchy_syntax::ast::{Item, Module, Type, TypeDef};
use witchy_canonical_source::capabilities::externref_cap_name;

/// The reference leaf that makes a value require reference-safe storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceLeaf {
    ExternRef(&'static str),
    Function,
    /// RFC-0081's owned `{structref payload, i32 witness}` envelope.
    Existential,
}

type StoredParamSummaries<'a> = HashMap<&'a str, HashSet<String>>;

#[derive(Clone, Copy)]
enum FunctionTraversal {
    AsReference,
    IgnoreSignature,
    InspectSignature,
}

/// Resolves module-local aliases and ADTs while classifying concrete types.
/// A positive result means that storing the value solely in linear-memory
/// scalar slots would erase a Wasm reference.
pub struct ReferenceStorageClassifier<'a> {
    defs: HashMap<&'a str, &'a TypeDef>,
    aliases: HashMap<&'a str, (&'a [String], &'a Type)>,
    def_stored_params: StoredParamSummaries<'a>,
    alias_stored_params: StoredParamSummaries<'a>,
}

impl<'a> ReferenceStorageClassifier<'a> {
    pub fn new(module: &'a Module) -> Self {
        Self::from_items(module.items.iter())
    }

    /// The classifier over declarations drawn from any number of modules.
    pub fn from_items(items: impl IntoIterator<Item = &'a Item>) -> Self {
        let mut defs = HashMap::new();
        let mut aliases = HashMap::new();
        for item in items {
            match item {
                Item::Type(def) => {
                    defs.insert(def.name.as_str(), def);
                }
                Item::TypeAlias { name, params, ty } => {
                    aliases.insert(name.as_str(), (params.as_slice(), ty));
                }
                _ => {}
            }
        }
        let (def_stored_params, alias_stored_params) = stored_parameter_summaries(&defs, &aliases);
        Self { defs, aliases, def_stored_params, alias_stored_params }
    }

    pub fn first_reference(&self, ty: &Type) -> Option<ReferenceLeaf> {
        self.classify(
            ty,
            &HashMap::new(),
            &mut HashSet::new(),
            FunctionTraversal::AsReference,
        )
    }

    /// The first migrated host capability stored by `ty`, ignoring function
    /// values. Current Stage 4 aggregate eligibility needs this narrower query:
    /// scalar closure values still have their legacy representation, while an
    /// `externref` can never cross an i64 slot.
    pub fn first_externref(&self, ty: &Type) -> Option<&'static str> {
        match self.classify(
            ty,
            &HashMap::new(),
            &mut HashSet::new(),
            FunctionTraversal::IgnoreSignature,
        ) {
            Some(ReferenceLeaf::ExternRef(cap)) => Some(cap),
            Some(ReferenceLeaf::Function | ReferenceLeaf::Existential) | None => None,
        }
    }

    /// The first migrated capability appearing in either stored data or a
    /// nested function signature. Production closures still use a scalar call
    /// convention, so type checking needs this broader query until their typed
    /// indirect ABI migration is complete.
    pub fn first_externref_including_function_signatures(
        &self,
        ty: &Type,
    ) -> Option<&'static str> {
        match self.classify(
            ty,
            &HashMap::new(),
            &mut HashSet::new(),
            FunctionTraversal::InspectSignature,
        ) {
            Some(ReferenceLeaf::ExternRef(cap)) => Some(cap),
            Some(ReferenceLeaf::Function | ReferenceLeaf::Existential) | None => None,
        }
    }

    /// The first capability stored by `ty`, independent of its eventual
    /// scalar or reference representation. This is the authority counterpart
    /// to `first_externref`: it follows aliases and only the generic parameters
    /// an aggregate actually stores, while ignoring capability names that
    /// appear solely in a function signature.
    pub fn first_stored_capability(&self, ty: &Type) -> Option<String> {
        self.classify_stored_capability(ty, &HashMap::new(), &mut HashSet::new())
    }

    pub fn requires_reference_storage(&self, ty: &Type) -> bool {
        self.first_reference(ty).is_some()
    }

    fn classify(
        &self,
        ty: &Type,
        bindings: &HashMap<String, Type>,
        seen: &mut HashSet<String>,
        functions: FunctionTraversal,
    ) -> Option<ReferenceLeaf> {
        match ty {
            Type::Qualified(_, inner) => self.classify(inner, bindings, seen, functions),
            Type::Slice(elem) => self.classify(elem, bindings, seen, functions),
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: structural record composition reached storage classification before records::lower normalized it"
            ),
            Type::Tuple(items) => {
                items
                    .iter()
                    .find_map(|item| self.classify(item, bindings, seen, functions))
            }
            // The existential envelope is itself a GC reference regardless of
            // whether the trait carries type arguments. Its hidden concrete
            // payload is classified before construction and never crosses a
            // caller's scalar aggregate boundary directly.
            Type::Dyn(_, _) => Some(ReferenceLeaf::Existential),
            // A first-class function value is itself a reference regardless of
            // the scalar/reference shapes in its signature.
            Type::Fn(params, ret, _, _) => match functions {
                FunctionTraversal::AsReference => Some(ReferenceLeaf::Function),
                FunctionTraversal::IgnoreSignature => None,
                FunctionTraversal::InspectSignature => params
                    .iter()
                    .chain(std::iter::once(ret.as_ref()))
                    .find_map(|item| self.classify(item, bindings, seen, functions)),
            },
            Type::Named(name, args) => {
                if args.is_empty()
                    && let Some(bound) = bindings.get(name)
                {
                    let key = format!("binding:{name}");
                    if !seen.insert(key.clone()) {
                        return None;
                    }
                    let hit = self.classify(bound, bindings, seen, functions);
                    seen.remove(&key);
                    return hit;
                }
                if let Some(cap) = externref_cap_name(name) {
                    return Some(ReferenceLeaf::ExternRef(cap));
                }
                // The current generic and closure ABIs treat every explicit type
                // argument as a representation dependency, even when a local ADT
                // does not store that parameter. Keep this broader query aligned
                // with the inferred-type boundary checker; `first_externref`
                // remains the precise storage query.
                if matches!(functions, FunctionTraversal::InspectSignature)
                    && let Some(hit) = args
                        .iter()
                        .find_map(|arg| self.classify(arg, bindings, seen, functions))
                {
                    return Some(hit);
                }

                if let Some((params, target)) = self.aliases.get(name.as_str()) {
                    let key = format!("alias:{name}");
                    if !seen.insert(key.clone()) {
                        return self.classify_stored_args(
                            params,
                            args,
                            self.alias_stored_params.get(name.as_str()),
                            bindings,
                            seen,
                            functions,
                        );
                    }
                    let nested = bind(params, args, bindings);
                    let hit = self.classify(target, &nested, seen, functions);
                    seen.remove(&key);
                    return hit;
                }

                if let Some(def) = self.defs.get(name.as_str()) {
                    let key = format!("type:{name}");
                    let params = crate::typeck::type_def_params(def);
                    if !seen.insert(key.clone()) {
                        return self.classify_stored_args(
                            &params,
                            args,
                            self.def_stored_params.get(name.as_str()),
                            bindings,
                            seen,
                            functions,
                        );
                    }
                    let nested = bind(&params, args, bindings);
                    let hit = def
                        .variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter())
                        .find_map(|field| {
                            self.classify(field, &nested, seen, functions)
                        });
                    seen.remove(&key);
                    return hit;
                }

                // Built-in containers have no local TypeDef. Their runtime
                // payloads preserve each type argument, so any reference-bearing
                // argument makes the whole value reference-bearing.
                args
                    .iter()
                    .find_map(|arg| self.classify(arg, bindings, seen, functions))
            }
        }
    }

    fn classify_stored_args(
        &self,
        params: &[String],
        args: &[Type],
        relevant: Option<&HashSet<String>>,
        bindings: &HashMap<String, Type>,
        seen: &mut HashSet<String>,
        functions: FunctionTraversal,
    ) -> Option<ReferenceLeaf> {
        let relevant = relevant?;
        params
            .iter()
            .zip(args)
            .filter(|(param, _)| relevant.contains(param.as_str()))
            .find_map(|(_, arg)| self.classify(arg, bindings, seen, functions))
    }

    fn classify_stored_capability(
        &self,
        ty: &Type,
        bindings: &HashMap<String, Type>,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        match ty {
            Type::Qualified(_, inner) => {
                self.classify_stored_capability(inner, bindings, seen)
            }
            Type::Slice(inner) => {
                self.classify_stored_capability(inner, bindings, seen)
            }
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: structural record composition reached storage classification before records::lower normalized it"
            ),
            Type::Tuple(items) => items
                .iter()
                .find_map(|item| self.classify_stored_capability(item, bindings, seen)),
            Type::Dyn(_, args) => args
                .iter()
                .find_map(|arg| self.classify_stored_capability(arg, bindings, seen)),
            // A function value stores its closure/code identity, not values of
            // the capability types mentioned in its signature.
            Type::Fn(..) => None,
            Type::Named(name, args) => {
                if args.is_empty()
                    && let Some(bound) = bindings.get(name)
                {
                    let key = format!("authority-binding:{name}");
                    if !seen.insert(key.clone()) {
                        return None;
                    }
                    let hit = self.classify_stored_capability(bound, bindings, seen);
                    seen.remove(&key);
                    return hit;
                }

                if let Some(kind) = CapabilityKind::from_name(name) {
                    return Some(kind.name().to_owned());
                }

                if let Some((params, target)) = self.aliases.get(name.as_str()) {
                    let key = format!("authority-alias:{name}");
                    if !seen.insert(key.clone()) {
                        return self.classify_stored_capability_args(
                            params,
                            args,
                            self.alias_stored_params.get(name.as_str()),
                            bindings,
                            seen,
                        );
                    }
                    let nested = bind(params, args, bindings);
                    let hit = self.classify_stored_capability(target, &nested, seen);
                    seen.remove(&key);
                    return hit;
                }

                if let Some(def) = self.defs.get(name.as_str()) {
                    if def.is_capability {
                        return Some(name.clone());
                    }
                    let key = format!("authority-type:{name}");
                    let params = crate::typeck::type_def_params(def);
                    if !seen.insert(key.clone()) {
                        return self.classify_stored_capability_args(
                            &params,
                            args,
                            self.def_stored_params.get(name.as_str()),
                            bindings,
                            seen,
                        );
                    }
                    let nested = bind(&params, args, bindings);
                    let hit = def
                        .variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter())
                        .find_map(|field| {
                            self.classify_stored_capability(field, &nested, seen)
                        });
                    seen.remove(&key);
                    return hit;
                }

                args.iter()
                    .find_map(|arg| self.classify_stored_capability(arg, bindings, seen))
            }
        }
    }

    fn classify_stored_capability_args(
        &self,
        params: &[String],
        args: &[Type],
        relevant: Option<&HashSet<String>>,
        bindings: &HashMap<String, Type>,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        let relevant = relevant?;
        params
            .iter()
            .zip(args)
            .filter(|(param, _)| relevant.contains(param.as_str()))
            .find_map(|(_, arg)| self.classify_stored_capability(arg, bindings, seen))
    }
}

/// Substitute a closed nominal instantiation's type arguments through every
/// stored variant field. Representation planning uses this after type checking
/// so a declaration such as `Task(a)` can receive a concrete GC layout without
/// cloning or rewriting the source type declaration.
pub fn instantiate_type_def_fields(def: &TypeDef, args: &[Type]) -> Vec<Vec<Type>> {
    // Runtime layouts are keyed by the complete nominal identity, including
    // declared lifetime arguments.  Lifetime parameters were historically
    // omitted from `type_def_params` because they do not affect scalar type
    // inference; explicit-reference aggregates must nevertheless preserve the
    // full argument-to-field mapping while planning their typed carrier.
    let bindings = bind(
        &witchy_syntax::ast::effective_nominal_type_def_params(def),
        args,
        &HashMap::new(),
    );
    def.variants
        .iter()
        .map(|variant| {
            variant
                .fields
                .iter()
                .map(|field| substitute(field, &bindings, &mut HashSet::new()))
                .collect()
        })
        .collect()
}

fn substitute(
    ty: &Type,
    bindings: &HashMap<String, Type>,
    resolving: &mut HashSet<String>,
) -> Type {
    match ty {
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute(inner, bindings, resolving)),
        ),
        Type::Slice(elem) => Type::Slice(Box::new(substitute(elem, bindings, resolving))),
        Type::Tuple(items) => {
            Type::Tuple(items.iter().map(|item| substitute(item, bindings, resolving)).collect())
        }
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute(base, bindings, resolving)),
            fields: fields
                .iter()
                .map(|(name, ty)| (name.clone(), substitute(ty, bindings, resolving)))
                .collect(),
        },
        Type::Dyn(name, args) => Type::Dyn(
            name.clone(),
            args.iter().map(|arg| substitute(arg, bindings, resolving)).collect(),
        ),
        Type::Fn(params, ret, conventions, qualifiers) => Type::Fn(
            params.iter().map(|param| substitute(param, bindings, resolving)).collect(),
            Box::new(substitute(ret, bindings, resolving)),
            conventions.clone(),
            *qualifiers,
        ),
        Type::Named(name, args) => {
            if args.is_empty()
                && let Some(bound) = bindings.get(name)
            {
                if !resolving.insert(name.clone()) {
                    return ty.clone();
                }
                let resolved = substitute(bound, bindings, resolving);
                resolving.remove(name);
                return resolved;
            }
            Type::Named(
                name.clone(),
                args.iter().map(|arg| substitute(arg, bindings, resolving)).collect(),
            )
        }
    }
}

fn bind(
    params: &[String],
    args: &[Type],
    outer: &HashMap<String, Type>,
) -> HashMap<String, Type> {
    let mut bindings = outer.clone();
    for (param, arg) in params.iter().zip(args) {
        let resolved = substitute(arg, outer, &mut HashSet::new());
        if resolved == Type::Named(param.clone(), Vec::new()) {
            bindings.remove(param);
        } else {
            bindings.insert(param.clone(), resolved);
        }
    }
    bindings
}

fn stored_parameter_summaries<'a>(
    defs: &HashMap<&'a str, &'a TypeDef>,
    aliases: &HashMap<&'a str, (&'a [String], &'a Type)>,
) -> (StoredParamSummaries<'a>, StoredParamSummaries<'a>) {
    let mut def_summaries = defs
        .keys()
        .map(|name| (*name, HashSet::new()))
        .collect::<HashMap<_, _>>();
    let mut alias_summaries = aliases
        .keys()
        .map(|name| (*name, HashSet::new()))
        .collect::<HashMap<_, _>>();

    loop {
        let mut changed = false;
        let def_updates = defs
            .iter()
            .map(|(name, def)| {
                let params = crate::typeck::type_def_params(def);
                let mut deps = HashSet::new();
                for field in def.variants.iter().flat_map(|variant| &variant.fields) {
                    deps.extend(storage_dependencies(
                        field,
                        defs,
                        aliases,
                        &def_summaries,
                        &alias_summaries,
                    ));
                }
                deps.retain(|dep| params.contains(dep));
                (*name, deps)
            })
            .collect::<Vec<_>>();
        let alias_updates = aliases
            .iter()
            .map(|(name, (params, target))| {
                let mut deps = storage_dependencies(
                    target,
                    defs,
                    aliases,
                    &def_summaries,
                    &alias_summaries,
                );
                deps.retain(|dep| params.contains(dep));
                (*name, deps)
            })
            .collect::<Vec<_>>();

        for (name, deps) in def_updates {
            if def_summaries.get(name) != Some(&deps) {
                def_summaries.insert(name, deps);
                changed = true;
            }
        }
        for (name, deps) in alias_updates {
            if alias_summaries.get(name) != Some(&deps) {
                alias_summaries.insert(name, deps);
                changed = true;
            }
        }
        if !changed {
            return (def_summaries, alias_summaries);
        }
    }
}

fn storage_dependencies<'a>(
    ty: &Type,
    defs: &HashMap<&'a str, &'a TypeDef>,
    aliases: &HashMap<&'a str, (&'a [String], &'a Type)>,
    def_summaries: &HashMap<&'a str, HashSet<String>>,
    alias_summaries: &HashMap<&'a str, HashSet<String>>,
) -> HashSet<String> {
    match ty {
        Type::Qualified(_, inner) => {
            storage_dependencies(inner, defs, aliases, def_summaries, alias_summaries)
        }
        Type::Slice(inner) => {
            storage_dependencies(inner, defs, aliases, def_summaries, alias_summaries)
        }
        Type::Tuple(items) => items
            .iter()
            .flat_map(|item| {
                storage_dependencies(item, defs, aliases, def_summaries, alias_summaries)
            })
            .collect(),
        Type::RecordCompose { base, fields } => std::iter::once(base.as_ref())
            .chain(fields.iter().map(|(_, ty)| ty))
            .flat_map(|ty| {
                storage_dependencies(ty, defs, aliases, def_summaries, alias_summaries)
            })
            .collect(),
        Type::Dyn(_, args) => args
            .iter()
            .flat_map(|arg| {
                storage_dependencies(arg, defs, aliases, def_summaries, alias_summaries)
            })
            .collect(),
        // A function value's storage does not depend on its signature types.
        Type::Fn(..) => HashSet::new(),
        Type::Named(name, args) => {
            if externref_cap_name(name).is_some() {
                return HashSet::new();
            }
            if let Some((params, _)) = aliases.get(name.as_str()) {
                return mapped_dependencies(
                    params,
                    args,
                    alias_summaries.get(name.as_str()),
                    defs,
                    aliases,
                    def_summaries,
                    alias_summaries,
                );
            }
            if let Some(def) = defs.get(name.as_str()) {
                return mapped_dependencies(
                    &crate::typeck::type_def_params(def),
                    args,
                    def_summaries.get(name.as_str()),
                    defs,
                    aliases,
                    def_summaries,
                    alias_summaries,
                );
            }
            if args.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !name.contains('.')
            {
                return HashSet::from_iter([name.clone()]);
            }
            args.iter()
                .flat_map(|arg| {
                    storage_dependencies(arg, defs, aliases, def_summaries, alias_summaries)
                })
                .collect()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mapped_dependencies<'a>(
    params: &[String],
    args: &[Type],
    relevant: Option<&HashSet<String>>,
    defs: &HashMap<&'a str, &'a TypeDef>,
    aliases: &HashMap<&'a str, (&'a [String], &'a Type)>,
    def_summaries: &HashMap<&'a str, HashSet<String>>,
    alias_summaries: &HashMap<&'a str, HashSet<String>>,
) -> HashSet<String> {
    let Some(relevant) = relevant else { return HashSet::new() };
    params
        .iter()
        .zip(args)
        .filter(|(param, _)| relevant.contains(param.as_str()))
        .flat_map(|(_, arg)| {
            storage_dependencies(arg, defs, aliases, def_summaries, alias_summaries)
        })
        .collect()
}
