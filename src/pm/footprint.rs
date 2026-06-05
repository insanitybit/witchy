//! Capability-footprint computation — the heart of coven's supply-chain story.
//!
//! Because authority in witchy is carried in types and can only enter at `main`
//! (a non-`main` rune can never *forge* a capability — it must receive one
//! through its public surface), the exact set of capability *kinds* a rune can
//! demand of its caller is computable, statically, from its source. This module
//! does that: it walks a rune's top-level functions and actors and collects the
//! capability kinds appearing in their parameter / field types, transitively
//! through user-defined types that contain capabilities.
//!
//! The result is a sound, tight upper bound on what the rune can do — there is
//! no hidden authority to miss. The footprint is split across two axes: the
//! `runtime` axis (Console/Dir/Net/Socket/Subject) and the `build` axis
//! (Build* capabilities a build step demands of the consuming project).

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;

/// Runtime capability type names (what a caller / `main` must supply).
const RUNTIME_CAPS: &[&str] = &["Console", "Dir", "Net", "Socket", "Subject"];
/// Build-time capability type names (what a build step demands of the consumer).
const BUILD_CAPS: &[&str] = &["BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec"];

fn is_cap_type(name: &str) -> bool {
    RUNTIME_CAPS.contains(&name) || BUILD_CAPS.contains(&name)
}

fn is_build_cap(name: &str) -> bool {
    BUILD_CAPS.contains(&name)
}

/// A rune's computed capability footprint, on two independent axes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Runtime capability kinds the rune demands of its caller / `main`.
    pub runtime: BTreeSet<String>,
    /// Build-time capability kinds the rune's build step demands of the consumer.
    pub build: BTreeSet<String>,
}

impl Footprint {
    pub fn is_empty(&self) -> bool {
        self.runtime.is_empty() && self.build.is_empty()
    }

    /// Determinism class (§7.2): a build that shells out (`BuildExec`) cannot be
    /// guaranteed reproducible, only pinned-and-verified. Everything else the
    /// host can make byte-reproducible.
    pub fn determinism(&self) -> &'static str {
        if self.build.contains("BuildExec") {
            "pinned-only"
        } else {
            "guaranteed"
        }
    }

    /// The capability kinds present in `self` but absent in `base`, per axis.
    /// A non-empty result means `self` *widens* the footprint (the gated event).
    pub fn widening_over(&self, base: &Footprint) -> Widening {
        Widening {
            runtime: self.runtime.difference(&base.runtime).cloned().collect(),
            build: self.build.difference(&base.build).cloned().collect(),
        }
    }
}

/// New capability kinds an upgrade would introduce, per axis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Widening {
    pub runtime: BTreeSet<String>,
    pub build: BTreeSet<String>,
}

impl Widening {
    pub fn is_empty(&self) -> bool {
        self.runtime.is_empty() && self.build.is_empty()
    }
}

/// Compute the capability footprint of a single rune (one parsed module).
///
/// Public surface = every top-level function (its parameters) and every actor
/// (its capability-typed fields, granted at spawn, plus its handler parameters).
/// witchy has no enforced visibility yet — the linker exposes every top-level
/// function as `mod.func` — so scanning all of them is the correct, sound choice.
pub fn compute(module: &Module) -> Footprint {
    let taint = TaintMap::build(module);
    let mut fp = Footprint::default();
    for item in &module.items {
        match item {
            // `main` is the root entrypoint — never called by an importer — so its
            // capability parameters are the root grant, not a demand on a caller.
            // Excluding it makes the footprint exactly "what this rune asks of
            // whoever uses it".
            Item::Function(f) if f.name == "main" => {}
            Item::Function(f) => {
                for p in &f.params {
                    taint.collect(p.ty.as_ref(), &mut fp);
                }
            }
            Item::Actor(a) => {
                for field in &a.fields {
                    taint.collect(Some(&field.ty), &mut fp);
                }
                for h in &a.handlers {
                    for p in &h.params {
                        taint.collect(p.ty.as_ref(), &mut fp);
                    }
                }
            }
            // Traits/impls are lowered to functions before this runs in the real
            // pipeline; type defs contribute only via taint, handled above.
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) => {}
        }
    }
    fp
}

/// Verify that a declared `[capabilities]` contract covers everything the
/// computed footprint actually demands. Under-declaration (the rune demands
/// authority it does not admit to) is the dangerous case. An empty declaration
/// is treated as "no contract" and always passes. Returns `Err(gap)` describing
/// the undeclared kinds.
pub fn check_declared(
    computed: &Footprint,
    declared_runtime: &[String],
    declared_build: &[String],
) -> Result<(), String> {
    if declared_runtime.is_empty() && declared_build.is_empty() {
        return Ok(());
    }
    let dr: BTreeSet<String> = declared_runtime.iter().cloned().collect();
    let db: BTreeSet<String> = declared_build.iter().cloned().collect();
    let ur: Vec<String> = computed.runtime.difference(&dr).cloned().collect();
    let ub: Vec<String> = computed.build.difference(&db).cloned().collect();
    if ur.is_empty() && ub.is_empty() {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !ur.is_empty() {
        parts.push(format!("runtime: {}", ur.join(", ")));
    }
    if !ub.is_empty() {
        parts.push(format!("build: {}", ub.join(", ")));
    }
    Err(parts.join("; "))
}

/// Parse a rune's `(module-name, source)` pairs and compute its footprint. Used
/// by the registry (server-side recomputation) and the resolver — the footprint
/// is always recomputed from source, never trusted from metadata.
pub fn of_modules(modules: &[(String, String)]) -> super::PmResult<Footprint> {
    let mut fp = Footprint::default();
    for (name, src) in modules {
        let m = crate::parser::parse_module(src)
            .map_err(|e| super::PmError(format!("{name}: {e}")))?;
        let sub = compute(&m);
        fp.runtime.extend(sub.runtime);
        fp.build.extend(sub.build);
    }
    Ok(fp)
}

/// Per-type-name set of capability kinds reachable by construction. A record/ADT
/// that holds a `Net` (directly or via another tainted type) taints to `{Net}`;
/// constructing it requires the caller to supply that capability.
struct TaintMap {
    map: BTreeMap<String, BTreeSet<String>>,
}

impl TaintMap {
    fn build(module: &Module) -> Self {
        let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for item in &module.items {
            if let Item::Type(t) = item {
                map.entry(t.name.clone()).or_default();
            }
        }
        // Fixpoint: a type is tainted by the caps of its variants' field types,
        // which may themselves be tainted user types. Iterate until stable.
        loop {
            let mut changed = false;
            for item in &module.items {
                let Item::Type(t) = item else { continue };
                let mut acc = BTreeSet::new();
                for v in &t.variants {
                    for fty in &v.fields {
                        Self::kinds_of(fty, &map, &mut acc);
                    }
                }
                let slot = map.entry(t.name.clone()).or_default();
                let before = slot.len();
                slot.extend(acc);
                if slot.len() != before {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        TaintMap { map }
    }

    /// Add the capability kinds reachable from `ty` into the footprint, routed to
    /// the correct axis.
    fn collect(&self, ty: Option<&Type>, fp: &mut Footprint) {
        let Some(ty) = ty else { return };
        let mut kinds = BTreeSet::new();
        Self::kinds_of(ty, &self.map, &mut kinds);
        for k in kinds {
            if is_build_cap(&k) {
                fp.build.insert(k);
            } else {
                fp.runtime.insert(k);
            }
        }
    }

    /// The capability kinds reachable from a type, using `map` for user types.
    fn kinds_of(ty: &Type, map: &BTreeMap<String, BTreeSet<String>>, out: &mut BTreeSet<String>) {
        match ty {
            Type::Named(name, args) => {
                if is_cap_type(name) {
                    out.insert(name.clone());
                }
                if let Some(t) = map.get(name) {
                    out.extend(t.iter().cloned());
                }
                for a in args {
                    Self::kinds_of(a, map, out);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    Self::kinds_of(t, map, out);
                }
            }
            // A function-typed parameter the rune will *invoke*: if it consumes a
            // capability, the rune must hold one to call it. Sound to include.
            Type::Fn(params, ret) => {
                for p in params {
                    Self::kinds_of(p, map, out);
                }
                Self::kinds_of(ret, map, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    fn fp(src: &str) -> Footprint {
        compute(&parse_module(src).expect("parse"))
    }

    #[test]
    fn pure_rune_demands_nothing() {
        let f = fp("fn add(a: Int, b: Int) -> Int { a + b }");
        assert!(f.is_empty());
        assert_eq!(f.determinism(), "guaranteed");
    }

    #[test]
    fn net_param_is_detected() {
        let f = fp("fn fetch(net: Net, url: String) -> String { url }");
        assert!(f.runtime.contains("Net"));
        assert_eq!(f.runtime.len(), 1);
        assert!(f.build.is_empty());
    }

    #[test]
    fn multiple_caps_across_functions() {
        let f = fp(r#"
            fn log(console: Console, msg: String) { print(msg) }
            fn save(dir: Dir, name: String) -> String { name }
        "#);
        assert!(f.runtime.contains("Console"));
        assert!(f.runtime.contains("Dir"));
    }

    #[test]
    fn taint_through_user_record() {
        // A record holding a Net taints; a function taking that record demands Net.
        let f = fp(r#"
            type Client { conn: Net, name: String }
            fn use_client(c: Client) -> String { c.name }
        "#);
        assert!(f.runtime.contains("Net"), "taint should propagate through Client");
    }

    #[test]
    fn taint_is_transitive() {
        let f = fp(r#"
            type Inner { n: Net }
            type Outer { inner: Inner, tag: String }
            fn f(o: Outer) -> String { o.tag }
        "#);
        assert!(f.runtime.contains("Net"), "taint must reach through two levels");
    }

    #[test]
    fn actor_capability_field_counts() {
        let f = fp(r#"
            actor Logger {
              console: Console
              var count: Int = 0
              on log(msg: String) { print(msg) }
            }
        "#);
        assert!(f.runtime.contains("Console"));
    }

    #[test]
    fn build_cap_routes_to_build_axis() {
        let f = fp(r#"
            fn build(out: BuildOut, exec: BuildExec) { print("gen") }
        "#);
        assert!(f.build.contains("BuildOut"));
        assert!(f.build.contains("BuildExec"));
        assert!(f.runtime.is_empty());
        assert_eq!(f.determinism(), "pinned-only");
    }

    #[test]
    fn build_without_exec_is_guaranteed() {
        let f = fp("fn build(out: BuildOut, read: BuildRead) { print(\"x\") }");
        assert_eq!(f.determinism(), "guaranteed");
    }

    #[test]
    fn main_is_excluded_from_footprint() {
        // main's capability params are the root grant, not a demand on a caller.
        let f = fp("fn main(console: Console) { print(console, \"hi\") }");
        assert!(f.is_empty(), "main must not contribute to the footprint");
        // But a non-main function's caps still count.
        let f2 = fp("fn main(c: Console) { print(c, \"x\") }\nfn helper(net: Net) -> Int { 0 }");
        assert!(f2.runtime.contains("Net"));
        assert!(!f2.runtime.contains("Console"), "Console came only via main");
    }

    #[test]
    fn check_declared_catches_underdeclaration() {
        let f = fp("fn fetch(net: Net, u: String) -> String { u }");
        assert!(check_declared(&f, &[], &[]).is_ok(), "no contract = ok");
        assert!(
            check_declared(&f, &["Console".into()], &[]).is_err(),
            "declares Console but demands Net"
        );
        assert!(check_declared(&f, &["Net".into()], &[]).is_ok(), "declares Net, demands Net");
    }

    #[test]
    fn widening_detects_new_kind() {
        let old = fp("fn log(c: Console) { print(\"x\") }");
        let new = fp(r#"
            fn log(c: Console) { print("x") }
            fn beacon(n: Net) { print("x") }
        "#);
        let w = new.widening_over(&old);
        assert!(w.runtime.contains("Net"));
        assert!(!w.is_empty());
        // Narrowing (old over new) is free.
        assert!(old.widening_over(&new).is_empty());
    }
}
