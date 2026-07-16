//! Representation-neutral classification for values that transitively contain
//! a WebAssembly reference. Lowering uses this semantic fact to choose a
//! reference-safe aggregate representation; stage-specific support checks live
//! elsewhere and must not redefine the classification.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use witchy_syntax::ast::{Item, Module, Type, TypeDef};

/// The reference leaf that makes a value require reference-safe storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceLeaf {
    ExternRef(&'static str),
    Function,
}

/// The migrated host capabilities represented as unforgeable `externref`
/// values on the compiled backend.
pub fn externref_cap_name(name: &str) -> Option<&'static str> {
    match name {
        "Dir" => Some("Dir"),
        "File" => Some("File"),
        "Net" => Some("Net"),
        "Socket" => Some("Socket"),
        "Listener" => Some("Listener"),
        "Secret" => Some("Secret"),
        _ => None,
    }
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
        let mut defs = HashMap::new();
        let mut aliases = HashMap::new();
        for item in &module.items {
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
            Some(ReferenceLeaf::Function) | None => None,
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
            Some(ReferenceLeaf::Function) | None => None,
        }
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
            Type::Tuple(items) => {
                items
                    .iter()
                    .find_map(|item| self.classify(item, bindings, seen, functions))
            }
            Type::Dyn(_, args) => {
                args
                    .iter()
                    .find_map(|arg| self.classify(arg, bindings, seen, functions))
            }
            // A first-class function value is itself a reference regardless of
            // the scalar/reference shapes in its signature.
            Type::Fn(params, ret, _) => match functions {
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
                    let params = type_params(def);
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
        Type::Tuple(items) => {
            Type::Tuple(items.iter().map(|item| substitute(item, bindings, resolving)).collect())
        }
        Type::Dyn(name, args) => Type::Dyn(
            name.clone(),
            args.iter().map(|arg| substitute(arg, bindings, resolving)).collect(),
        ),
        Type::Fn(params, ret, conventions) => Type::Fn(
            params.iter().map(|param| substitute(param, bindings, resolving)).collect(),
            Box::new(substitute(ret, bindings, resolving)),
            conventions.clone(),
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
                let params = type_params(def);
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
        Type::Tuple(items) => items
            .iter()
            .flat_map(|item| {
                storage_dependencies(item, defs, aliases, def_summaries, alias_summaries)
            })
            .collect(),
        Type::Dyn(_, args) => args
            .iter()
            .flat_map(|arg| {
                storage_dependencies(arg, defs, aliases, def_summaries, alias_summaries)
            })
            .collect(),
        // A function value's storage does not depend on its signature types.
        Type::Fn(_, _, _) => HashSet::new(),
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
                    &type_params(def),
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

fn type_params(def: &TypeDef) -> Vec<String> {
    if !def.params.is_empty() {
        return def.params.clone();
    }
    let mut params = Vec::new();
    for field in def.variants.iter().flat_map(|variant| &variant.fields) {
        collect_implicit_params(field, &mut params);
    }
    params
}

fn collect_implicit_params(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Qualified(_, inner) => collect_implicit_params(inner, out),
        Type::Tuple(items) => {
            for item in items {
                collect_implicit_params(item, out);
            }
        }
        Type::Dyn(_, args) => {
            for arg in args {
                collect_implicit_params(arg, out);
            }
        }
        Type::Fn(params, ret, _) => {
            for param in params {
                collect_implicit_params(param, out);
            }
            collect_implicit_params(ret, out);
        }
        Type::Named(name, args) => {
            if args.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !name.contains('.')
            {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            } else {
                for arg in args {
                    collect_implicit_params(arg, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::ast::Function;

    fn function<'a>(module: &'a Module, name: &str) -> &'a Function {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == name => Some(function),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing function {name}"))
    }

    fn param<'a>(module: &'a Module, function_name: &str) -> &'a Type {
        function(module, function_name).params[0].ty.as_ref().expect("typed parameter")
    }

    #[test]
    fn externref_capability_set_is_explicit_and_closed() {
        for name in ["Dir", "File", "Net", "Socket", "Listener", "Secret"] {
            assert_eq!(externref_cap_name(name), Some(name));
        }
        for name in ["Console", "Clock", "Rand", "Env", "SecretStore", "Exec"] {
            assert_eq!(externref_cap_name(name), None, "{name}");
        }
    }

    #[test]
    fn classifies_direct_nested_generic_recursive_and_phantom_shapes() {
        let module = witchy_syntax::parser::parse_module(
            r#"
type Boxed(a):
    Boxed(a)

type Inferred:
    Inferred(a)

type Phantom(a):
    Phantom

type Node(a):
    Node(a, Option(Node(a)))

type Evolve(a):
    Step(Evolve(fn(Int) -> Int))
    End(a)

type Inner(a):
    Inner(a)

type Outer(a):
    Outer(Inner(a))

type Forward(a) = Boxed(a)

type Grow(a):
    More(Grow(List(a)))
    End(fn(Int) -> Int)

type RecursivePhantom(a):
    Again(RecursivePhantom(List(a)))
    Done

type Callback = fn(Int) -> Int
type CapabilityCallback = fn(File[Read]) -> Nil

type Mixed:
    Mixed(Callback, Dir[Read])

type MixedAlias = (Callback, File[Read])

type RecursiveMixed(a):
    MoreMixed(RecursiveMixed(List(a)))
    EndMixed(Callback, Net[Connect])

type Step:
    Empty
    Item(a, Iter(a))

type Iter:
    Iter(fn() -> Step(a))

fn direct(x: Dir[Read]) -> Nil:
    nil

fn callable(x: fn(Int) -> Int) -> Nil:
    nil

fn nested(x: List((Int, Callback))) -> Nil:
    nil

fn boxed(x: Boxed(fn(Int) -> Int)) -> Nil:
    nil

fn inferred(x: Inferred(fn(Int) -> Int)) -> Nil:
    nil

fn recursive(x: Node(fn(Int) -> Int)) -> Nil:
    nil

fn evolving(x: Evolve(Int)) -> Nil:
    nil

fn forwarded(x: Outer(fn(Int) -> Int)) -> Nil:
    nil

fn alias_forwarded(x: Forward(fn(Int) -> Int)) -> Nil:
    nil

fn growing(x: Grow(Int)) -> Nil:
    nil

fn recursive_phantom(x: RecursivePhantom(fn(Int) -> Int)) -> Nil:
    nil

fn phantom(x: Phantom(fn(Int) -> Int)) -> Nil:
    nil

fn plain(x: List((Int, String))) -> Nil:
    nil

fn mixed(x: Mixed) -> Nil:
    nil

fn mixed_alias(x: MixedAlias) -> Nil:
    nil

fn recursive_mixed(x: RecursiveMixed(Int)) -> Nil:
    nil

fn capability_callback(x: List(CapabilityCallback)) -> Nil:
    nil

fn enumerate(x: Iter((Int, a))) -> Nil:
    nil

fn enumerate_cap(x: Iter((File[Read], a))) -> Nil:
    nil

fn phantom_cap(x: Phantom(File[Read])) -> Nil:
    nil
"#,
        )
        .expect("parse storage shapes");
        let classifier = ReferenceStorageClassifier::new(&module);

        assert_eq!(
            classifier.first_reference(param(&module, "direct")),
            Some(ReferenceLeaf::ExternRef("Dir"))
        );
        for name in [
            "callable",
            "nested",
            "boxed",
            "inferred",
            "recursive",
            "evolving",
            "forwarded",
            "alias_forwarded",
            "growing",
        ] {
            assert_eq!(
                classifier.first_reference(param(&module, name)),
                Some(ReferenceLeaf::Function),
                "{name}"
            );
        }
        assert!(!classifier.requires_reference_storage(param(&module, "phantom")));
        assert!(!classifier.requires_reference_storage(param(&module, "recursive_phantom")));
        assert!(!classifier.requires_reference_storage(param(&module, "plain")));
        for (name, cap) in [
            ("mixed", "Dir"),
            ("mixed_alias", "File"),
            ("recursive_mixed", "Net"),
        ] {
            assert_eq!(
                classifier.first_reference(param(&module, name)),
                Some(ReferenceLeaf::Function),
                "the general query preserves source field order for {name}"
            );
            assert_eq!(classifier.first_externref(param(&module, name)), Some(cap), "{name}");
        }
        assert_eq!(classifier.first_externref(param(&module, "callable")), None);
        assert_eq!(classifier.first_externref(param(&module, "capability_callback")), None);
        assert_eq!(
            classifier.first_externref_including_function_signatures(
                param(&module, "capability_callback")
            ),
            Some("File")
        );
        assert_eq!(classifier.first_reference(param(&module, "enumerate")), Some(ReferenceLeaf::Function));
        assert_eq!(classifier.first_externref(param(&module, "enumerate")), None);
        assert_eq!(
            classifier.first_externref_including_function_signatures(param(&module, "enumerate")),
            None
        );
        assert_eq!(
            classifier.first_externref_including_function_signatures(
                param(&module, "enumerate_cap")
            ),
            Some("File")
        );
        assert_eq!(classifier.first_externref(param(&module, "phantom_cap")), None);
        assert_eq!(
            classifier.first_externref_including_function_signatures(
                param(&module, "phantom_cap")
            ),
            Some("File")
        );
    }
}
