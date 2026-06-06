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

use std::collections::BTreeSet;

use crate::ast::{Item, Module, Param, Type};

/// The host capabilities the runtime grants at an entry point. (`Subject`, an
/// actor handle from `spawn`, is intra-program authority, not host authority,
/// so it isn't a supply-chain footprint concern.)
pub const HOST_CAPABILITIES: &[&str] = &["Console", "Dir", "Net"];

fn capability_of(ty: &Type) -> Option<&'static str> {
    if let Type::Named(name, _) = ty {
        return HOST_CAPABILITIES
            .iter()
            .copied()
            .find(|cap| *cap == name.as_str());
    }
    None
}

fn param_capabilities(params: &[Param]) -> BTreeSet<&'static str> {
    params
        .iter()
        .filter_map(|p| p.ty.as_ref().and_then(capability_of))
        .collect()
}

/// One entry point and the host capabilities it requires.
pub struct Entry {
    pub name: String,
    pub capabilities: BTreeSet<&'static str>,
}

/// A module's capability footprint: each entry point's requirements and the
/// union across all of them (the maximum host authority the module can wield).
pub struct Footprint {
    pub entries: Vec<Entry>,
    pub total: BTreeSet<&'static str>,
}

/// What changed between two versions of a module's footprint. `added` is a
/// *widening* — host authority the newer version demands that the older did not
/// (e.g. a dependency update that suddenly asks for `Net`); `removed` is a
/// narrowing, which is always safe. The supply-chain gate blocks on widening.
pub struct FootprintDiff {
    pub added: BTreeSet<&'static str>,
    pub removed: BTreeSet<&'static str>,
}

impl FootprintDiff {
    /// Whether the newer footprint demands authority the older one did not. This
    /// is the signal the install/CI gate fails on: new authority must be an
    /// explicit, reviewed decision, never something a version bump slips in.
    pub fn widened(&self) -> bool {
        !self.added.is_empty()
    }
}

/// Compare two footprints by their total authority — the primitive behind the
/// block-on-widening gate. Because capabilities are unforgeable and only enter
/// through parameters, a module cannot gain authority without changing a public
/// entry point's signature, so this total-level diff fully captures a widening.
pub fn diff(old: &Footprint, new: &Footprint) -> FootprintDiff {
    FootprintDiff {
        added: new.total.difference(&old.total).copied().collect(),
        removed: old.total.difference(&new.total).copied().collect(),
    }
}

pub fn analyze(module: &Module) -> Footprint {
    let mut entries = Vec::new();
    let mut total = BTreeSet::new();
    for item in &module.items {
        let (name, capabilities) = match item {
            // Public functions and `main` are how authority enters the module.
            Item::Function(f) if f.public || f.name == "main" => {
                (f.name.clone(), param_capabilities(&f.params))
            }
            // An actor's capability-typed fields without an initializer are
            // granted at spawn.
            Item::Actor(a) => {
                let caps = a
                    .fields
                    .iter()
                    .filter(|fl| fl.init.is_none())
                    .filter_map(|fl| capability_of(&fl.ty))
                    .collect();
                (format!("actor {}", a.name), caps)
            }
            _ => continue,
        };
        total.extend(capabilities.iter().copied());
        entries.push(Entry { name, capabilities });
    }
    Footprint { entries, total }
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
            "show", "json", "url", "duration", "random",
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
