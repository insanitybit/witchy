//! (RFC-0042) Module-scoped types: canonicalize every user TYPE and CONSTRUCTOR
//! name to `module.Name`, per module, before the linker merges the namespaces.
//!
//! Functions were already module-scoped (`mod.func`); types were not — every
//! `type` in the link set landed in ONE flat global namespace, so two modules
//! declaring `Step` (iter + chan) silently merged their variant sets and became
//! un-coimportable. This pass makes types symmetric with functions: a module's
//! own types become `home.Type` (constructors `home.Ctor`), and every reference
//! is rewritten to its canonical qualified name using the module's own
//! declarations, its `from X import Y` bindings, and the ambient prelude.
//!
//! Resolution is per module, so it knows the home module and its imports — the
//! same place `aliases::resolve` runs. Two moments, as RFC-0042 §6 describes:
//! this pass (types, annotations, constructor expressions, and in-scope
//! constructor patterns), then a post-merge sweep ([`resolve_residual_patterns`])
//! that fixes bare constructor patterns whose type only becomes unambiguous once
//! the whole program is linked (a plain `import iter` + `match … Item(x)`).
//!
//! Ambient names stay bare: the primitives, the host capabilities, and the
//! prelude types `Option`/`Result` (and their `Some`/`None`/`Ok`/`Err`) — these
//! are load-bearing language surface, not ordinary library types.

// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet};

use crate::ast::*;
use crate::linker::LinkError;

fn lerr<T>(message: impl Into<String>) -> Result<T, LinkError> {
    Err(LinkError { message: message.into() })
}

/// Type names known without a module qualifier: primitives, host capabilities,
/// the built-in generics, and the prelude types `Option`/`Result`. MUST mirror
/// `witchy_types::typeck::BUILTIN_TYPE_NAMES` (the checker's counterpart). A type
/// declared with one of these names is left bare (today's flat behavior); every
/// other user type is module-qualified.
const AMBIENT_TYPES: &[&str] = &[
    "Int", "Float", "Duration", "String", "Bytes", "__Msg", "Bool", "Nil", "Console", "Clock",
    "Rand", "Env", "Secret", "SecretStore", "Dir", "File", "Net", "Exec", "Socket", "Listener",
    "List", "Option", "Result", "Dict", "BuildOut", "BuildRead", "BuildEnv", "BuildNet",
    "BuildExec",
    // `cmp.Ordering` is the comparison hierarchy's result type: derive- and
    // operator-generated code references it (and its variants) pervasively, and
    // `cmp` is its sole declarer, so it collides with nothing. Keeping it ambient
    // (like `Option`/`Result`) means every `derive(Ord)` / `match o: Less ->`
    // keeps working bare — no forced `import cmp`.
    "Ordering",
    // `policy.NetPolicy`/`policy.DirPolicy` (RFC-0011): the cap-refinement verbs
    // `only`/`deny` are BUILT-IN and type their policy argument by the bare name
    // `NetPolicy`/`DirPolicy` (typeck.rs), and a program may declare its own
    // policy record for them — so these names, like `Ordering`, stay ambient.
    "NetPolicy", "DirPolicy",
    // `set.Set` / `iter.Iter`: the method-dispatch and for-loop machinery still
    // treats these as ambient owner types (`type_owner_module`, the `for x in set`
    // -> `set.to_list` view, the `Set`-member `Eq` check). Each has a single
    // declarer, so keeping it ambient collides with nothing and keeps `xs.map()`
    // / `for x in aSet` / `collect` working without an import.
    "Set", "Iter",
];

/// Constructors that stay bare everywhere — the prelude `Option`/`Result` ones,
/// plus `cmp.Ordering`'s `Less`/`Equal`/`Greater` (see `AMBIENT_TYPES`). They are
/// ambient language surface (`?`, main-signature checking, comparison), so a bare
/// one never module-qualifies and never needs an import.
const AMBIENT_CTORS: &[&str] =
    &["Some", "None", "Ok", "Err", "Less", "Equal", "Greater", "NetPolicy", "DirPolicy"];

fn is_ambient_type(name: &str) -> bool {
    AMBIENT_TYPES.contains(&name)
}

fn is_ambient_ctor(name: &str) -> bool {
    AMBIENT_CTORS.contains(&name)
}

/// Compiler-synthesized type heads that name no module and stay bare: the
/// `TupleN` head a tuple `impl` dispatches under, and the `__anonN` record a
/// `.{…}` literal desugars to.
fn is_synthetic_type(name: &str) -> bool {
    is_anon_type(name)
        || (name.strip_prefix("Tuple").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

fn is_anon_type(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The types (and their constructors) one module declares, in their BARE spelling
/// — the raw material for building each importer's resolution maps.
#[derive(Default)]
struct ModTypes {
    /// Bare names of the (non-ambient) types this module declares.
    types: HashSet<String>,
    /// Bare constructor name -> bare owning type name.
    ctors: HashMap<String, String>,
    /// Bare names of the SEALED (`capability`) types this module declares — an
    /// RFC-0002 brand may be constructed/destructured only in its home module, so
    /// a bare reference to one from ELSEWHERE is canonicalized (not rejected as
    /// "qualify it") so the linker's sealing check reports the precise error.
    sealed: HashSet<String>,
}

/// Program-wide facts gathered before any rewrite: each module's declared types
/// and its exported function names (to validate `from X import <function>`).
struct World {
    types: HashMap<String, ModTypes>,
    /// All functions declared by a module. Used for same-module collision checks.
    fns: HashMap<String, HashSet<String>>,
    /// Public functions exported by a module. Used for cross-module imports.
    pub_fns: HashMap<String, HashSet<String>>,
    /// module -> the ambient-named types it declares (`cmp.Ordering`, `set.Set`,
    /// `iter.Iter`, `option.Option`). Kept OUT of `types` — they stay bare (a bare
    /// `Ordering` is ambient) — but recorded so the qualified spelling `cmp.Ordering`
    /// resolves to the same bare ambient name instead of a false "no such type".
    ambient: HashMap<String, HashSet<String>>,
}

impl World {
    fn build(modules: &[(String, Module)]) -> World {
        let mut types: HashMap<String, ModTypes> = HashMap::new();
        let mut fns: HashMap<String, HashSet<String>> = HashMap::new();
        let mut pub_fns: HashMap<String, HashSet<String>> = HashMap::new();
        let mut ambient: HashMap<String, HashSet<String>> = HashMap::new();
        for (name, m) in modules {
            let mt = types.entry(name.clone()).or_default();
            let fset = fns.entry(name.clone()).or_default();
            let pub_fset = pub_fns.entry(name.clone()).or_default();
            for item in &m.items {
                match item {
                    Item::Type(t) if is_synthetic_type(&t.name) => {}
                    Item::Type(t) if is_ambient_type(&t.name) => {
                        ambient.entry(name.clone()).or_default().insert(t.name.clone());
                    }
                    Item::Type(t) => {
                        mt.types.insert(t.name.clone());
                        if t.sealed {
                            mt.sealed.insert(t.name.clone());
                        }
                        for v in &t.variants {
                            mt.ctors.insert(v.name.clone(), t.name.clone());
                        }
                    }
                    Item::Function(f) => {
                        fset.insert(f.name.clone());
                        if f.public {
                            pub_fset.insert(f.name.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        World { types, fns, pub_fns, ambient }
    }

    /// Whether `module` declares a type spelled (bare) `ty`.
    fn module_has_type(&self, module: &str, ty: &str) -> bool {
        self.types.get(module).is_some_and(|mt| mt.types.contains(ty))
    }

    fn module_has_public_fn(&self, module: &str, name: &str) -> bool {
        self.pub_fns.get(module).is_some_and(|fns| fns.contains(name))
    }

    /// Whether `module` declares the ambient-named type `ty` (e.g. `cmp`/`Ordering`).
    fn module_declares_ambient(&self, module: &str, ty: &str) -> bool {
        self.ambient.get(module).is_some_and(|s| s.contains(ty))
    }

    /// Modules that declare a TYPE named (bare) `name`, sorted — for the
    /// unresolved-type hint (BUG-328).
    fn type_exporters(&self, name: &str) -> Vec<String> {
        let mut v: Vec<String> =
            self.types.iter().filter(|(_, mt)| mt.types.contains(name)).map(|(m, _)| m.clone()).collect();
        v.sort();
        v
    }

    /// Modules that declare a CONSTRUCTOR named (bare) `name`, each paired with its
    /// owning type's bare name, sorted — for the unresolved-constructor hint (BUG-328).
    fn ctor_exporters(&self, name: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .types
            .iter()
            .filter_map(|(m, mt)| mt.ctors.get(name).map(|owner| (m.clone(), owner.clone())))
            .collect();
        v.sort();
        v
    }

    /// Modules that declare a SEALED constructor named (bare) `name`, each paired
    /// with its owning type — the subset of [`ctor_exporters`] whose owner is a
    /// `capability` brand (RFC-0002). Used to canonicalize a bare out-of-module
    /// reference so the sealing check, not the "qualify it" hint, reports it.
    fn sealed_ctor_exporters(&self, name: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .types
            .iter()
            .filter_map(|(m, mt)| {
                mt.ctors
                    .get(name)
                    .filter(|owner| mt.sealed.contains(owner.as_str()))
                    .map(|owner| (m.clone(), owner.clone()))
            })
            .collect();
        v.sort();
        v
    }
}

/// The per-module resolution maps: what each bare name canonicalizes to.
struct Scope<'a> {
    home: &'a str,
    imports: &'a [String],
    world: &'a World,
    /// bare type name -> canonical (`home.T` or `srcmod.T`).
    type_map: HashMap<String, String>,
    /// bare constructor name -> canonical (`home.C` or `srcmod.C`).
    ctor_map: HashMap<String, String>,
}

/// Canonicalize every type and constructor reference in `modules`, in place.
pub fn resolve(modules: &mut [(String, Module)]) -> Result<(), LinkError> {
    let world = World::build(modules);
    // Split the borrow: build each scope from `world`, then rewrite that module.
    #[allow(clippy::needless_range_loop)] // index needed: read modules[idx] then mutate modules[idx].1
    for idx in 0..modules.len() {
        let (home, imports, from_imports) = {
            let (n, m) = &modules[idx];
            (n.clone(), m.imports.clone(), m.from_imports.clone())
        };
        let scope = Scope::build(&home, &imports, &from_imports, &world)?;
        scope.rewrite_module(&mut modules[idx].1)?;
    }
    Ok(())
}

impl<'a> Scope<'a> {
    fn build(
        home: &'a str,
        imports: &'a [String],
        from_imports: &'a [(String, Vec<String>)],
        world: &'a World,
    ) -> Result<Scope<'a>, LinkError> {
        let mut type_map: HashMap<String, String> = HashMap::new();
        let mut ctor_map: HashMap<String, String> = HashMap::new();
        // The module's own declarations.
        if let Some(mt) = world.types.get(home) {
            for t in &mt.types {
                type_map.insert(t.clone(), format!("{home}.{t}"));
            }
            for c in mt.ctors.keys() {
                ctor_map.insert(c.clone(), format!("{home}.{c}"));
            }
        }
        // `from X import Y, Z` — each listed name is bound unqualified. Two
        // unqualified bindings of one name (a second from-import, or a from-import
        // shadowing a local type) are a loud error at the second import site.
        let mut unqual: HashMap<String, String> = HashMap::new();
        if let Some(mt) = world.types.get(home) {
            for t in &mt.types {
                unqual.insert(t.clone(), format!("a local type `{t}`"));
            }
        }
        // A local `fn` is an unqualified binding too, so a `from X import <fn>`
        // that names it is the same §3 collision as two from-imports or a
        // from-imported type vs a local type — not a silent retarget of every
        // call site to the local (BUG-287).
        if let Some(fset) = world.fns.get(home) {
            for f in fset {
                unqual.entry(f.clone()).or_insert_with(|| format!("a local function `{f}`"));
            }
        }
        for (srcmod, names) in from_imports {
            for name in names {
                // An ambient name (`Ordering`, `Set`, `Iter`, `Less`, …) is in
                // scope everywhere without an import — so a `from cmp import
                // Ordering` is a collision, not a missing export. Check this FIRST:
                // ambient types are kept out of the module type map, so the
                // export check below would otherwise fire the false "exports no
                // type or function" message (BUG-291).
                if is_ambient_type(name) || is_ambient_ctor(name) {
                    return lerr(format!(
                        "`from {srcmod} import {name}` collides with the ambient prelude name \
                         `{name}` — it is already in scope everywhere; drop the import"
                    ));
                }
                let brought_type = world.module_has_type(srcmod, name);
                let brought_fn = world.module_has_public_fn(srcmod, name);
                if !brought_type && !brought_fn {
                    return lerr(format!(
                        "`from {srcmod} import {name}`: module `{srcmod}` exports no type or \
                         function named `{name}`"
                    ));
                }
                if let Some(prev) = unqual.get(name.as_str()) {
                    return lerr(format!(
                        "`from {srcmod} import {name}` collides with {prev} — both bind `{name}` \
                         unqualified. Drop one and use the qualified name (`{srcmod}.{name}`), or \
                         import under different names"
                    ));
                }
                unqual.insert(name.clone(), format!("`from {srcmod} import {name}`"));
                if brought_type {
                    type_map.insert(name.clone(), format!("{srcmod}.{name}"));
                    // A from-imported type brings its constructors' bare names too.
                    if let Some(mt) = world.types.get(srcmod) {
                        for (c, owner) in &mt.ctors {
                            if owner == name {
                                ctor_map.insert(c.clone(), format!("{srcmod}.{c}"));
                            }
                        }
                    }
                }
                // A from-imported FUNCTION needs no type/ctor entry; the linker
                // owns direct-call resolution for explicit `from X import f`
                // bindings.
            }
        }
        Ok(Scope { home, imports, world, type_map, ctor_map })
    }

    /// Whether `module` is referenceable here (imported, or a prelude module).
    fn in_scope(&self, module: &str) -> bool {
        self.imports.iter().any(|i| i == module)
            || matches!(module, "list" | "string" | "dict" | "math" | "option" | "result")
    }

    // ---- type references -------------------------------------------------

    /// Canonicalize a written type NAME (a bare `String`, e.g. an impl head).
    fn resolve_type_name(&self, name: &str) -> Result<String, LinkError> {
        if let Some((module, ty)) = name.split_once('.') {
            // A parser-qualified `mod.Type`: validate and keep it canonical.
            if !self.in_scope(module) {
                return lerr(format!(
                    "type `{name}`: module `{module}` is not imported (add `import {module}`)"
                ));
            }
            // An ambient type declared by this module (`cmp.Ordering`, `set.Set`,
            // `iter.Iter`, `option.Option`): the qualified spelling is sugar for
            // the bare ambient name (BUG-291) — they name one and the same type.
            if is_ambient_type(ty) && self.world.module_declares_ambient(module, ty) {
                return Ok(ty.to_string());
            }
            if !self.world.module_has_type(module, ty) {
                return lerr(format!("module `{module}` has no type `{ty}`"));
            }
            return Ok(name.to_string());
        }
        if is_ambient_type(name) || name == "Self" || is_synthetic_type(name) {
            // `Self` is the impl/trait's own type, substituted during trait
            // lowering; `TupleN` is the synthetic head a tuple impl dispatches
            // under; `__anonN` is a `.{…}` record — all name no module, stay bare.
            return Ok(name.to_string());
        }
        if name.chars().next().is_some_and(|c| c.is_lowercase()) {
            // A lowercase, module-less name is a generic type parameter.
            return Ok(name.to_string());
        }
        if let Some(canon) = self.type_map.get(name) {
            return Ok(canon.clone());
        }
        // Name the module(s) that actually export the type, matching the
        // unknown-function path's house style — never a literal `module.` (BUG-328).
        match self.world.type_exporters(name).as_slice() {
            [] => lerr(format!(
                "unknown type `{name}` (in module `{}`) — it is not declared here, not a \
                 built-in, and no in-scope module exports a type named `{name}`",
                self.home
            )),
            [m] => lerr(format!(
                "unknown type `{name}` (in module `{}`) — qualify it (`{m}.{name}`) or add \
                 `from {m} import {name}`",
                self.home
            )),
            many => {
                let list =
                    many.iter().map(|m| format!("`{m}.{name}`")).collect::<Vec<_>>().join(" or ");
                lerr(format!(
                    "unknown type `{name}` (in module `{}`) — it is exported by several modules; \
                     qualify it: {list}",
                    self.home
                ))
            }
        }
    }

    fn resolve_type(&self, ty: &mut Type) -> Result<(), LinkError> {
        match ty {
            Type::Qualified(_, inner) => self.resolve_type(inner),
            Type::Tuple(ts) => ts.iter_mut().try_for_each(|t| self.resolve_type(t)),
            Type::Fn(ps, r) => {
                ps.iter_mut().try_for_each(|p| self.resolve_type(p))?;
                self.resolve_type(r)
            }
            Type::Named(name, args) => {
                // `Dir[Read]` / `File[Write]` / `Net[Connect]` carry capability
                // RIGHTS in their arguments, not types — leave them untouched.
                if matches!(name.as_str(), "Dir" | "File" | "Net") {
                    return Ok(());
                }
                for a in args.iter_mut() {
                    self.resolve_type(a)?;
                }
                *name = self.resolve_type_name(name)?;
                Ok(())
            }
        }
    }

    // ---- expression constructors ----------------------------------------

    /// Canonicalize a constructor NAME in expression position (bare or qualified).
    /// A nullary "constructor" whose name is actually a TYPE (`Net.tcp(…)`,
    /// `Set.new()`) is a type-as-value static-method receiver: it resolves like a
    /// type name (bare if ambient, canonical otherwise), so static dispatch keys
    /// on the same name as the `impl` head.
    /// A bare constructor of a SEALED capability declared in a single in-scope
    /// module canonicalizes to `module.Ctor`, so the RFC-0002 sealing check
    /// (which keys on the canonical declaration name) reports the precise
    /// "sealed capability … cannot construct/destructure" error. You may not
    /// construct such a brand AT ALL outside its home module, so the ordinary
    /// "qualify it (`m.Ctor`) or from-import it" hint would be actively
    /// misleading — neither spelling is allowed (BUG-313 × RFC-0042).
    fn sealed_ctor_canonical(&self, name: &str) -> Option<String> {
        let mut hits = self
            .world
            .sealed_ctor_exporters(name)
            .into_iter()
            .filter(|(m, _)| self.in_scope(m));
        let (m, _) = hits.next()?;
        hits.next().is_none().then(|| format!("{m}.{name}"))
    }

    fn resolve_ctor_expr_name(&self, name: &str) -> Result<String, LinkError> {
        if let Some((module, ctor)) = name.split_once('.') {
            if !self.in_scope(module) {
                return lerr(format!(
                    "constructor `{name}`: module `{module}` is not imported"
                ));
            }
            let known = self.world.types.get(module).is_some_and(|mt| {
                mt.ctors.contains_key(ctor) || mt.types.contains(ctor)
            });
            if !known {
                return lerr(format!("module `{module}` has no constructor `{ctor}`"));
            }
            return Ok(name.to_string());
        }
        if is_ambient_ctor(name) || is_synthetic_type(name) {
            return Ok(name.to_string());
        }
        if let Some(canon) = self.ctor_map.get(name) {
            return Ok(canon.clone());
        }
        // Not a constructor — a bare TYPE used as a static-method receiver.
        if is_ambient_type(name) {
            return Ok(name.to_string());
        }
        if let Some(canon) = self.type_map.get(name) {
            return Ok(canon.clone());
        }
        // A sealed capability's brand: canonicalize so sealing reports it (below).
        if let Some(canon) = self.sealed_ctor_canonical(name) {
            return Ok(canon);
        }
        // Name the exporting module and the constructor's owning type, matching the
        // unknown-function path — never a literal `module.`/`<Type>` (BUG-328).
        match self.world.ctor_exporters(name).as_slice() {
            [] => lerr(format!(
                "unknown constructor `{name}` — no in-scope module exports a constructor named \
                 `{name}`"
            )),
            [(m, owner)] => lerr(format!(
                "unknown constructor `{name}` — qualify it (`{m}.{name}`) or add \
                 `from {m} import {owner}`"
            )),
            many => {
                let list =
                    many.iter().map(|(m, _)| format!("`{m}.{name}`")).collect::<Vec<_>>().join(" or ");
                lerr(format!(
                    "unknown constructor `{name}` — it is exported by several modules; qualify \
                     it: {list}"
                ))
            }
        }
    }

    /// Whether `module.Suffix` names a constructor of an in-scope module — used to
    /// turn a parser `Call`/`Field` (`iter.Item(1)` / `iter.Empty`) into a `Ctor`.
    fn is_qualified_ctor(&self, module: &str, suffix: &str) -> bool {
        suffix.chars().next().is_some_and(|c| c.is_uppercase())
            && self.in_scope(module)
            && self.world.types.get(module).is_some_and(|mt| mt.ctors.contains_key(suffix))
    }

    /// Whether `module.Suffix` names a constructor or a type in an in-scope module.
    /// A static method receiver (`json.Json.from(x)`) uses the same zero-arg `Ctor`
    /// representation as a bare type receiver (`Json.from(x)`), even when `Json`
    /// is a type name rather than a data constructor.
    fn is_qualified_type_receiver(&self, module: &str, suffix: &str) -> bool {
        suffix.chars().next().is_some_and(|c| c.is_uppercase())
            && self.in_scope(module)
            && self.world.types.get(module).is_some_and(|mt| {
                mt.ctors.contains_key(suffix) || mt.types.contains(suffix)
            })
    }

    // ---- patterns --------------------------------------------------------

    /// Canonicalize a constructor pattern name. A bare name that resolves in scope
    /// becomes canonical; one that does not is LEFT BARE for the post-merge sweep
    /// (or the checker) to resolve against the scrutinee's type (RFC-0042 §4).
    fn resolve_ctor_pat_name(&self, name: &str) -> Result<String, LinkError> {
        if let Some((module, ctor)) = name.split_once('.') {
            if !self.in_scope(module) {
                return lerr(format!("constructor `{name}`: module `{module}` is not imported"));
            }
            if !self.world.types.get(module).is_some_and(|mt| mt.ctors.contains_key(ctor)) {
                return lerr(format!("module `{module}` has no constructor `{ctor}`"));
            }
            return Ok(name.to_string());
        }
        if is_ambient_ctor(name) {
            return Ok(name.to_string());
        }
        if let Some(canon) = self.ctor_map.get(name) {
            return Ok(canon.clone());
        }
        // A sealed capability's brand destructured from another module: canonicalize
        // it (rather than leaving it bare) so the sealing check keys on the same
        // canonical name and reports "cannot destructure" (BUG-313 × RFC-0042).
        if let Some(canon) = self.sealed_ctor_canonical(name) {
            return Ok(canon);
        }
        Ok(name.to_string())
    }

    fn resolve_pattern(&self, p: &mut Pattern) -> Result<(), LinkError> {
        match p {
            Pattern::Ctor { name, args } => {
                *name = self.resolve_ctor_pat_name(name)?;
                for a in args {
                    self.resolve_pattern(a)?;
                }
            }
            Pattern::Tuple(ps) | Pattern::List { elems: ps, .. } | Pattern::Or(ps) => {
                for q in ps {
                    self.resolve_pattern(q)?;
                }
            }
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Duration(_)
            | Pattern::IntRange { .. } => {}
        }
        Ok(())
    }

    // ---- items -----------------------------------------------------------

    fn rewrite_module(&self, m: &mut Module) -> Result<(), LinkError> {
        for item in &mut m.items {
            match item {
                Item::Type(t) => {
                    // A user type may not redeclare an ambient name (a primitive,
                    // a host capability, or a prelude type) when doing so would
                    // strand its constructors: an ambient-named type is kept out of
                    // the module type map, so a NON-ambient constructor it declares
                    // (`type Secret: Hidden(String)`, `type Set: ...`) is
                    // unreachable by any spelling (BUG-289). A declaration whose
                    // constructors are ALL ambient (`type Result: Ok(a) Err(e)`,
                    // `type Option: Some(a) None`) resolves fine and stays legal —
                    // a program may restate the prelude type. The canonical std
                    // declarers (`cmp`/`set`/`iter`/`option`/`result`/`policy`) are
                    // exempt outright.
                    if is_ambient_type(&t.name)
                        && !is_synthetic_type(&t.name)
                        && !crate::linker::STD_MODULES.contains(&self.home)
                        && t.variants.iter().any(|v| !is_ambient_ctor(&v.name))
                    {
                        return lerr(format!(
                            "type `{name}` shadows the ambient built-in name `{name}` — rename it \
                             (a user type may not redeclare a primitive, capability, or prelude \
                             type with new constructors; they would be unreachable)",
                            name = t.name
                        ));
                    }
                    // Canonicalize the declaration itself (unless ambient or a
                    // compiler synthetic like `__anonN`, whose constructions stay
                    // bare — canonicalizing the def would strand them).
                    if !is_ambient_type(&t.name) && !is_synthetic_type(&t.name) {
                        let qual = format!("{}.{}", self.home, t.name);
                        for v in &mut t.variants {
                            v.name = format!("{}.{}", self.home, v.name);
                        }
                        t.name = qual;
                    }
                    for v in &mut t.variants {
                        for ft in &mut v.fields {
                            self.resolve_type(ft)?;
                        }
                    }
                }
                Item::Function(f) => self.rewrite_function(f)?,
                Item::Trait(tr) => {
                    for ms in &mut tr.methods {
                        self.rewrite_methodsig(ms)?;
                    }
                }
                Item::Impl(im) => {
                    im.type_name = self.resolve_type_name(&im.type_name)?;
                    for t in &mut im.target_args {
                        self.resolve_type(t)?;
                    }
                    for t in &mut im.trait_args {
                        self.resolve_type(t)?;
                    }
                    self.resolve_bounds(&mut im.bounds)?;
                    for method in &mut im.methods {
                        self.rewrite_function(method)?;
                    }
                }
                Item::Const { value, .. } => self.resolve_expr(value)?,
                Item::TypeAlias { ty, .. } => self.resolve_type(ty)?,
                // A `comptime:` block runs as its OWN pruned program (comptime
                // auto-imports `meta`), where it is linked — and so type-resolved —
                // in that program's scope, not this module's. Leftover blocks are
                // dropped at merge. Resolving them here would use the wrong imports.
                Item::Comptime(_) => {}
            }
        }
        Ok(())
    }

    fn rewrite_function(&self, f: &mut Function) -> Result<(), LinkError> {
        for p in &mut f.params {
            if let Some(t) = &mut p.ty {
                self.resolve_type(t)?;
            }
            if let Some(d) = &mut p.default {
                self.resolve_expr(d)?;
            }
        }
        if let Some(t) = &mut f.ret {
            self.resolve_type(t)?;
        }
        self.resolve_bounds(&mut f.bounds)?;
        self.resolve_block(&mut f.body)
    }

    fn rewrite_methodsig(&self, ms: &mut MethodSig) -> Result<(), LinkError> {
        for p in &mut ms.params {
            if let Some(t) = &mut p.ty {
                self.resolve_type(t)?;
            }
        }
        if let Some(t) = &mut ms.ret {
            self.resolve_type(t)?;
        }
        if let Some(b) = &mut ms.default {
            self.resolve_block(b)?;
        }
        Ok(())
    }

    /// A `where`-clause's trait type-arguments are written-type positions.
    fn resolve_bounds(&self, bounds: &mut [(String, String, Vec<Type>)]) -> Result<(), LinkError> {
        for (_, _, trait_args) in bounds.iter_mut() {
            for t in trait_args {
                self.resolve_type(t)?;
            }
        }
        Ok(())
    }

    fn resolve_block(&self, b: &mut Block) -> Result<(), LinkError> {
        if let Some(region) = &mut b.region {
            if let Some(t) = &mut region.ty {
                self.resolve_type(t)?;
            }
        }
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { ty, value, .. } => {
                    if let Some(t) = ty {
                        self.resolve_type(t)?;
                    }
                    self.resolve_expr(value)?;
                }
                Stmt::LetPattern { pattern, value } => {
                    self.resolve_pattern(pattern)?;
                    self.resolve_expr(value)?;
                }
                Stmt::Assign { value, .. } | Stmt::Yield(value) | Stmt::Expr(value) => {
                    self.resolve_expr(value)?;
                }
                Stmt::Return(Some(e)) => self.resolve_expr(e)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }

    fn resolve_expr(&self, e: &mut Expr) -> Result<(), LinkError> {
        match e {
            Expr::Ctor { name, args } => {
                *name = self.resolve_ctor_expr_name(name)?;
                for a in args {
                    self.resolve_expr(a)?;
                }
            }
            // `iter.Item(1)` parsed as a qualified Call; if the suffix is a
            // constructor of that module, it is really a constructor application.
            Expr::Call { name, args } => {
                if let Some((module, suffix)) = name.split_once('.') {
                    if self.is_qualified_ctor(module, suffix) {
                        let name = std::mem::take(name);
                        let mut new_args = Vec::new();
                        std::mem::swap(args, &mut new_args);
                        for a in &mut new_args {
                            self.resolve_expr(a)?;
                        }
                        *e = Expr::Ctor { name, args: new_args };
                        return Ok(());
                    }
                }
                for a in args {
                    self.resolve_expr(a)?;
                }
            }
            // `iter.Empty` (nullary) parsed as a field read on the module name.
            Expr::Field { base, field } => {
                if let Expr::Var(module) = base.as_ref() {
                    if self.is_qualified_ctor(module, field) {
                        *e = Expr::Ctor { name: format!("{module}.{field}"), args: Vec::new() };
                        return Ok(());
                    }
                }
                self.resolve_expr(base)?;
            }
            Expr::As { expr, ty } => {
                self.resolve_expr(expr)?;
                self.resolve_type(ty)?;
            }
            Expr::Lambda { params, body, ret } => {
                for p in params.iter_mut() {
                    if let Some(t) = &mut p.ty {
                        self.resolve_type(t)?;
                    }
                }
                if let Some(t) = ret {
                    self.resolve_type(t)?;
                }
                self.resolve_block(body)?;
            }
            Expr::Record { name, fields, spread } => {
                // Named-field / spread construction (`Point(x: 1, ..p)`). Canonicalize
                // the ctor name when it resolves in scope (a local or from-imported
                // record), but LEAVE an unresolved bare name for the records-lowering
                // pass — which reports the precise "`X(...)` … `X` is not a record
                // type" (BUG-316), resolves an imported record once every module is
                // merged (BUG-342), and rejects a genuine typo. Emitting the bare-
                // ctor "qualify it" error here would both mask that clearer message
                // and pre-empt the merged pass that legitimately resolves an imported
                // record type this module cannot yet see.
                if let Ok(canon) = self.resolve_ctor_expr_name(name) {
                    *name = canon;
                }
                for (_, v) in fields {
                    self.resolve_expr(v)?;
                }
                if let Some(s) = spread {
                    self.resolve_expr(s)?;
                }
            }
            Expr::RecordUpdate { name, base, fields } => {
                if let Some(name) = name {
                    if let Ok(canon) = self.resolve_ctor_expr_name(name) {
                        *name = canon;
                    }
                }
                self.resolve_expr(base)?;
                for (_, v) in fields {
                    self.resolve_expr(v)?;
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, a) in args {
                    self.resolve_expr(a)?;
                }
            }
            Expr::Apply { func, args } => {
                self.resolve_expr(func)?;
                for a in args {
                    self.resolve_expr(a)?;
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                let qualified_type_receiver = match receiver.as_ref() {
                    Expr::Field { base, field } => match base.as_ref() {
                        Expr::Var(module) if self.is_qualified_type_receiver(module, field) => {
                            Some((module.clone(), field.clone()))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((module, field)) = qualified_type_receiver {
                    **receiver = Expr::Ctor {
                        name: format!("{module}.{field}"),
                        args: Vec::new(),
                    };
                }
                self.resolve_expr(receiver)?;
                for a in args {
                    self.resolve_expr(a)?;
                }
            }
            Expr::List(xs) | Expr::Tuple(xs) => {
                for x in xs {
                    self.resolve_expr(x)?;
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) => self.resolve_expr(expr)?,
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs)?;
                self.resolve_expr(rhs)?;
            }
            Expr::Range { lo, hi, .. } => {
                self.resolve_expr(lo)?;
                self.resolve_expr(hi)?;
            }
            Expr::Index { base, index } => {
                self.resolve_expr(base)?;
                self.resolve_expr(index)?;
            }
            Expr::If { cond, then_block, else_block } => {
                self.resolve_expr(cond)?;
                self.resolve_block(then_block)?;
                if let Some(b) = else_block {
                    self.resolve_block(b)?;
                }
            }
            Expr::While { cond, body } => {
                self.resolve_expr(cond)?;
                self.resolve_block(body)?;
            }
            Expr::For { iter, body, .. } => {
                self.resolve_expr(iter)?;
                self.resolve_block(body)?;
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.resolve_pattern(pattern)?;
                self.resolve_expr(scrutinee)?;
                self.resolve_block(body)?;
            }
            Expr::Match { scrutinee, arms } => {
                self.resolve_expr(scrutinee)?;
                for arm in arms.iter_mut() {
                    self.resolve_pattern(&mut arm.pattern)?;
                    if let Some(g) = &mut arm.guard {
                        self.resolve_expr(g)?;
                    }
                    self.resolve_expr(&mut arm.body)?;
                }
            }
            Expr::Block(b) => self.resolve_block(b)?,
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Post-merge sweep: bare constructor PATTERNS whose owning type only becomes
// unambiguous after the whole program is linked (a plain `import iter` + a
// `match … Item(x)`). By construction of the per-module pass, every remaining
// bare `Pattern::Ctor` here is a cross-module reference under a plain import.
// Resolve it by global uniqueness of the constructor's unqualified suffix; if
// the suffix is ambiguous, that is a loud error asking for a qualifier (the
// RFC-0042 §4 fallback), never a silent runtime tag mismatch.
// ---------------------------------------------------------------------------

/// Rewrite residual bare constructor patterns in the merged `module` to their
/// canonical `module.Ctor` names, using the full set of declared constructors.
pub fn resolve_residual_patterns(module: &mut Module) -> Result<(), LinkError> {
    // Every canonical constructor name in the program, indexed by unqualified
    // suffix -> the canonical names sharing it.
    let mut by_suffix: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            for v in &t.variants {
                let suffix = v.name.rsplit('.').next().unwrap_or(&v.name).to_string();
                by_suffix.entry(suffix).or_default().push(v.name.clone());
            }
        }
    }
    for item in &mut module.items {
        match item {
            Item::Function(f) => resolve_residual_block(&mut f.body, &by_suffix)?,
            Item::Impl(im) => {
                for method in &mut im.methods {
                    resolve_residual_block(&mut method.body, &by_suffix)?;
                }
            }
            Item::Trait(tr) => {
                for ms in &mut tr.methods {
                    if let Some(b) = &mut ms.default {
                        resolve_residual_block(b, &by_suffix)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_residual_pattern(
    p: &mut Pattern,
    by_suffix: &HashMap<String, Vec<String>>,
) -> Result<(), LinkError> {
    match p {
        Pattern::Ctor { name, args } => {
            if !name.contains('.') && !is_ambient_ctor(name) {
                match by_suffix.get(name.as_str()).map(|v| v.as_slice()) {
                    Some([only]) => *name = only.clone(),
                    Some(many) if many.len() > 1 => {
                        return lerr(format!(
                            "ambiguous constructor `{name}` in a pattern — it names {}. Qualify \
                             it (e.g. `{}`) or match through a typed scrutinee",
                            many.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(" or "),
                            many[0]
                        ));
                    }
                    // Unknown/zero: leave bare — the checker reports it (or it is a
                    // genuinely bare tag the runtime never produces).
                    _ => {}
                }
            }
            for a in args {
                resolve_residual_pattern(a, by_suffix)?;
            }
        }
        Pattern::Tuple(ps) | Pattern::List { elems: ps, .. } | Pattern::Or(ps) => {
            for q in ps {
                resolve_residual_pattern(q, by_suffix)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_residual_block(
    b: &mut Block,
    by_suffix: &HashMap<String, Vec<String>>,
) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::LetPattern { pattern, value } => {
                resolve_residual_pattern(pattern, by_suffix)?;
                resolve_residual_expr(value, by_suffix)?;
            }
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => resolve_residual_expr(value, by_suffix)?,
            Stmt::Return(Some(e)) => resolve_residual_expr(e, by_suffix)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    /// Run the per-module type/constructor resolution over a set of `(name, src)`
    /// modules — the linker's `type_resolve::resolve` step in isolation.
    fn resolve_src(mods: &[(&str, &str)]) -> Result<(), LinkError> {
        let mut modules: Vec<(String, Module)> = mods
            .iter()
            .map(|(n, s)| (n.to_string(), parse_module(s).expect("parse")))
            .collect();
        resolve(&mut modules)
    }

    #[test]
    fn from_import_fn_collides_with_local_fn() {
        // BUG-287: `from json import stringify` + a local `fn stringify` is the
        // §3 unqualified-binding collision, not a silent retarget to the local.
        let err = resolve_src(&[
            ("json", "pub fn stringify(j: Int) -> String:\n    \"j\"\n"),
            (
                "main",
                "from json import stringify\n\nfn stringify(j: Int) -> String:\n    \"local\"\n\nfn main(console: Console):\n    print(console, stringify(1))\n",
            ),
        ])
        .unwrap_err();
        assert!(
            err.message.contains("collides with a local function `stringify`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn from_import_fn_without_local_is_ok() {
        // Control: no local `stringify`, so the from-import is legal.
        resolve_src(&[
            ("json", "pub fn stringify(j: Int) -> String:\n    \"j\"\n"),
            (
                "main",
                "from json import stringify\n\nfn main(console: Console):\n    print(console, stringify(1))\n",
            ),
        ])
        .expect("no collision");
    }

    #[test]
    fn from_import_private_fn_is_not_an_export() {
        // Module privacy is part of the language surface: a private helper may be
        // called inside its module, but cannot be imported unqualified elsewhere.
        let err = resolve_src(&[
            ("lib", "fn hidden(n: Int) -> Int:\n    n + 1\n\npub fn shown(n: Int) -> Int:\n    hidden(n)\n"),
            ("main", "from lib import hidden\n\nfn main(console: Console):\n    print(console, \"x\")\n"),
        ])
        .unwrap_err();
        assert!(
            err.message.contains("module `lib` exports no type or function named `hidden`"),
            "{}",
            err.message
        );
    }

    const CMP_STUB: &str =
        "type Ordering:\n    Less\n    Equal\n    Greater\n\npub fn reverse(o: Ordering) -> Ordering:\n    o\n";

    #[test]
    fn qualified_ambient_type_resolves() {
        // BUG-291: `cmp.Ordering` in an annotation resolves to the bare ambient
        // `Ordering`, not a false "module `cmp` has no type `Ordering`".
        resolve_src(&[
            ("cmp", CMP_STUB),
            ("main", "import cmp\n\nfn f(o: cmp.Ordering) -> Int:\n    0\n"),
        ])
        .expect("cmp.Ordering resolves");
    }

    #[test]
    fn from_import_ambient_type_is_collision_not_missing_export() {
        // BUG-291: `from cmp import Ordering` is an ambient-name collision, not the
        // false "exports no type or function named `Ordering`".
        let err = resolve_src(&[
            ("cmp", CMP_STUB),
            ("main", "from cmp import Ordering\n\nfn main(console: Console):\n    print(console, \"x\")\n"),
        ])
        .unwrap_err();
        assert!(err.message.contains("ambient prelude name `Ordering`"), "{}", err.message);
    }

    #[test]
    fn user_type_shadowing_ambient_name_is_rejected() {
        // BUG-289: a user module declaring `type Secret`/`type Set`/… is a loud
        // error (its constructors would be unreachable), while the canonical std
        // declarer (`cmp`) may still declare `type Ordering`.
        for ambient in ["Secret", "Set", "Iter", "Ordering"] {
            let src = format!("type {ambient}:\n    Wrapped(Int)\n");
            let err = resolve_src(&[("main", &src)]).unwrap_err();
            assert!(
                err.message.contains(&format!("type `{ambient}` shadows the ambient built-in name")),
                "{ambient}: {}",
                err.message
            );
        }
        // A std module keeps declaring its own ambient type.
        resolve_src(&[("cmp", CMP_STUB)]).expect("cmp may declare Ordering");
        // A program may restate a prelude type whose constructors are all ambient
        // (`Ok`/`Err`, `Some`/`None`) — those resolve, so nothing is stranded.
        resolve_src(&[("main", "type Result:\n    Ok(a)\n    Err(e)\n")])
            .expect("type Result with ambient ctors is legal");
        resolve_src(&[("main", "type Option:\n    Some(a)\n    None\n")])
            .expect("type Option with ambient ctors is legal");
    }

    #[test]
    fn unknown_ctor_hint_names_the_exporting_module() {
        // BUG-328: the hint names the real module and owning type — no literal
        // `module.`/`<Type>` placeholder text.
        let err = resolve_src(&[
            ("json", "type Json:\n    JsonInt(Int)\n    JsonNull\n"),
            ("main", "import json\n\nfn main(console: Console):\n    let x = JsonInt(5)\n    print(console, \"x\")\n"),
        ])
        .unwrap_err();
        assert!(
            err.message.contains("`json.JsonInt`") && err.message.contains("from json import Json"),
            "{}",
            err.message
        );
        assert!(!err.message.contains("module."), "leaked placeholder: {}", err.message);
    }

    #[test]
    fn unknown_type_hint_names_the_exporting_module() {
        // BUG-328: same house style for an unresolved type reference.
        let err = resolve_src(&[
            ("json", "type Json:\n    JsonInt(Int)\n"),
            ("main", "import json\n\nfn f(x: Json) -> Int:\n    0\n"),
        ])
        .unwrap_err();
        assert!(
            err.message.contains("`json.Json`") && err.message.contains("from json import Json"),
            "{}",
            err.message
        );
        assert!(!err.message.contains("(`module."), "leaked placeholder: {}", err.message);
    }
}

fn resolve_residual_expr(
    e: &mut Expr,
    by_suffix: &HashMap<String, Vec<String>>,
) -> Result<(), LinkError> {
    match e {
        Expr::Match { scrutinee, arms } => {
            resolve_residual_expr(scrutinee, by_suffix)?;
            for arm in arms.iter_mut() {
                resolve_residual_pattern(&mut arm.pattern, by_suffix)?;
                if let Some(g) = &mut arm.guard {
                    resolve_residual_expr(g, by_suffix)?;
                }
                resolve_residual_expr(&mut arm.body, by_suffix)?;
            }
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            resolve_residual_pattern(pattern, by_suffix)?;
            resolve_residual_expr(scrutinee, by_suffix)?;
            resolve_residual_block(body, by_suffix)?;
        }
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                resolve_residual_expr(a, by_suffix)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                resolve_residual_expr(a, by_suffix)?;
            }
        }
        Expr::Apply { func, args } => {
            resolve_residual_expr(func, by_suffix)?;
            for a in args {
                resolve_residual_expr(a, by_suffix)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            resolve_residual_expr(receiver, by_suffix)?;
            for a in args {
                resolve_residual_expr(a, by_suffix)?;
            }
        }
        Expr::RecordUpdate { name, base, fields } => {
            if let Some(name) = name {
                if !name.contains('.') && !is_ambient_ctor(name) {
                    if let Some([only]) = by_suffix.get(name.as_str()).map(|v| v.as_slice()) {
                        *name = only.clone();
                    }
                }
            }
            resolve_residual_expr(base, by_suffix)?;
            for (_, v) in fields {
                resolve_residual_expr(v, by_suffix)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                resolve_residual_expr(v, by_suffix)?;
            }
            if let Some(s) = spread {
                resolve_residual_expr(s, by_suffix)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => resolve_residual_expr(expr, by_suffix)?,
        Expr::Binary { lhs, rhs, .. } => {
            resolve_residual_expr(lhs, by_suffix)?;
            resolve_residual_expr(rhs, by_suffix)?;
        }
        Expr::Range { lo, hi, .. } => {
            resolve_residual_expr(lo, by_suffix)?;
            resolve_residual_expr(hi, by_suffix)?;
        }
        Expr::Index { base, index } => {
            resolve_residual_expr(base, by_suffix)?;
            resolve_residual_expr(index, by_suffix)?;
        }
        Expr::If { cond, then_block, else_block } => {
            resolve_residual_expr(cond, by_suffix)?;
            resolve_residual_block(then_block, by_suffix)?;
            if let Some(b) = else_block {
                resolve_residual_block(b, by_suffix)?;
            }
        }
        Expr::While { cond, body } => {
            resolve_residual_expr(cond, by_suffix)?;
            resolve_residual_block(body, by_suffix)?;
        }
        Expr::For { iter, body, .. } => {
            resolve_residual_expr(iter, by_suffix)?;
            resolve_residual_block(body, by_suffix)?;
        }
        Expr::Lambda { body, .. } => resolve_residual_block(body, by_suffix)?,
        Expr::Block(b) => resolve_residual_block(b, by_suffix)?,
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}
