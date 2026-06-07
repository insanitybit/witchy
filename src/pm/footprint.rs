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
const RUNTIME_CAPS: &[&str] = &["Console", "Clock", "Dir", "Net", "Socket", "Subject"];
/// Build-time capability type names (what a build step demands of the consumer).
const BUILD_CAPS: &[&str] = &["BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec"];

fn is_cap_type(name: &str) -> bool {
    RUNTIME_CAPS.contains(&name) || BUILD_CAPS.contains(&name)
}

fn is_build_cap(name: &str) -> bool {
    BUILD_CAPS.contains(&name)
}

// --- capability rights (verb-precision) ---
//
// A footprint string is either a bare capability (`Net`, full rights) or a
// bracketed subset (`Net[Connect]`, `Dir[Read]`). Storage stays a flat
// `BTreeSet<String>` (so manifests/lockfiles are unchanged and a legacy bare
// `Net` keeps meaning "full"); the rights-precision lives in the *comparison*
// primitives below, which parse the strings before diffing. This makes the gate
// verb-aware: a `Net[Connect]` that grows to also `Listen` is a widening, while a
// full `Net` tightened to `Net[Connect]` is a safe narrowing.

/// The full right-set a *bare* capability confers. Only `Dir`/`Net` have rights;
/// `Net` has two axes (verbs + transports), both full when bare.
fn full_rights(cap: &str) -> BTreeSet<String> {
    match cap {
        "Dir" => ["Read", "Write"].iter().map(|s| s.to_string()).collect(),
        "Net" => ["Connect", "Listen", "Tcp", "Udp", "Uds"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        _ => BTreeSet::new(),
    }
}

const NET_VERBS: [&str; 2] = ["Connect", "Listen"];
const NET_TRANSPORTS: [&str; 3] = ["Tcp", "Udp", "Uds"];

/// Map a bracket marker to its canonical right for a capability, or `None` if it
/// isn't a recognized right.
fn right_marker(cap: &str, marker: &str) -> Option<&'static str> {
    match (cap, marker) {
        ("Dir", "Read") => Some("Read"),
        ("Dir", "Write") => Some("Write"),
        ("Net", "Connect") => Some("Connect"),
        ("Net", "Listen") => Some("Listen"),
        ("Net", "Tcp") => Some("Tcp"),
        ("Net", "Udp") => Some("Udp"),
        ("Net", "Uds") => Some("Uds"),
        _ => None,
    }
}

/// `Net` has two independent axes; an axis with no marker mentioned defaults to
/// full (so `Net[Connect]` keeps all transports, matching the type system). This
/// expansion is what makes flat set-difference compute the correct per-axis
/// widening, so it must run on every set of `Net` markers before comparison.
fn default_net_axes(r: &mut BTreeSet<String>) {
    if !NET_VERBS.iter().any(|v| r.contains(*v)) {
        r.extend(NET_VERBS.iter().map(|s| s.to_string()));
    }
    if !NET_TRANSPORTS.iter().any(|t| r.contains(*t)) {
        r.extend(NET_TRANSPORTS.iter().map(|s| s.to_string()));
    }
}

/// The rights a capability annotation confers: the recognized bracket markers if
/// present, else (bare capability) the full set. `Net`'s axes default to full
/// independently.
fn rights_from_args(cap: &str, args: &[Type]) -> BTreeSet<String> {
    if args.is_empty() {
        return full_rights(cap);
    }
    let mut r = BTreeSet::new();
    for a in args {
        if let Type::Named(n, _) = a {
            if let Some(m) = right_marker(cap, n) {
                r.insert(m.to_string());
            }
        }
    }
    if cap == "Net" {
        default_net_axes(&mut r);
    }
    r
}

/// Render a capability + rights as a footprint string: a bare name when it
/// carries its full right-set (or none). `Net` is rendered axis-aware — an axis
/// at its full set is omitted, so `{Connect, Tcp, Udp, Uds}` prints `Net[Connect]`.
fn render_cap(name: &str, rights: &BTreeSet<String>) -> String {
    if rights.is_empty() || *rights == full_rights(name) {
        return name.to_string();
    }
    if name == "Net" {
        let mut parts: Vec<&str> = Vec::new();
        if !NET_VERBS.iter().all(|v| rights.contains(*v)) {
            parts.extend(NET_VERBS.iter().copied().filter(|v| rights.contains(*v)));
        }
        if !NET_TRANSPORTS.iter().all(|t| rights.contains(*t)) {
            parts.extend(NET_TRANSPORTS.iter().copied().filter(|t| rights.contains(*t)));
        }
        return format!("Net[{}]", parts.join(", "));
    }
    format!(
        "{name}[{}]",
        rights.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// Parse a footprint string into `(capability, rights)`. A bare name carries the
/// full right-set; brackets list a subset (with `Net`'s unmentioned axis filled
/// to full, so a stored `Net[Connect]` round-trips to its true right-set).
fn parse_cap(s: &str) -> (String, BTreeSet<String>) {
    if let Some(open) = s.find('[') {
        let name = s[..open].to_string();
        let inner = s[open + 1..].trim_end_matches(']');
        let mut rights: BTreeSet<String> = inner
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        if name == "Net" {
            default_net_axes(&mut rights);
        }
        (name, rights)
    } else {
        let r = full_rights(s);
        (s.to_string(), r)
    }
}

/// Collapse a flat footprint string-set into `capability -> union of rights`,
/// merging multiple entries for one capability (one module's `Net[Connect]` and
/// another's `Net[Listen]` become `Net -> {Connect, Listen}`).
fn normalize(set: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in set {
        let (name, rights) = parse_cap(s);
        m.entry(name).or_default().extend(rights);
    }
    m
}

/// The capabilities/rights present in `a` but not covered by `b`, rendered back
/// to footprint strings. A wholly-new capability, or a new right on a shared one,
/// both appear — the rights-precise difference behind `widening_over` and
/// `check_declared` (undeclared = demanded − declared).
pub fn cap_difference(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    let (na, nb) = (normalize(a), normalize(b));
    let mut out = BTreeSet::new();
    for (cap, ar) in &na {
        match nb.get(cap) {
            None => {
                out.insert(render_cap(cap, ar));
            }
            Some(br) => {
                let extra: BTreeSet<String> = ar.difference(br).cloned().collect();
                if !extra.is_empty() {
                    out.insert(render_cap(cap, &extra));
                }
            }
        }
    }
    out
}

/// Whether `set` (an allowed/declared footprint) covers a single demanded
/// capability string: the capability is present and its granted rights include
/// every demanded one. A bare grant (`Net`) covers any narrowing (`Net[Listen]`).
pub fn covers(set: &BTreeSet<String>, demand: &str) -> bool {
    let (name, dr) = parse_cap(demand);
    match normalize(set).get(&name) {
        Some(granted) => dr.is_subset(granted),
        None => false,
    }
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

    /// The capabilities/rights present in `self` but not covered by `base`, per
    /// axis. A non-empty result means `self` *widens* the footprint (the gated
    /// event) — including a new right on a capability already present.
    pub fn widening_over(&self, base: &Footprint) -> Widening {
        Widening {
            runtime: cap_difference(&self.runtime, &base.runtime),
            build: cap_difference(&self.build, &base.build),
        }
    }

    /// Collapse redundant per-capability entries (merging their rights) so the
    /// stored set holds one tidy string per capability.
    pub fn normalize(&mut self) {
        let tidy = |set: &BTreeSet<String>| -> BTreeSet<String> {
            normalize(set)
                .iter()
                .map(|(n, r)| render_cap(n, r))
                .collect()
        };
        self.runtime = tidy(&self.runtime);
        self.build = tidy(&self.build);
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
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } => {}
        }
    }
    fp.normalize();
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
    // Under-declaration is demanded minus declared, rights-aware: declaring a
    // bare `Net` covers a demanded `Net[Connect]`, but declaring `Net[Connect]`
    // does not cover a demanded `Net[Listen]`.
    let ur: Vec<String> = cap_difference(&computed.runtime, &dr).into_iter().collect();
    let ub: Vec<String> = cap_difference(&computed.build, &db).into_iter().collect();
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
    fp.normalize();
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
                    out.insert(render_cap(name, &rights_from_args(name, args)));
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
        let f = fp(r#"
fn add(a: Int, b: Int) -> Int:
    (a + b)
"#);
        assert!(f.is_empty());
        assert_eq!(f.determinism(), "guaranteed");
    }

    #[test]
    fn net_param_is_detected() {
        let f = fp(r#"
fn fetch(net: Net, url: String) -> String:
    url
"#);
        assert!(f.runtime.contains("Net"));
        assert_eq!(f.runtime.len(), 1);
        assert!(f.build.is_empty());
    }

    #[test]
    fn multiple_caps_across_functions() {
        let f = fp(r#"
fn log(console: Console, msg: String):
    print(msg)

fn save(dir: Dir, name: String) -> String:
    name
"#);
        assert!(f.runtime.contains("Console"));
        assert!(f.runtime.contains("Dir"));
    }

    #[test]
    fn taint_through_user_record() {
        // A record holding a Net taints; a function taking that record demands Net.
        let f = fp(r#"
type Client:
    conn: Net
    name: String

fn use_client(c: Client) -> String:
    (c).name
"#);
        assert!(f.runtime.contains("Net"), "taint should propagate through Client");
    }

    #[test]
    fn taint_is_transitive() {
        let f = fp(r#"
type Inner:
    n: Net

type Outer:
    inner: Inner
    tag: String

fn f(o: Outer) -> String:
    (o).tag
"#);
        assert!(f.runtime.contains("Net"), "taint must reach through two levels");
    }

    #[test]
    fn actor_capability_field_counts() {
        let f = fp(r#"
actor Logger:
    console: Console
    var count: Int = 0

impl Logger:
    on log(msg: String):
        print(msg)
"#);
        assert!(f.runtime.contains("Console"));
    }

    #[test]
    fn build_cap_routes_to_build_axis() {
        let f = fp(r#"
fn build(out: BuildOut, exec: BuildExec):
    print("gen")
"#);
        assert!(f.build.contains("BuildOut"));
        assert!(f.build.contains("BuildExec"));
        assert!(f.runtime.is_empty());
        assert_eq!(f.determinism(), "pinned-only");
    }

    #[test]
    fn build_without_exec_is_guaranteed() {
        let f = fp(r#"
fn build(out: BuildOut, read: BuildRead):
    print("x")
"#);
        assert_eq!(f.determinism(), "guaranteed");
    }

    #[test]
    fn main_is_excluded_from_footprint() {
        // main's capability params are the root grant, not a demand on a caller.
        let f = fp(r#"
fn main(console: Console):
    print(console, "hi")
"#);
        assert!(f.is_empty(), "main must not contribute to the footprint");
        // But a non-main function's caps still count.
        let f2 = fp(r#"
fn main(c: Console):
    print(c, "x")

fn helper(net: Net) -> Int:
    0
"#);
        assert!(f2.runtime.contains("Net"));
        assert!(!f2.runtime.contains("Console"), "Console came only via main");
    }

    #[test]
    fn check_declared_catches_underdeclaration() {
        let f = fp(r#"
fn fetch(net: Net, u: String) -> String:
    u
"#);
        assert!(check_declared(&f, &[], &[]).is_ok(), "no contract = ok");
        assert!(
            check_declared(&f, &["Console".into()], &[]).is_err(),
            "declares Console but demands Net"
        );
        assert!(check_declared(&f, &["Net".into()], &[]).is_ok(), "declares Net, demands Net");
    }

    #[test]
    fn widening_detects_new_kind() {
        let old = fp(r#"
fn log(c: Console):
    print("x")
"#);
        let new = fp(r#"
fn log(c: Console):
    print("x")

fn beacon(n: Net):
    print("x")
"#);
        let w = new.widening_over(&old);
        assert!(w.runtime.contains("Net"));
        assert!(!w.is_empty());
        // Narrowing (old over new) is free.
        assert!(old.widening_over(&new).is_empty());
    }

    #[test]
    fn net_verb_is_carried_into_the_footprint() {
        // A client rune demands `Net[Connect]`, not a bare `Net`.
        let f = fp("fn fetch(n: Net[Connect], u: String) -> String:\n    u\n");
        assert!(f.runtime.contains("Net[Connect]"));
        assert!(!f.runtime.contains("Net"));
    }

    #[test]
    fn verbs_union_across_functions_to_full_net() {
        // One function connects, another listens — the rune's footprint is full Net.
        let f = fp(
            "fn fetch(n: Net[Connect]) -> Int:\n    0\nfn serve(n: Net[Listen]) -> Int:\n    0\n",
        );
        assert_eq!(f.runtime.iter().cloned().collect::<Vec<_>>(), vec!["Net"]);
    }

    #[test]
    fn gaining_a_verb_is_a_widening() {
        // A `Net[Connect]` client that learns to `listen` widens — verb-precisely.
        let old = fp("fn h(n: Net[Connect]) -> Int:\n    0\n");
        let new = fp("fn h(n: Net[Connect, Listen]) -> Int:\n    0\n");
        let w = new.widening_over(&old);
        assert_eq!(w.runtime.iter().cloned().collect::<Vec<_>>(), vec!["Net[Listen]"]);
        assert!(!w.is_empty());
        // Dropping the verb back is a safe narrowing.
        assert!(old.widening_over(&new).is_empty());
    }

    #[test]
    fn dropping_a_dir_right_is_not_a_widening() {
        let old = fp("fn load(d: Dir) -> Int:\n    0\n");
        let new = fp("fn load(d: Dir[Read]) -> Int:\n    0\n");
        assert!(new.widening_over(&old).is_empty());
        assert!(old.widening_over(&new).runtime.contains("Dir[Write]"));
    }

    #[test]
    fn declared_coverage_is_rights_aware() {
        let f = fp("fn fetch(n: Net[Connect], u: String) -> String:\n    u\n");
        // A bare `Net` declaration covers the narrowed `Net[Connect]` demand.
        assert!(check_declared(&f, &["Net".into()], &[]).is_ok());
        // Declaring the exact verb also covers it.
        assert!(check_declared(&f, &["Net[Connect]".into()], &[]).is_ok());
        // Declaring only the *other* verb under-declares (demands Connect, not Listen).
        assert!(check_declared(&f, &["Net[Listen]".into()], &[]).is_err());
    }

    #[test]
    fn net_transport_is_carried_and_gaining_one_widens() {
        // A TCP-pinned client audits as `Net[Connect, Tcp]`, distinct from a
        // transport-agnostic `Net[Connect]`.
        let pinned = fp("fn dial(n: Net[Connect, Tcp]) -> Int:\n    0\n");
        assert!(pinned.runtime.contains("Net[Connect, Tcp]"));
        // Opening the transport axis (TCP-pinned -> all transports) gains Udp/Uds.
        let open = fp("fn dial(n: Net[Connect]) -> Int:\n    0\n");
        let w = open.widening_over(&pinned);
        assert!(w.runtime.iter().any(|k| k.contains("Udp")), "should gain Udp: {:?}", w.runtime);
        // Pinning back to TCP is a safe narrowing.
        assert!(pinned.widening_over(&open).is_empty());
        // A bare `Net` declaration still covers a transport-pinned demand.
        assert!(check_declared(&pinned, &["Net".into()], &[]).is_ok());
    }
}
