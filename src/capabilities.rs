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
//! `Net`) is visible and can be gated.

use std::collections::{BTreeSet, HashMap};

use crate::ast::{Item, Module, Type};

/// The host capabilities the runtime grants at an entry point. (`Subject`, an
/// actor handle from `spawn`, is intra-program authority, not host authority,
/// so it isn't a supply-chain footprint concern.)
pub const HOST_CAPABILITIES: &[&str] = &["Console", "Dir", "Net"];

fn host_cap(name: &str) -> Option<&'static str> {
    HOST_CAPABILITIES.iter().copied().find(|c| *c == name)
}

/// Host capabilities reachable from a type, resolving user types through `taint`.
/// A capability wrapped in a type — a brand like `ConfigDir(Dir)`, or any record
/// holding one — still confers that authority on whoever receives the value, so
/// the analyzer must see through the wrapper to stay sound.
fn caps_in(ty: &Type, taint: &HashMap<String, BTreeSet<&'static str>>, out: &mut BTreeSet<&'static str>) {
    match ty {
        Type::Named(name, args) => {
            if let Some(h) = host_cap(name) {
                out.insert(h);
            }
            if let Some(caps) = taint.get(name) {
                out.extend(caps.iter().copied());
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

/// For each user type, the host capabilities a value of it carries (transitively
/// through its fields). Computed to a fixpoint, since a type may be tainted by
/// another tainted user type.
fn taint_map(module: &Module) -> HashMap<String, BTreeSet<&'static str>> {
    let mut map: HashMap<String, BTreeSet<&'static str>> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            map.entry(t.name.clone()).or_default();
        }
    }
    loop {
        let mut changed = false;
        for item in &module.items {
            let Item::Type(t) = item else { continue };
            let mut acc = BTreeSet::new();
            for v in &t.variants {
                for fty in &v.fields {
                    caps_in(fty, &map, &mut acc);
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
    map
}

/// Single-field newtype brands (`type ConfigDir: ConfigDir(Dir)`): a one-variant,
/// one-field type wrapping exactly one host capability (directly or via another
/// brand). The brand name is reported as a refinement of the bare capability —
/// authority-equivalent to it, but carrying the program's intent.
fn brand_map(
    module: &Module,
    taint: &HashMap<String, BTreeSet<&'static str>>,
) -> HashMap<String, &'static str> {
    let mut brands = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            if t.variants.len() == 1 && t.variants[0].fields.len() == 1 {
                let mut caps = BTreeSet::new();
                caps_in(&t.variants[0].fields[0], taint, &mut caps);
                if caps.len() == 1 {
                    brands.insert(t.name.clone(), *caps.iter().next().unwrap());
                }
            }
        }
    }
    brands
}

/// One entry point: the host capabilities it requires, plus the names of any
/// capability brands it receives them through (a display-only refinement).
pub struct Entry {
    pub name: String,
    pub capabilities: BTreeSet<&'static str>,
    pub brands: BTreeSet<String>,
}

/// A module's capability footprint: each entry point's requirements and the
/// union across all of them (the maximum host authority the module can wield).
pub struct Footprint {
    pub entries: Vec<Entry>,
    pub total: BTreeSet<&'static str>,
    /// The capability brands (refinements) used anywhere in the module — the
    /// union of every entry's brands. Authority-equivalent to their host caps,
    /// but a finer-grained record of *intent*.
    pub brands: BTreeSet<String>,
}

/// What changed between two versions of a module's footprint. `added` is a
/// *widening* — host authority the newer version demands that the older did not
/// (e.g. a dependency update that suddenly asks for `Net`); `removed` is a
/// narrowing, which is always safe. The supply-chain gate blocks on widening.
///
/// `refinements_dropped`/`refinements_gained` track *brand* changes. They never
/// change host authority — a brand is authority-equivalent to its host cap — so
/// they don't fail the gate, but a dropped refinement (a confined `ConfigDir`
/// loosened back to a raw `Dir`) is an intent change worth surfacing in review.
pub struct FootprintDiff {
    pub added: BTreeSet<&'static str>,
    pub removed: BTreeSet<&'static str>,
    pub refinements_dropped: BTreeSet<String>,
    pub refinements_gained: BTreeSet<String>,
}

impl FootprintDiff {
    /// Whether the newer footprint demands authority the older one did not. This
    /// is the signal the install/CI gate fails on: new authority must be an
    /// explicit, reviewed decision, never something a version bump slips in.
    /// Brand changes are intentional refinements, not authority, so they never
    /// trip this.
    pub fn widened(&self) -> bool {
        !self.added.is_empty()
    }
}

/// Compare two footprints by their total authority — the primitive behind the
/// block-on-widening gate. Because capabilities are unforgeable and only enter
/// through parameters, a module cannot gain authority without changing a public
/// entry point's signature, so this total-level diff fully captures a widening.
/// Brand differences are reported alongside as refinement (intent) changes.
pub fn diff(old: &Footprint, new: &Footprint) -> FootprintDiff {
    FootprintDiff {
        added: new.total.difference(&old.total).copied().collect(),
        removed: old.total.difference(&new.total).copied().collect(),
        refinements_dropped: old.brands.difference(&new.brands).cloned().collect(),
        refinements_gained: new.brands.difference(&old.brands).cloned().collect(),
    }
}

pub fn analyze(module: &Module) -> Footprint {
    let taint = taint_map(module);
    let brands = brand_map(module, &taint);
    let mut entries = Vec::new();
    let mut total = BTreeSet::new();
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
        let mut capabilities = BTreeSet::new();
        let mut entry_brands = BTreeSet::new();
        for ty in types {
            let mut caps = BTreeSet::new();
            caps_in(ty, &taint, &mut caps);
            if caps.is_empty() {
                continue;
            }
            capabilities.extend(caps.iter().copied());
            // A directly-named brand is recorded as a refinement.
            if let Type::Named(n, _) = ty {
                if brands.contains_key(n.as_str()) {
                    entry_brands.insert(n.clone());
                }
            }
        }
        total.extend(capabilities.iter().copied());
        entries.push(Entry {
            name,
            capabilities,
            brands: entry_brands,
        });
    }
    let brands = entries.iter().flat_map(|e| e.brands.iter().cloned()).collect();
    Footprint {
        entries,
        total,
        brands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    fn footprint(src: &str) -> Footprint {
        analyze(&parse_module(src).expect("parse"))
    }

    #[test]
    fn pure_functions_have_no_footprint() {
        let fp = footprint(r#"
pub fn add(a: Int, b: Int) -> Int:
    (a + b)
"#);
        assert!(fp.total.is_empty());
        assert_eq!(fp.entries.len(), 1);
        assert!(fp.entries[0].capabilities.is_empty());
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
            ["Console", "Net"].into_iter().collect::<BTreeSet<_>>()
        );
        let names: Vec<&str> = fp.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["serve", "main"]);
        let serve = fp.entries.iter().find(|e| e.name == "serve").unwrap();
        assert_eq!(
            serve.capabilities,
            ["Console", "Net"].into_iter().collect::<BTreeSet<_>>()
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
        assert_eq!(d.added, ["Net"].into_iter().collect::<BTreeSet<_>>());
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
        assert_eq!(d.removed, ["Net"].into_iter().collect::<BTreeSet<_>>());
        assert!(!d.widened());
    }

    #[test]
    fn branded_capability_is_seen_through_and_refined() {
        // A `ConfigDir(Dir)` brand still confers `Dir` authority (sound), and the
        // brand name is reported as a refinement.
        let fp = footprint(
            "type ConfigDir:\n    ConfigDir(Dir)\npub fn load(c: ConfigDir) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, ["Dir"].into_iter().collect::<BTreeSet<_>>());
        let load = fp.entries.iter().find(|e| e.name == "load").unwrap();
        assert!(load.capabilities.contains("Dir"));
        assert!(load.brands.contains("ConfigDir"));
    }

    #[test]
    fn single_field_record_is_also_a_brand() {
        // A one-field record wrapping a capability is a brand too, not just a
        // positional newtype.
        let fp = footprint(
            "type ConfigDir:\n    dir: Dir\npub fn load(c: ConfigDir) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, ["Dir"].into_iter().collect::<BTreeSet<_>>());
        let load = fp.entries.iter().find(|e| e.name == "load").unwrap();
        assert!(load.brands.contains("ConfigDir"));
    }

    #[test]
    fn brands_resolve_transitively() {
        // A brand of a brand of `Net` still audits as `Net`.
        let fp = footprint(
            "type Raw:\n    Raw(Net)\ntype Api:\n    Api(Raw)\npub fn fetch(a: Api) -> Int:\n    0\n",
        );
        assert_eq!(fp.total, ["Net"].into_iter().collect::<BTreeSet<_>>());
    }

    #[test]
    fn any_capability_carrying_type_taints() {
        // Not just newtype brands: a record that holds a `Net` confers `Net` on a
        // caller, so the footprint must include it.
        let fp = footprint(
            "type Conn:\n    host: String\n    net: Net\npub fn open(c: Conn) -> Int:\n    0\n",
        );
        assert!(fp.total.contains("Net"));
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
        assert_eq!(d.added, ["Net"].into_iter().collect::<BTreeSet<_>>());
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
        assert!(fp.total.contains("Console"));
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
            "show", "json", "url", "duration", "random", "regex",
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
        let net_only = ["http", "server"];
        for name in net_only {
            let src = crate::linker::std_source(name).expect("bundled module");
            let fp = footprint(src);
            assert_eq!(
                fp.total,
                ["Net"].into_iter().collect::<BTreeSet<_>>(),
                "networking module `{name}` should require only Net",
            );
        }
    }
}
