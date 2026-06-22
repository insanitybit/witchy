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

    // RFC-0003: the address-pattern matcher shared by both backends. Exact match,
    // port wildcard, and IPv4 CIDR — generalizing the old exact-string allowlist.
    #[test]
    fn address_matcher_patterns() {
        assert!(address_admits("10.0.0.5:6379", "10.0.0.5:6379"));
        assert!(!address_admits("10.0.0.5:6379", "10.0.0.6:6379"));
        assert!(!address_admits("10.0.0.5:6379", "10.0.0.5:6380"));
        assert!(address_admits("10.0.0.5:*", "10.0.0.5:6379"));
        assert!(!address_admits("10.0.0.5:*", "10.0.0.6:1"));
        assert!(address_admits("10.0.0.0/24:6379", "10.0.0.5:6379"));
        assert!(address_admits("10.0.0.0/24:6379", "10.0.0.255:6379"));
        assert!(!address_admits("10.0.0.0/24:6379", "10.0.1.0:6379"));
        assert!(!address_admits("10.0.0.0/24:6379", "10.0.0.5:80"));
        assert!(address_admits("10.0.0.0/24:*", "10.0.0.5:80"));
        assert!(address_admits("0.0.0.0/0:*", "203.0.113.7:443"));
        assert!(address_admits("api.example.com:443", "api.example.com:443"));
        assert!(!address_admits("10.0.0.0/24:*", "api.example.com:443"));
        assert!(net_allows(
            &["127.0.0.1:80".to_string(), "10.0.0.0/8:*".to_string()],
            "10.1.2.3:6379"
        ));
        assert!(!net_allows(&["127.0.0.1:80".to_string()], "127.0.0.1:81"));
    }

    // The "root grant is always concrete" check shared by both backends: a bare
    // `Secret` needs a key (it IS the key); an empty `Net`/`SecretStore` is a real
    // capability and is always grantable.
    #[test]
    fn main_secret_grant_is_concrete() {
        let params_of = |src: &str| {
            parse_module(src)
                .expect("parse")
                .items
                .iter()
                .find_map(|it| match it {
                    crate::ast::Item::Function(f) if f.name == "main" => Some(f.params.clone()),
                    _ => None,
                })
                .expect("main")
        };
        let secret = params_of("fn main(console: Console, signing: Secret):\n    print(console, \"x\")\n");
        assert!(unmintable_main_cap(&secret, false).is_some(), "no key → refuse");
        assert!(unmintable_main_cap(&secret, true).is_none(), "key → grantable");
        // SecretStore and Net are real even when empty — never refused.
        let store = params_of("fn main(console: Console, store: SecretStore):\n    print(console, \"x\")\n");
        assert!(unmintable_main_cap(&store, false).is_none());
        let net = params_of("fn main(console: Console, net: Net):\n    print(console, \"x\")\n");
        assert!(unmintable_main_cap(&net, false).is_none());
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

    /// Supply-chain regression net: the bundled std modules must keep the
    /// capability footprints they advertise. The pure modules stay pure (empty
    /// footprint), and only the networking modules require authority — exactly
    /// `Net`, never `Console`/`Dir`. If a future change slips a capability param
    /// into, say, `list`, this fails loudly.
    #[test]
    fn std_module_footprints_are_pinned() {
        let pure = [
            "list", "string", "math", "option", "result", "func", "cmp", "ascii", "set",
            "json", "url", "duration", "random", "regex", "compiler", "toml", "semver",
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
        // `show` is pure except for `say` (the Show-accepting `print`), which
        // takes a `Console` — so the module's surface demands exactly that (the
        // `Show` trait, its impls, and `show_list` stay capability-free).
        let show = footprint(crate::linker::std_source("show").expect("bundled module"));
        assert_eq!(
            show.total.keys().copied().collect::<Vec<_>>(),
            vec!["Console"],
            "show's only capability demand is Console (for say)",
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
        // `exec` takes an `Exec` (to spawn) plus a `Dir[Read]` (to name and
        // confine the executable) — exactly those two, nothing ambient.
        let exec_fp = footprint(crate::linker::std_source("exec").expect("bundled module"));
        assert_eq!(
            exec_fp.total.keys().copied().collect::<Vec<_>>(),
            vec!["Dir", "Exec"],
            "exec requires exactly Dir + Exec",
        );
    }
