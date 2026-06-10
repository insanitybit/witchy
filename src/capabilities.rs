//! Capability footprint analysis — the auditable core of witchy's supply-chain
//! story.
//!
//! Witchy's host capabilities (`Console`, `Dir`, `Net`) are unforgeable: no
//! expression can construct one, and there is no ambient authority. A capability
//! can only enter code as a parameter. Therefore a function's authority is
//! exactly its capability-typed parameters, and a module's footprint is the
//! union over its entry points (public functions, `main`, and the fields an
//! actor is granted at spawn). Unlike Go — where any dependency runs with your
//! full ambient authority — this makes "what can this code touch?" statically
//! computable, so a dependency that *widens* its footprint (suddenly asks for
//! `Net`, or asks for a `Net` it can now *listen* on) is visible and gateable.
//!
//! The footprint is right-precise: a capability carries the *verbs* it permits
//! (`Dir[Read]`, `Net[Connect]`), so the audit distinguishes a read-only loader
//! from one that writes files, or a client from a server. Bare `Dir`/`Net` carry
//! the full right-set.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ast::{Item, Module, Type};

/// The host capabilities the runtime grants at an entry point. (`Subject`, an
/// actor handle from `spawn`, is intra-program authority, not host authority,
/// so it isn't a supply-chain footprint concern.)
pub const HOST_CAPABILITIES: &[&str] = &["Console", "Clock", "Env", "Secret", "Dir", "Net"];

/// The build-time capabilities a rune's `build` entrypoint may demand — the
/// parallel set to the runtime host caps, tracked on a separate axis. Kind-only
/// (the specific tool/dir/host/var is the consumer's grant, not the type), so
/// they carry no rights. See docs/build-time-execution-plan.md.
pub const BUILD_CAPABILITIES: &[&str] =
    &["BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec"];

/// The rights (verbs) a single capability permits. Empty for `Console`, which
/// has no sub-verbs.
pub type Rights = BTreeSet<&'static str>;

/// A capability footprint: each present capability mapped to the union of rights
/// demanded for it. `Dir` ⇒ `{Read, Write}`, `Net` ⇒ `{Connect, Listen}`.
pub type CapSet = BTreeMap<&'static str, Rights>;

fn host_cap(name: &str) -> Option<&'static str> {
    HOST_CAPABILITIES.iter().copied().find(|c| *c == name)
}

fn build_cap(name: &str) -> Option<&'static str> {
    BUILD_CAPABILITIES.iter().copied().find(|c| *c == name)
}

/// Build-time capability kinds reachable from a type (no rights — kind-only).
/// Used only over the `build` entrypoint's parameters; recurses through tuples/
/// generics for soundness even though a build cap is normally a direct param.
fn build_caps_in(ty: &Type, out: &mut CapSet) {
    match ty {
        Type::Named(name, args) => {
            if let Some(b) = build_cap(name) {
                out.entry(b).or_default();
            }
            for a in args {
                build_caps_in(a, out);
            }
        }
        Type::Tuple(ts) => ts.iter().for_each(|t| build_caps_in(t, out)),
        Type::Fn(params, ret) => {
            params.iter().for_each(|p| build_caps_in(p, out));
            build_caps_in(ret, out);
        }
    }
}

/// The full right-set for a capability — what a *bare* `Dir`/`Net` (no brackets)
/// confers. `Console` and unknown names have no rights.
fn full_rights(cap: &str) -> Rights {
    match cap {
        "Dir" => ["Read", "Write"].into_iter().collect(),
        // `Net` has two axes: verbs and transports. Bare `Net` is full on both.
        "Net" => ["Connect", "Listen", "Tcp", "Udp", "Uds"].into_iter().collect(),
        _ => Rights::new(),
    }
}

const NET_VERBS: [&str; 2] = ["Connect", "Listen"];
const NET_TRANSPORTS: [&str; 3] = ["Tcp", "Udp", "Uds"];

/// Map a bracketed marker to its canonical right name, or `None` if it isn't a
/// recognized right for that capability.
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

/// The rights a capability-typed annotation confers: the bracketed markers if
/// present, else (bare capability) the full set. `Net`'s two axes default
/// independently — an axis with no marker mentioned is full — so `Net[Connect]`
/// keeps all transports, matching the type system.
fn rights_from_args(cap: &'static str, args: &[Type]) -> Rights {
    if args.is_empty() {
        return full_rights(cap);
    }
    let mut r = Rights::new();
    for a in args {
        if let Type::Named(n, _) = a {
            if let Some(m) = right_marker(cap, n) {
                r.insert(m);
            }
        }
    }
    if cap == "Net" {
        if !NET_VERBS.iter().any(|v| r.contains(v)) {
            r.extend(NET_VERBS);
        }
        if !NET_TRANSPORTS.iter().any(|t| r.contains(t)) {
            r.extend(NET_TRANSPORTS);
        }
    }
    r
}

/// Union `src` into `dst`, merging the rights of capabilities present in both.
fn merge_into(dst: &mut CapSet, src: &CapSet) {
    for (cap, rights) in src {
        dst.entry(cap).or_default().extend(rights.iter().copied());
    }
}

/// Host capabilities (with rights) reachable from a type, resolving user types
/// through `taint`. A capability wrapped in a type — a brand like
/// `ConfigDir(Dir[Read])`, or any record holding one — still confers that
/// authority (at those rights) on whoever receives the value, so the analyzer
/// must see through the wrapper to stay sound.
fn caps_in(ty: &Type, taint: &HashMap<String, CapSet>, out: &mut CapSet) {
    match ty {
        Type::Named(name, args) => {
            if let Some(h) = host_cap(name) {
                out.entry(h).or_default().extend(rights_from_args(h, args));
            }
            if let Some(caps) = taint.get(name) {
                merge_into(out, caps);
            }
            for a in args {
                caps_in(a, taint, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                caps_in(t, taint, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                caps_in(p, taint, out);
            }
            caps_in(ret, taint, out);
        }
    }
}

/// For each user type, the host capabilities (with rights) a value of it carries
/// (transitively through its fields). Computed to a fixpoint, since a type may
/// be tainted by another tainted user type.
fn taint_map(module: &Module) -> HashMap<String, CapSet> {
    let mut map: HashMap<String, CapSet> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            map.entry(t.name.clone()).or_default();
        }
    }
    loop {
        let mut changed = false;
        for item in &module.items {
            let Item::Type(t) = item else { continue };
            let mut acc = CapSet::new();
            for v in &t.variants {
                for fty in &v.fields {
                    caps_in(fty, &map, &mut acc);
                }
            }
            let slot = map.entry(t.name.clone()).or_default();
            let before = slot.clone();
            merge_into(slot, &acc);
            if *slot != before {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    map
}

/// Single-field newtype brands (`type ConfigDir: ConfigDir(Dir)`): a one-variant,
/// one-field type wrapping exactly one host capability (directly or via another
/// brand). The brand name is reported as a refinement of the bare capability —
/// authority-equivalent to it, but carrying the program's intent.
fn brand_map(
    module: &Module,
    taint: &HashMap<String, CapSet>,
) -> HashMap<String, &'static str> {
    let mut brands = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            if t.variants.len() == 1 && t.variants[0].fields.len() == 1 {
                let mut caps = CapSet::new();
                caps_in(&t.variants[0].fields[0], taint, &mut caps);
                if caps.len() == 1 {
                    brands.insert(t.name.clone(), *caps.keys().next().unwrap());
                }
            }
        }
    }
    brands
}

/// One entry point: the host capabilities (with rights) it requires, plus the
/// names of any capability brands it receives them through (a display-only
/// refinement).
pub struct Entry {
    pub name: String,
    pub capabilities: CapSet,
    pub brands: BTreeSet<String>,
}

/// A module's capability footprint: each entry point's requirements and the
/// union across all of them (the maximum host authority the module can wield).
pub struct Footprint {
    pub entries: Vec<Entry>,
    pub total: CapSet,
    /// The capability brands (refinements) used anywhere in the module — the
    /// union of every entry's brands. Authority-equivalent to their host caps,
    /// but a finer-grained record of *intent*.
    pub brands: BTreeSet<String>,
    /// The **build-time** footprint: the build capabilities the rune's `build`
    /// entrypoint demands (empty if it ships no build step). A separate axis from
    /// the runtime `total` — they are granted and gated independently.
    pub build: CapSet,
}

/// What changed between two versions of a module's footprint. `added` is a
/// *widening* — host authority the newer version demands that the older did not.
/// That includes a wholly new capability (a dependency that suddenly asks for
/// `Net`) *and* a new right on an existing one (a `Net[Connect]` that can now
/// also `Listen`). `removed` is a narrowing (a dropped capability or right),
/// which is always safe. The supply-chain gate blocks on widening.
///
/// `refinements_dropped`/`refinements_gained` track *brand* changes. They never
/// change host authority — a brand is authority-equivalent to its host cap — so
/// they don't fail the gate, but a dropped refinement (a confined `ConfigDir`
/// loosened back to a raw `Dir`) is an intent change worth surfacing in review.
pub struct FootprintDiff {
    pub added: CapSet,
    pub removed: CapSet,
    /// Build-axis changes, tracked independently of the runtime axis. A
    /// `build_added` is a build-time widening — "this version now wants to `exec`
    /// / reach the network at build time" — gated separately (`--allow-build-cap`).
    pub build_added: CapSet,
    pub build_removed: CapSet,
    pub refinements_dropped: BTreeSet<String>,
    pub refinements_gained: BTreeSet<String>,
}

impl FootprintDiff {
    /// Whether the newer footprint demands authority the older one did not. This
    /// is the signal the install/CI gate fails on: new authority — a new
    /// capability or a new right on an existing one — must be an explicit,
    /// reviewed decision, never something a version bump slips in. Brand changes
    /// are intentional refinements, not authority, so they never trip this.
    pub fn widened(&self) -> bool {
        !self.added.is_empty() || !self.build_added.is_empty()
    }

    /// Whether the *build* axis specifically widened — the consuming project must
    /// grant the new build capability (and pass `--allow-build-cap`) to proceed.
    pub fn build_widened(&self) -> bool {
        !self.build_added.is_empty()
    }
}

/// The capabilities/rights present in `a` but not `b`: a wholly new capability,
/// or new rights on a shared one. The primitive behind both directions of a diff.
fn cap_delta(a: &CapSet, b: &CapSet) -> CapSet {
    let mut out = CapSet::new();
    for (cap, ar) in a {
        match b.get(cap) {
            None => {
                out.insert(cap, ar.clone());
            }
            Some(br) => {
                let extra: Rights = ar.difference(br).copied().collect();
                if !extra.is_empty() {
                    out.insert(cap, extra);
                }
            }
        }
    }
    out
}

/// Compare two footprints by their total authority — the primitive behind the
/// block-on-widening gate. Because capabilities are unforgeable and only enter
/// through parameters, a module cannot gain authority without changing a public
/// entry point's signature, so this total-level diff fully captures a widening.
/// Brand differences are reported alongside as refinement (intent) changes.
pub fn diff(old: &Footprint, new: &Footprint) -> FootprintDiff {
    FootprintDiff {
        added: cap_delta(&new.total, &old.total),
        removed: cap_delta(&old.total, &new.total),
        build_added: cap_delta(&new.build, &old.build),
        build_removed: cap_delta(&old.build, &new.build),
        refinements_dropped: old.brands.difference(&new.brands).cloned().collect(),
        refinements_gained: new.brands.difference(&old.brands).cloned().collect(),
    }
}

/// Render one capability with its rights for human output: a bare name when it
/// has the full right-set (or none, like `Console`), else bracketed —
/// `Console`, `Dir`, `Dir[Read]`, `Net[Connect]`, `Net[Connect, Tcp]`.
pub fn show_cap(name: &str, rights: &Rights) -> String {
    if rights.is_empty() || *rights == full_rights(name) {
        return name.to_string();
    }
    if name == "Net" {
        // Two axes: omit an axis that is at its full set, so the expanded
        // `{Connect, Tcp, Udp, Uds}` prints as `Net[Connect]`, not the verbose
        // transport list. (Mirrors `NetRights`' Display in the type checker.)
        let mut parts: Vec<&str> = Vec::new();
        if !NET_VERBS.iter().all(|v| rights.contains(v)) {
            parts.extend(NET_VERBS.iter().filter(|v| rights.contains(*v)));
        }
        if !NET_TRANSPORTS.iter().all(|t| rights.contains(t)) {
            parts.extend(NET_TRANSPORTS.iter().filter(|t| rights.contains(*t)));
        }
        return format!("Net[{}]", parts.join(", "));
    }
    format!(
        "{name}[{}]",
        rights.iter().copied().collect::<Vec<_>>().join(", ")
    )
}

/// Render a whole capability set as a comma-joined list, or `(none)`.
pub fn show_caps(caps: &CapSet) -> String {
    if caps.is_empty() {
        "(none)".to_string()
    } else {
        caps.iter()
            .map(|(n, r)| show_cap(n, r))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn analyze(module: &Module) -> Footprint {
    let taint = taint_map(module);
    let brands = brand_map(module, &taint);
    let mut entries = Vec::new();
    let mut total = CapSet::new();
    for item in &module.items {
        // The capability-bearing types at this entry point: a public function's
        // (or `main`'s) parameters, or an actor's spawn-granted fields.
        let (name, types): (String, Vec<&Type>) = match item {
            Item::Function(f) if f.public || f.name == "main" => (
                f.name.clone(),
                f.params.iter().filter_map(|p| p.ty.as_ref()).collect(),
            ),
            Item::Actor(a) => (
                format!("actor {}", a.name),
                a.fields
                    .iter()
                    .filter(|fl| fl.init.is_none())
                    .map(|fl| &fl.ty)
                    .collect(),
            ),
            _ => continue,
        };
        let mut capabilities = CapSet::new();
        let mut entry_brands = BTreeSet::new();
        for ty in types {
            let mut caps = CapSet::new();
            caps_in(ty, &taint, &mut caps);
            if caps.is_empty() {
                continue;
            }
            merge_into(&mut capabilities, &caps);
            // A directly-named brand is recorded as a refinement.
            if let Type::Named(n, _) = ty {
                if brands.contains_key(n.as_str()) {
                    entry_brands.insert(n.clone());
                }
            }
        }
        merge_into(&mut total, &capabilities);
        entries.push(Entry {
            name,
            capabilities,
            brands: entry_brands,
        });
    }
    let brands = entries.iter().flat_map(|e| e.brands.iter().cloned()).collect();
    // The build axis: the build capabilities the `build` entrypoint demands,
    // computed identically over its signature (§4.1). Build caps can only appear
    // there (the signature checks enforce it), so this never overlaps `total`.
    let mut build = CapSet::new();
    if let Some(b) = crate::typeck::build_entrypoint(module) {
        for ty in b.params.iter().filter_map(|p| p.ty.as_ref()) {
            build_caps_in(ty, &mut build);
        }
    }
    Footprint {
        entries,
        total,
        brands,
        build,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    fn footprint(src: &str) -> Footprint {
        analyze(&parse_module(src).expect("parse"))
    }

    /// Build a `CapSet` from `(cap, &[rights])` pairs, for terse assertions.
    fn cs(items: &[(&'static str, &[&'static str])]) -> CapSet {
        items
            .iter()
            .map(|(c, rs)| (*c, rs.iter().copied().collect::<Rights>()))
            .collect()
    }

    // Bare `Net` is full on both axes; `Net[Connect]` keeps all transports.
    const NET_FULL: &[&str] = &["Connect", "Listen", "Tcp", "Udp", "Uds"];
    const NET_CONNECT: &[&str] = &["Connect", "Tcp", "Udp", "Uds"];

    #[test]
    fn pure_functions_have_no_footprint() {
        let fp = footprint(r#"
pub fn add(a: Int, b: Int) -> Int:
    (a + b)
"#);
        assert!(fp.total.is_empty());
        assert_eq!(fp.entries.len(), 1);
        assert!(fp.entries[0].capabilities.is_empty());
        assert!(fp.build.is_empty(), "no build step ⇒ empty build footprint");
    }

    #[test]
    fn build_footprint_is_the_union_of_the_build_entrypoints_caps() {
        // The build axis is the `build` entrypoint's build caps; it is separate
        // from the runtime axis (here empty — a pure codegen rune).
        let fp = footprint(
            "fn build(out: BuildOut, schema: BuildRead, cc: BuildExec):\n    write_out(out, \"x.witchy\", read_build(schema, \"a.proto\"))\n",
        );
        assert!(fp.total.is_empty(), "runtime footprint is empty for a pure build step");
        assert_eq!(
            fp.build,
            cs(&[("BuildOut", &[]), ("BuildRead", &[]), ("BuildExec", &[])])
        );
    }

    #[test]
    fn a_build_axis_widening_is_flagged_independently_of_runtime() {
        let old = footprint("fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x\", read_build(schema, \"a\"))\n");
        let new = footprint("fn build(out: BuildOut, schema: BuildRead, dl: BuildNet):\n    write_out(out, \"x\", fetch_build(dl, \"h\", \"/a\"))\n");
        let d = diff(&old, &new);
        assert!(d.build_widened(), "a new build cap is a build-axis widening");
        assert!(d.widened(), "build widening counts as an overall widening (gates)");
        assert_eq!(d.build_added, cs(&[("BuildNet", &[])]));
        assert!(d.added.is_empty(), "the runtime axis did not widen");
        // The reverse is a safe narrowing.
        assert!(!diff(&new, &old).build_widened());
    }

    #[test]
    fn footprint_is_the_union_of_entry_capabilities() {
        let src = r#"
fn helper(a: Int) -> Int:
    a

pub fn serve(console: Console, net: Net) -> Int:
    0

fn main(console: Console):
    print(console, "hi")
"#;
        let fp = footprint(src);
        // Private `helper` is not an entry point; the union is Console + Net.
        assert_eq!(
            fp.total,
            cs(&[("Console", &[]), ("Net", NET_FULL)])
        );
        let names: Vec<&str> = fp.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["serve", "main"]);
        let serve = fp.entries.iter().find(|e| e.name == "serve").unwrap();
        assert_eq!(
            serve.capabilities,
            cs(&[("Console", &[]), ("Net", NET_FULL)])
        );
    }

    #[test]
    fn diff_flags_a_widening_as_added_authority() {
        // A dependency update whose public API newly demands `Net` is a widening:
        // the gate must see `Net` as added and report widened().
        let old = footprint(r#"
pub fn serve(console: Console) -> Int:
    0
"#);
        let new = footprint(r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#);
        let d = diff(&old, &new);
        assert_eq!(d.added, cs(&[("Net", NET_FULL)]));
        assert!(d.removed.is_empty());
        assert!(d.widened());
    }

    #[test]
    fn diff_of_unchanged_footprint_is_not_a_widening() {
        let old = footprint(r#"
pub fn serve(net: Net) -> Int:
    0
"#);
        let new = footprint(r#"
pub fn serve(net: Net) -> Int:
    1
"#);
        let d = diff(&old, &new);
        assert!(d.added.is_empty());
        assert!(!d.widened());
    }

    #[test]
    fn diff_treats_dropped_authority_as_a_safe_narrowing() {
        // Giving up `Net` is a narrowing — recorded in `removed`, never widened().
        let old = footprint(r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#);
        let new = footprint(r#"
pub fn serve(console: Console) -> Int:
    0
"#);
        let d = diff(&old, &new);
        assert!(d.added.is_empty());
        assert_eq!(d.removed, cs(&[("Net", NET_FULL)]));
        assert!(!d.widened());
    }

    #[test]
    fn dir_rights_appear_in_the_footprint() {
        // A read-only loader audits as `Dir[Read]`, not a bare `Dir`.
        let fp = footprint("pub fn load(d: Dir[Read]) -> Int:\n    0\n");
        assert_eq!(fp.total, cs(&[("Dir", &["Read"])]));
        assert_eq!(show_caps(&fp.total), "Dir[Read]");
    }

    #[test]
    fn net_verbs_appear_in_the_footprint() {
        // A client audits as `Net[Connect]`; a server as `Net[Listen]`.
        let client = footprint("pub fn fetch(n: Net[Connect]) -> Int:\n    0\n");
        assert_eq!(client.total, cs(&[("Net", NET_CONNECT)]));
        assert_eq!(show_caps(&client.total), "Net[Connect]");
        let server = footprint("pub fn serve(n: Net[Listen]) -> Int:\n    0\n");
        assert_eq!(show_caps(&server.total), "Net[Listen]");
    }

    #[test]
    fn net_transport_appears_in_the_footprint() {
        // A TCP-pinned client audits as `Net[Connect, Tcp]`, distinct from a
        // transport-agnostic `Net[Connect]`.
        let fp = footprint("pub fn dial(n: Net[Connect, Tcp]) -> Int:\n    0\n");
        assert_eq!(fp.total, cs(&[("Net", &["Connect", "Tcp"])]));
        assert_eq!(show_caps(&fp.total), "Net[Connect, Tcp]");
        // Gaining a transport is a widening: a TCP-pinned client that opens up to
        // all transports demands `Udp`/`Uds` it did not before.
        let pinned = footprint("pub fn h(n: Net[Connect, Tcp]) -> Int:\n    0\n");
        let open = footprint("pub fn h(n: Net[Connect]) -> Int:\n    0\n");
        let d = diff(&pinned, &open);
        assert_eq!(d.added, cs(&[("Net", &["Udp", "Uds"])]));
        assert!(d.widened());
        // The reverse (pinning to TCP) is a safe narrowing.
        assert!(!diff(&open, &pinned).widened());
    }

    #[test]
    fn entry_rights_union_across_entry_points() {
        // One entry connects, another listens — the module's total is both verbs.
        let fp = footprint(
            "pub fn fetch(n: Net[Connect]) -> Int:\n    0\npub fn serve(n: Net[Listen]) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, cs(&[("Net", NET_FULL)]));
        assert_eq!(show_caps(&fp.total), "Net");
    }

    #[test]
    fn gaining_a_right_is_a_widening() {
        // A `Net[Connect]` client that now also listens is a widening — the
        // supply-chain signal is verb-precise.
        let old = footprint("pub fn h(n: Net[Connect]) -> Int:\n    0\n");
        let new = footprint("pub fn h(n: Net[Connect, Listen]) -> Int:\n    0\n");
        let d = diff(&old, &new);
        assert_eq!(d.added, cs(&[("Net", &["Listen"])]));
        assert!(d.removed.is_empty());
        assert!(d.widened());
    }

    #[test]
    fn dropping_a_right_is_a_safe_narrowing() {
        // The reverse — a full `Dir` tightened to `Dir[Read]` — drops `Write`,
        // a narrowing that never trips the gate.
        let old = footprint("pub fn load(d: Dir) -> Int:\n    0\n");
        let new = footprint("pub fn load(d: Dir[Read]) -> Int:\n    0\n");
        let d = diff(&old, &new);
        assert!(d.added.is_empty());
        assert_eq!(d.removed, cs(&[("Dir", &["Write"])]));
        assert!(!d.widened());
    }

    #[test]
    fn branded_capability_is_seen_through_and_refined() {
        // A `ConfigDir(Dir)` brand still confers `Dir` authority (sound), and the
        // brand name is reported as a refinement.
        let fp = footprint(
            "type ConfigDir:\n    ConfigDir(Dir)\npub fn load(c: ConfigDir) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, cs(&[("Dir", &["Read", "Write"])]));
        let load = fp.entries.iter().find(|e| e.name == "load").unwrap();
        assert!(load.capabilities.contains_key("Dir"));
        assert!(load.brands.contains("ConfigDir"));
    }

    #[test]
    fn a_brand_carries_the_rights_of_the_capability_it_wraps() {
        // A brand over a narrowed capability audits at those rights, not full.
        let fp = footprint(
            "type LogDir:\n    LogDir(Dir[Write])\npub fn log(d: LogDir) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, cs(&[("Dir", &["Write"])]));
        assert_eq!(show_caps(&fp.total), "Dir[Write]");
        let log = fp.entries.iter().find(|e| e.name == "log").unwrap();
        assert!(log.brands.contains("LogDir"));
    }

    #[test]
    fn single_field_record_is_also_a_brand() {
        // A one-field record wrapping a capability is a brand too, not just a
        // positional newtype.
        let fp = footprint(
            "type ConfigDir:\n    dir: Dir\npub fn load(c: ConfigDir) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, cs(&[("Dir", &["Read", "Write"])]));
        let load = fp.entries.iter().find(|e| e.name == "load").unwrap();
        assert!(load.brands.contains("ConfigDir"));
    }

    #[test]
    fn brands_resolve_transitively() {
        // A brand of a brand of `Net` still audits as `Net`.
        let fp = footprint(
            "type Raw:\n    Raw(Net)\ntype Api:\n    Api(Raw)\npub fn fetch(a: Api) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, cs(&[("Net", NET_FULL)]));
    }

    #[test]
    fn any_capability_carrying_type_taints() {
        // Not just newtype brands: a record that holds a `Net` confers `Net` on a
        // caller, so the footprint must include it.
        let fp = footprint(
            "type Conn:\n    host: String\n    net: Net\npub fn open(c: Conn) -> Int:\n    0\n",
        );
        assert!(fp.total.contains_key("Net"));
    }

    #[test]
    fn dropping_a_brand_is_a_refinement_change_not_a_widening() {
        // v1 confines its Dir as `ConfigDir`; v2 takes a raw `Dir`. Same host
        // authority (no widening), but the refinement is dropped — surfaced.
        let old = footprint("type ConfigDir:\n    ConfigDir(Dir)\npub fn load(c: ConfigDir) -> Int:\n    0\n");
        let new = footprint("pub fn load(d: Dir) -> Int:\n    0\n");
        let d = diff(&old, &new);
        assert!(d.added.is_empty());
        assert!(!d.widened());
        assert_eq!(
            d.refinements_dropped,
            ["ConfigDir".to_string()].into_iter().collect::<BTreeSet<_>>()
        );
        assert!(d.refinements_gained.is_empty());
        // The reverse direction reports the brand as gained (tightened intent).
        let back = diff(&new, &old);
        assert!(back.refinements_dropped.is_empty());
        assert_eq!(
            back.refinements_gained,
            ["ConfigDir".to_string()].into_iter().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn a_branded_capability_cannot_hide_a_widening() {
        // Adding a function that takes a branded `Net` is still a widening — the
        // brand does not let a dependency slip new authority past the gate.
        let old = footprint("pub fn load(d: Dir) -> Int:\n    0\n");
        let new = footprint(
            "type ApiNet:\n    ApiNet(Net)\npub fn load(d: Dir) -> Int:\n    0\npub fn sync(n: ApiNet) -> Int:\n    0\n",
        );
        let d = diff(&old, &new);
        assert_eq!(d.added, cs(&[("Net", NET_FULL)]));
        assert!(d.widened());
    }

    #[test]
    fn actor_spawn_fields_count_as_footprint() {
        let src = r#"
actor Logger:
    console: Console
    var count: Int = 0

impl Logger:
    on Log(msg: String):
        count = (count + 1)
"#;
        let fp = footprint(src);
        assert!(fp.total.contains_key("Console"));
    }

    /// Supply-chain regression net: the bundled std modules must keep the
    /// capability footprints they advertise. The pure modules stay pure (empty
    /// footprint), and only the networking modules require authority — exactly
    /// `Net`, never `Console`/`Dir`. If a future change slips a capability param
    /// into, say, `list`, this fails loudly.
    #[test]
    fn std_module_footprints_are_pinned() {
        let pure = [
            "list", "string", "math", "option", "result", "func", "ord", "eq", "ascii", "set",
            "show", "json", "url", "duration", "random", "regex", "compiler", "toml", "semver",
            "rights", "dict", "csv", "time", "encoding", "path",
        ];
        for name in pure {
            let src = crate::linker::std_source(name).expect("bundled module");
            let fp = footprint(src);
            assert!(
                fp.total.is_empty(),
                "std module `{name}` should be pure but needs {:?}",
                fp.total
            );
        }
        // `crypto` is pure except for `sign`/`public_key`, which take a
        // `Secret` — so the module's surface demands exactly that (its hashing
        // and verification stay capability-free).
        let crypto = footprint(crate::linker::std_source("crypto").expect("bundled module"));
        assert_eq!(
            crypto.total.keys().copied().collect::<Vec<_>>(),
            vec!["Secret"],
            "crypto's only capability demand is Secret (for sign/public_key)",
        );
        // The networking modules take a bare `Net` (full verbs) for now — they
        // are not yet tightened to `Net[Connect]`/`Net[Listen]`.
        let net_only = ["http", "server"];
        for name in net_only {
            let src = crate::linker::std_source(name).expect("bundled module");
            let fp = footprint(src);
            assert_eq!(
                fp.total.keys().copied().collect::<Vec<_>>(),
                vec!["Net"],
                "networking module `{name}` should require only Net",
            );
        }
    }
}
