use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// `Rand` follows the same receiver-method shape as other capabilities:
    /// `rand.hex(n)` lowers to `rand.hex(rand, n)`, without needing the ambiguous
    /// double-receiver spelling `rand.hex(rand, n)`.
    #[test]
    fn rand_capability_supports_std_method_syntax() {
        let src = "import rand\n\nfn main(console: Console, rand: Rand):\n    let token = rand.hex(4)\n    console.print(\"${token.length()}\")\n";
        let expected = ["8"];
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );

        use crate::runtime::{Capabilities, Runtime};
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    rand: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), expected, "wasm");
    }

    /// (RFC-0047) `==` on a capability type is a compile-time error — capabilities
    /// are authority, not data. Direct and nested-in-a-container both error.
    #[test]
    fn capability_equality_is_a_compile_error() {
        let direct = "fn main(console: Console):\n    console.print(\"${console == console}\")\n";
        let e = typeck::check_str(direct).expect_err("`console == console` must be rejected");
        assert!(e.contains("not defined on capability types"), "teaching error, got: {e}");
        let in_tuple = "fn main(console: Console):\n    console.print(\"${(console, 1) == (console, 1)}\")\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("cap in a tuple must be rejected")
                .contains("not defined on capability types"),
            "a capability nested in a tuple must be rejected too"
        );
        let in_sum = "type Resource:\n    Missing\n    Opened(Dir[Read])\n\nfn same(a: Resource, b: Resource) -> Bool:\n    a == b\n";
        assert!(
            typeck::check_str(in_sum).expect_err("cap in a nominal sum must be rejected")
                .contains("not defined on capability types"),
            "a capability nested in a GC-lowered sum must remain non-comparable"
        );
    }

    /// RFC-0005 Stage 4 (sum slice): a non-generic nominal sum that carries a
    /// migrated capability uses one tagged GC struct with disjoint per-variant
    /// field bands. Wrong-variant patterns must test the tag before touching an
    /// inactive (possibly null) reference field. Recursive and mutually
    /// recursive sums use the same Wasm GC recursion group.
    #[test]
    fn capability_sum_runs_on_tagged_gc_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_capsum_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-sum").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = r#"
type Resource:
    Missing(String)
    Opened(Dir[Read], String)
    Count(Int)
    Ratio(Float)
    Pair(String, String)

type FrozenHolder:
    FrozenHolder(frozen Resource)

type Tree:
    Empty
    Leaf(Dir[Read], String)
    Branch(Tree, Tree)

type Outer:
    OuterEmpty
    OuterInner(Inner)

type Inner:
    InnerCap(Dir[Read], String)
    InnerOuter(Outer)

fn resource_label(r: Resource) -> String:
    match r:
        Opened(_, name) -> "opened: " + name
        Missing(name) -> "missing: " + name
        Count(n) -> "count: ${n}"
        Ratio(x) -> "ratio: ${x}"
        Pair(a, b) -> "pair: ${a}:${b}"

fn keep_resource(r: Resource) -> Resource:
    return r

fn keep_qualified(r: frozen Resource) -> frozen Resource:
    return r

fn unwrap_frozen(h: FrozenHolder) -> Resource:
    match h:
        FrozenHolder(r) -> r

fn mark(console: Console, label: String) -> String:
    console.print(label)
    label

fn load_tree(t: Tree) -> String:
    match t:
        Leaf(dir, name) -> dir.read(name)
        Branch(Leaf(_, _), _) -> "wrong branch"
        Branch(Empty, Leaf(dir, name)) -> dir.read(name)
        Branch(_, _) -> "other branch"
        Empty -> "empty"

fn load_tree_or(t: Tree) -> String:
    match t:
        Leaf(dir, name) | Branch(_, Leaf(dir, name)) -> dir.read(name)
        _ -> "or-empty"

fn load_outer(o: Outer) -> String:
    match o:
        OuterInner(InnerCap(dir, name)) -> dir.read(name)
        OuterInner(InnerOuter(_)) -> "nested outer"
        OuterEmpty -> "empty outer"

fn main(console: Console, root: Dir[Read]):
    console.print(resource_label(keep_resource(Missing("absent"))))
    console.print(resource_label(keep_qualified(Missing("qualified"))))
    console.print(resource_label(unwrap_frozen(FrozenHolder(Missing("field")))))
    console.print(resource_label(Count(922337203685477580)))
    console.print(resource_label(Pair(mark(console, "left"), mark(console, "right"))))
    console.print(load_tree(Empty))
    console.print(load_tree(Branch(Empty, Leaf(root, "greeting.txt"))))
    console.print(load_tree_or(Empty))
    console.print(load_tree_or(Branch(Empty, Leaf(root, "greeting.txt"))))
    console.print(load_outer(OuterInner(InnerCap(root, "greeting.txt"))))
"#;
        let want = vec![
            "missing: absent".to_string(),
            "missing: qualified".to_string(),
            "missing: field".to_string(),
            "count: 922337203685477580".to_string(),
            "left".to_string(),
            "right".to_string(),
            "pair: left:right".to_string(),
            "empty".to_string(),
            "hello-sum".to_string(),
            "or-empty".to_string(),
            "hello-sum".to_string(),
            "hello-sum".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying sums");
        let bin_again = codegen::compile_module_binary(&linked)
            .expect_lowered("the same module still lowers");
        assert_eq!(bin_again, bin, "GC aggregate IDs and binary output must be deterministic");
        let mut rt = Runtime::batch().expect("runtime");
        let caps = Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn(&bin, caps, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0005 Stage 4 (tuple slice): a fully concrete tuple that transitively
    /// carries a migrated capability uses a deterministic typed GC-struct
    /// layout. This covers the direct ABI, numeric projection, `let` and
    /// `match` patterns, nested tuples, and tuples stored inside nominal GC
    /// aggregates without ever routing the authority through an i64 slot.
    #[test]
    fn capability_tuple_runs_on_gc_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_captuple_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-tuple").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = r#"
type ReadPair = (Dir[Read], String, Int)
type LabeledPair(a) = (Dir[Read], a, Int)

type Holder:
    Holder((Dir[Read], (String, Int)))

type Packet:
    Empty
    Packed((Dir[Read], (String, Int)))

fn keep(pair: (Dir[Read], String, Int)) -> (Dir[Read], String, Int):
    return pair

fn keep_alias(pair: ReadPair) -> ReadPair:
    pair

fn keep_generic_alias(pair: LabeledPair(String)) -> LabeledPair(String):
    pair

fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn project(pair: (Dir[Read], String, Int)) -> String:
    read_named(pair.0, pair.1) + ":${pair.2}"

fn destructure(pair: (Dir[Read], String, Int)) -> String:
    let (dir, name, count) = pair
    dir.read(name) + ":${count}"

fn choose(pair: (Dir[Read], String, Int)) -> String:
    match pair:
        (dir, name, count) -> dir.read(name) + ":${count}"

fn keep_qualified(pair: (frozen Dir[Read], String)) -> (frozen Dir[Read], String):
    pair

fn qualified(pair: (frozen Dir[Read], String)) -> String:
    let (dir, name) = pair
    dir.read(name)

fn optional(pair: (Option(Dir[Read]), String)) -> String:
    match pair:
        (Some(dir), name) -> dir.read(name)
        (None, _) -> "none"

fn select(root: Dir[Read], labels: List(String)) -> (Dir[Read], String, Int):
    match list.at(labels, 0):
        "first" -> (root, "greeting.txt", 4)
        _ -> (root, "greeting.txt", 5)

fn nested(holder: Holder) -> String:
    match holder:
        Holder((dir, (name, count))) -> dir.read(name) + ":${count}"

fn packed(packet: Packet) -> String:
    match packet:
        Packed((dir, (name, count))) -> dir.read(name) + ":${count}"
        Empty -> "empty"

fn main(console: Console, root: Dir[Read]):
    let pair = keep((root, "greeting.txt", 1))
    console.print(project(pair))
    console.print(destructure(pair))
    console.print(choose(pair))
    console.print(project(keep_alias((root, "greeting.txt", 6))))
    console.print(project(keep_generic_alias((root, "greeting.txt", 7))))
    console.print(qualified(keep_qualified((root, "greeting.txt"))))
    console.print(optional((Some(root), "greeting.txt")))
    console.print(optional((None, "greeting.txt")))
    console.print(project(select(root, ["first"])))
    console.print(nested(Holder((root, ("greeting.txt", 2)))))
    console.print(packed(Empty))
    console.print(packed(Packed((root, ("greeting.txt", 3)))))
"#;
        let want = vec![
            "hello-tuple:1".to_string(),
            "hello-tuple:1".to_string(),
            "hello-tuple:1".to_string(),
            "hello-tuple:6".to_string(),
            "hello-tuple:7".to_string(),
            "hello-tuple".to_string(),
            "hello-tuple".to_string(),
            "none".to_string(),
            "hello-tuple:4".to_string(),
            "hello-tuple:2".to_string(),
            "empty".to_string(),
            "hello-tuple:3".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying tuples");
        let bin_again = codegen::compile_module_binary(&linked)
            .expect_lowered("the same tuple module still lowers");
        assert_eq!(bin_again, bin, "GC tuple IDs and binary output must be deterministic");
        let mut rt = Runtime::batch().expect("runtime");
        let caps = Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn(&bin, caps, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0005 Stage 3: a named sealed capability record can carry a migrated
    /// `Net` externref alongside ordinary data. The compiled backend lowers the
    /// record to a typed GC struct, so the carried authority never passes through
    /// the i64 slot/linear-memory representation.
    #[test]
    fn carried_state_capability_record_runs_on_gc_struct_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "capability Postgres:\n    net: Net[Connect, Tcp]\n    table: String\n\nfn connect(net: Net[Connect, Tcp]) -> Postgres:\n    Postgres(net, \"public\")\n\nfn use_table(pg: Postgres, name: String) -> Postgres:\n    match pg:\n        Postgres(net, _) -> Postgres(net, name)\n\nfn count_rows(pg: Postgres, requested: String) -> String:\n    match pg:\n        Postgres(_, table) ->\n            if requested == table:\n                \"ok: counted rows in \" + requested\n            else:\n                \"denied: \" + requested + \" is outside this handle (scoped to \" + table + \")\"\n\nfn main(console: Console, net: Net):\n    let users = use_table(connect(net), \"users\")\n    console.print(count_rows(users, \"users\"))\n    console.print(count_rows(users, \"secrets\"))\n";
        let want = vec![
            "ok: counted rows in users".to_string(),
            "denied: secrets is outside this handle (scoped to users)".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying records to GC structs");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bin,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
    }

    /// Host-capability operations are reachable via UFCS method syntax: `console.print(x)`
    /// lowers to the bare intrinsic `console.print(x)` — the same surface a library
    /// capability's own `impl` methods already get. The foundation for RFC-0011's
    /// "refinement is a method" model (`net.only(...)`, `dir.subtree(...)`). The method
    /// and free-function forms must agree on both backends.
    #[test]
    fn host_capability_ufcs_method_calls() {
        let src = "fn main(console: Console):\n    console.print(\"a\")\n    console.print(\"b\")\n";
        let expected = ["a", "b"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // The refinement verb `net.only(...)` (method) / `only(...)` (free) is exercised on
        // both backends by `net_only_refinement_verb_backends_agree` below.
    }

    #[test]
    fn capability_is_sealed_across_modules() {
        // RFC-0002: `capability Conn from Net` is a SEALED brand — it may be
        // constructed or destructured only in its declaring module (`redis`).
        use crate::pipeline::link;
        use crate::parser::parse_module;
        let lib = "capability Conn from Net[Connect, Tcp]\npub fn open(net: Net[Connect, Tcp]) -> Conn:\n    Conn(net)\npub fn ping(c: Conn) -> Int:\n    match c:\n        Conn(net) -> 1\n";
        let mods = |app: &str| {
            vec![
                ("redis".to_string(), parse_module(lib).expect("lib parse")),
                ("app".to_string(), parse_module(app).expect("app parse")),
            ]
        };
        // Forging the sealed cap in another module is rejected.
        let forge = "import redis\nfn main(console: Console, net: Net):\n    let c = Conn(net)\n    console.print(\"${redis.ping(c)}\")\n";
        let e = format!("{:?}", link(mods(forge), "app").expect_err("forge must be rejected"));
        assert!(e.contains("sealed capability") && e.contains("construct"), "{e}");
        // Unwrapping (destructuring) it in another module is rejected too.
        let unwrap = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    match c:\n        Conn(n) -> console.print(\"x\")\n";
        let e2 = format!("{:?}", link(mods(unwrap), "app").expect_err("unwrap must be rejected"));
        assert!(e2.contains("destructure"), "{e2}");
        // The legitimate path — mint via the library, then use it — links fine.
        let ok = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    console.print(\"${redis.ping(c)}\")\n";
        assert!(link(mods(ok), "app").is_ok(), "legit mint-then-use must link");
        // A module can construct/destructure its OWN sealed capability.
        assert!(parse_module(lib).is_ok());
    }

    /// `now` (Clock) and `get_env` (Env) compile to capability-gated host
    /// imports. `get_env` is deterministic given the process env, so both
    /// backends must agree exactly; `now` is wall-clock, so each backend is
    /// checked for plausibility instead. Also exercises a multi-capability
    /// `main` (Console + Env / Console + Clock), which codegen now accepts.
    #[test]
    fn clock_and_env_compile_to_wasm_and_agree() {
        let host_path = std::env::var("PATH").expect("the test process has PATH");
        let env_src = "import option\n\nfn main(console: Console, env: Env):\n    match env.get_env(\"PATH\"):\n        Some(v) -> console.print(\"got: \" + v)\n        None -> console.print(\"unset\")\n    match env.get_env(\"WITCHY_E2E_DEFINITELY_UNSET\"):\n        Some(v) -> console.print(\"got: \" + v)\n        None -> console.print(\"unset\")\n";
        let want = vec![format!("got: {host_path}"), "unset".to_string()];
        let module = parser::parse_module(env_src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert_eq!(link_run(env_src), want.clone(), "interpreter");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm"), want, "compiled WASM must agree");

        // The clock: both backends must yield a plausible epoch-milliseconds.
        let clock_src = "fn main(console: Console, clock: Clock):\n    console.print(if clock.now() > 1500000000000: \"plausible\" else: \"implausible\")\n";
        assert_eq!(interp(clock_src), vec!["plausible"], "interpreter");
        assert_eq!(run_on_wasm(clock_src), vec!["plausible"], "compiled WASM");
    }

    /// The full Dir family compiles to capability-gated host imports and agrees
    /// with the interpreter: read/exists/is_dir/subdir/write/make_dir/list all
    /// round-trip in a confined temp directory, and escape attempts (`..`,
    /// absolute paths) FAIL on both backends.
    #[test]
    fn dir_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        let src = "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"a.txt\"))\n    console.print(\"${dir.exists(\"a.txt\")}\")\n    console.print(\"${dir.exists(\"missing.txt\")}\")\n    let sub = dir.subtree(\"sub\")\n    console.print(sub.read(\"b.txt\"))\n    dir.write(\"out.txt\", \"written\")\n    console.print(dir.read(\"out.txt\"))\n    dir.make_dir(\"made\")\n    console.print(\"${dir.is_dir(\"made\")}\")\n    for name in dir.list():\n        console.print(\"entry: \" + name)\n";
        let want = vec![
            "alpha".to_string(),
            "true".to_string(),
            "false".to_string(),
            "beta".to_string(),
            "written".to_string(),
            "true".to_string(),
            "entry: a.txt".to_string(),
            "entry: made".to_string(),
            "entry: out.txt".to_string(),
            "entry: sub".to_string(),
        ];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    dir_write: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        for bad in ["../outside.txt", "/etc/hosts"] {
            let esc = format!(
                "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"{bad}\"))\n"
            );
            assert!(interpreter::run_in(&esc, &root).is_err(), "interp must reject `{bad}`");
            let m = parser::parse_module(&esc).expect("parse");
            let wbytes = codegen::compile_module_binary(&m)
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut a = rt
                .spawn(
                    &wbytes,
                    Capabilities {
                        print: true,
                        quiet: true,
                        dir_root: Some(root.clone()),
                        dir_read: true,
                        dir_write: true,
                        ..Default::default()
                    },
                    64,
                )
                .expect("spawn");
            assert!(a.run().is_err(), "WASM must trap on `{bad}`");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: `dir.only(Dir.ext(...))` confines a `Dir` to an ENTRY policy —
    /// reading a matching extension is allowed, a non-matching one is refused at the
    /// policy check — identically on both backends.
    #[test]
    fn dir_only_ext_policy_confines_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dirpol_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("ok.txt"), "hello").expect("seed txt");
        std::fs::write(root.join("secret.key"), "TOPSECRET").expect("seed key");
        let root_str = root.to_str().expect("utf8 root").to_string();

        let caps = || Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        };

        // Allowed: read a `.txt` through a Dir narrowed to `ext(".txt")`.
        let ok_src = "fn main(console: Console, dir: Dir):\n    let txt = dir.only(Dir.ext(\".txt\"))\n    console.print(txt.read(\"ok.txt\"))\n";
        let want = vec!["hello".to_string()];
        assert_eq!(
            interpreter::run_module(resolve_std_src(ok_src), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(ok_src))
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // Denied: a `.key` through the same narrowed Dir is refused on both backends.
        let bad_src = "fn main(console: Console, dir: Dir):\n    let txt = dir.only(Dir.ext(\".txt\"))\n    console.print(txt.read(\"secret.key\"))\n";
        assert!(
            interpreter::run_module(resolve_std_src(bad_src), &root_str, Vec::new()).is_err(),
            "interp must refuse a .key",
        );
        let bbytes = codegen::compile_module_binary(&resolve_std_src(bad_src))
            .expect_lowered("the binary path lowers this program");
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2.spawn(&bbytes, caps(), 64).expect("spawn");
        assert!(a.run().is_err(), "WASM must refuse a .key");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: the `kind:` Dir entry policy. `dir.only(Dir.files())` admits a file
    /// read but DENIES opening a sub-directory; `dir.only(Dir.dirs())` is the mirror.
    /// An `ext`-only policy still traverses (kind gates directories, ext gates file names),
    /// so `kind` is additive and backward-compatible — all identical on both backends.
    #[test]
    fn dir_kind_policy_confines_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dirkind_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("ok.txt"), "hello").expect("seed txt");
        let root_str = root.to_str().expect("utf8 root").to_string();

        let caps = || Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        };
        // Assert BOTH backends produce `want`.
        let ok_both = |src: &str, want: Vec<String>| {
            assert_eq!(
                interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).expect("interp"),
                want,
                "interp: {src}",
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
            actor.run().expect("run");
            assert_eq!(actor.output(), want, "wasm: {src}");
        };
        // Assert BOTH backends REFUSE (the policy check trips identically).
        let err_both = |src: &str| {
            assert!(
                interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).is_err(),
                "interp should refuse: {src}",
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
            assert!(actor.run().is_err(), "wasm should refuse: {src}");
        };

        // `files()`: read a file OK; opening a sub-directory DENIED (the DoD headline).
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    console.print(d.read(\"ok.txt\"))\n",
            vec!["hello".to_string()],
        );
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    let s = d.subtree(\"sub\")\n    console.print(\"unreached\")\n");

        // `dirs()`: open a sub-directory OK; reading a file DENIED (the mirror).
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.dirs())\n    let s = d.subtree(\"sub\")\n    console.print(\"traversed\")\n",
            vec!["traversed".to_string()],
        );
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.dirs())\n    console.print(d.read(\"ok.txt\"))\n");

        // An `ext`-only policy still traverses — kind gates directories, ext gates files.
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    let s = d.subtree(\"sub\")\n    console.print(\"traversed\")\n",
            vec!["traversed".to_string()],
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: `dir.subtree(path)` is the method form of `subdir` — it narrows a
    /// `Dir` to a subtree identically on both backends, and the same `..`/absolute
    /// confinement applies. Mirrors `net.only(...)` as the host-primitive method form.
    #[test]
    fn dir_subtree_method_narrows_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_subtree_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        // Method form `dir.subtree("sub")` and chained `.subtree(...).subtree(...)`.
        std::fs::create_dir_all(root.join("sub/deep")).expect("mkdir deep");
        std::fs::write(root.join("sub/deep/c.txt"), "gamma").expect("seed c");
        let src = "fn main(console: Console, dir: Dir):\n    let s = dir.subtree(\"sub\")\n    console.print(s.read(\"b.txt\"))\n    console.print(s.subtree(\"deep\").read(\"c.txt\"))\n";
        let want = vec!["beta".to_string(), "gamma".to_string()];

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // The subtree is still confined: `..` from inside it escapes and FAILS.
        let esc = "fn main(console: Console, dir: Dir):\n    console.print(dir.subtree(\"sub\").read(\"../a.txt\"))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp rejects `..` from a subtree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0012: the `File` capability round-trips on BOTH backends. `dir.read_file`/
    /// `dir.write_file` navigate a `Dir` to a confined `File` leaf; `read(File)` /
    /// `write(File, data)` operate on it (no path arg), with the same `..`/absolute
    /// confinement as `Dir`. The compiled path uses the `file_read` WIR helper plus
    /// `dir_open`/`dir_create`/`file_write` host ops.
    #[test]
    fn file_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("note.txt"), "alpha").expect("seed");

        let src = "fn main(console: Console, dir: Dir):\n    console.print(dir.read_file(\"note.txt\").read())\n    let out = dir.write_file(\"out.txt\")\n    out.write(\"beta\")\n    console.print(dir.read_file(\"out.txt\").read())\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    dir_write: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // A `File` opened via navigation is still confined: `..` escapes and FAILS
        // on both backends.
        let esc = "fn main(console: Console, dir: Dir):\n    console.print(dir.read_file(\"../escape.txt\").read())\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp rejects `..` via open");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut a = rt
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on `..` via open");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0012: `main` may receive a `File` DIRECTLY (the `--file` grant) — the
    /// least-authority single-file case, with NO `Dir`. The i-th `File` param maps
    /// to the i-th grant on both backends (interpreter `file_grants` /
    /// `Capabilities::file_grants` + the pre-populated files table).
    #[test]
    fn file_main_grant_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_fmg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let a_txt = root.join("a.txt");
        let b_txt = root.join("b.txt");
        std::fs::write(&a_txt, "alpha").expect("seed a");
        std::fs::write(&b_txt, "beta").expect("seed b");

        // Two File params, mapped positionally to two grants; no Dir granted.
        let src = "fn main(console: Console, first: File[Read], second: File[Read]):\n    console.print(first.read())\n    console.print(second.read())\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let interp_out = interpreter::run_module_files(module, &root, vec![a_txt.clone(), b_txt.clone()])
            .expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    file_grants: vec![a_txt.clone(), b_txt.clone()],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `Exec` capability compiles to a capability-gated host import and
    /// agrees with the interpreter: a confined subprocess runs identically on
    /// both backends, returning the `"<code>\n<output>"` payload, and an
    /// executable outside the granted `Dir` subtree FAILS on both. (Unix-only —
    /// it spawns a shell script.)
    #[cfg(unix)]
    #[test]
    fn exec_capability_compiles_to_wasm_and_agrees() {
        use crate::runtime::{Capabilities, Runtime};
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("witchy_wasm_exec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        // A tiny deterministic program: echo its two args, then echo stdin.
        let script = root.join("greet");
        std::fs::write(&script, "#!/bin/sh\necho \"args=$1,$2\"\ncat\n").expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        // args "a\0b" -> argv [a, b]; stdin "hi". Payload: "0\nargs=a,b\nhi".
        let src = "fn main(console: Console, runner: Exec, dir: Dir):\n    console.print(runner.exec(dir, \"greet\", \"a\\0b\", \"hi\"))\n";

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    exec: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        // The parity invariant: byte-identical output on both backends.
        assert_eq!(interp_out, actor.output(), "exec must agree across backends");
        // And it actually ran the process.
        assert!(
            interp_out.join("\n").contains("args=a,b") && interp_out.join("\n").contains("hi"),
            "exec output should contain the subprocess result, got {interp_out:?}"
        );

        // An executable outside the granted subtree is rejected on both backends.
        let esc = "fn main(console: Console, runner: Exec, dir: Dir):\n    console.print(runner.exec(dir, \"../escape\", \"\", \"\"))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp must reject escape");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    exec: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on an escaping exec path");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `main` taking several `Dir` params gets several *distinct* grants —
    /// positional handles (the first `--dir` backs handle 0, the next handle 1)
    /// — identically on both backends. Reading from each confined subtree yields
    /// that subtree's file, and the two never cross. (RFC-0004 multi-Dir.)
    #[test]
    fn multi_dir_grants_are_positional_and_agree() {
        use crate::runtime::{Capabilities, Runtime};
        let base = std::env::temp_dir().join(format!("witchy_multidir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).expect("mkdir a");
        std::fs::create_dir_all(&dir_b).expect("mkdir b");
        std::fs::write(dir_a.join("f.txt"), "from-A").expect("seed a");
        std::fs::write(dir_b.join("f.txt"), "from-B").expect("seed b");

        // Both Dirs name `f.txt`, but each resolves within its own subtree.
        let src = "fn main(console: Console, da: Dir, db: Dir):\n    console.print(da.read(\"f.txt\"))\n    console.print(db.read(\"f.txt\"))\n";
        let want = vec!["from-A".to_string(), "from-B".to_string()];

        let interp_out =
            interpreter::run_in_dirs(src, &[dir_a.clone(), dir_b.clone()]).expect("interp");
        assert_eq!(interp_out, want, "interpreter multi-dir");

        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(dir_a.clone()),
                    dir_roots: vec![dir_b.clone()],
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM multi-dir must agree");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Rights are enforced at the GRANT: a module that imports a write operation
    /// cannot even instantiate under a read-only Dir grant, and any Dir import
    /// fails with no grant at all.
    #[test]
    fn dir_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_rights_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let writer = "fn main(console: Console, dir: Dir):\n    dir.write(\"x.txt\", \"data\")\n    console.print(\"wrote\")\n";
        let module = parser::parse_module(writer).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &bytes,
            Capabilities {
                print: true,
                quiet: true,
                dir_root: Some(root.clone()),
                dir_read: true,
                dir_write: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "write import must not instantiate under a read-only grant");
        let reader = "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"x.txt\"))\n";
        let m = parser::parse_module(reader).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "Dir import must not instantiate without a Dir grant");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The enforcement half: a module that imports `now`/`env_*` but was NOT
    /// granted Clock/Env must fail at instantiation — the host function simply
    /// is not linked, so the authority is structurally absent.
    #[test]
    fn ungranted_clock_and_env_fail_to_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let srcs = [
            "fn main(console: Console, clock: Clock):\n    console.print(\"${clock.now()}\")\n",
            "import option\n\nfn main(console: Console, env: Env):\n    match env.get_env(\"X\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n",
        ];
        for src in srcs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let denied = rt.spawn(
                &bytes,
                Capabilities { print: true, ..Default::default() },
                4,
            );
            assert!(denied.is_err(), "ungranted Clock/Env import must fail to instantiate");
        }
    }

    /// `std/rights` matches capability strings rights-precisely (the logic the pm
    /// check/gate and coven's publish enforcement share): a bare kind covers any
    /// rights of that kind, a bracketed one only a subset — so `Net[Connect]` does
    /// NOT cover full `Net`.
    #[test]
    fn rights_module_covers_capabilities_rights_precisely() {
        let src = r#"import rights

fn main(console: Console):
    console.print(yes(rights.covers("Net", "Net[Listen]")))
    console.print(yes(rights.covers("Net[Connect]", "Net")))
    console.print(yes(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    console.print(yes(rights.covers("Dir", "Console")))
    console.print(yes(rights.any_covers(["Console", "Dir[Read]"], "Dir[Read]")))
    console.print(list.join(rights.uncovered(["Net[Connect]"], ["Net", "Console"]), "|"))

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            // `Net[Connect, Tcp]` does not cover `Net[Connect]`: the demanded
            // type admits every Connect transport, while the declared type is
            // Tcp-only.
            vec!["y", "n", "n", "n", "y", "Net|Console"]
        );
    }

    /// The `Clock` capability yields wall-clock time (ms since epoch) via `now`.
    /// Reading the clock is ambient nondeterminism, so it's capability-gated and
    /// surfaces in the footprint — not a pure builtin.
    #[test]
    fn clock_capability_yields_wall_clock_time() {
        let out = interp(
            "fn main(console: Console, clock: Clock):\n    console.print(\"${clock.now()}\")\n",
        );
        let ms: i64 = out[0].parse().expect("now should print an integer");
        assert!(ms > 1_600_000_000_000, "now should be ms since the Unix epoch (got {ms})");
        // `now` needs a Clock — calling it with another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, clock: Clock):\n    let t = clock.now()\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// (RFC-0038) A bare grantable capability granted to `main` mints an identical
    /// sealed record on BOTH backends: the interpreter builds a `Value::Ctor` from
    /// the grant fields; the compiled backend stages each field host-side and
    /// wraps them in a record via `mk{N}`. The two must agree bit-for-bit.
    #[test]
    fn grantable_user_cap_mints_identically_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "grantable capability UiRoot:\n    policy: String\n    app_id: String\n\nfn descr(u: UiRoot) -> String:\n    match u:\n        UiRoot(p, a) -> p + \"@\" + a\n\nfn main(console: Console, ui: UiRoot):\n    console.print(descr(ui))\n";
        let expected = vec!["coven-web@web".to_string()];

        // Interpreter: grant keyed by param name -> field values.
        let module = parser::parse_module(src).expect("parse");
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "coven-web".to_string());
        fields.insert("app_id".to_string(), "web".to_string());
        let mut grants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        assert_eq!(
            interpreter::run_module_user_caps(module, ".", vec![], vec![], vec![], grants).expect("interp"),
            expected,
            "interp"
        );

        // Compiled: field values staged host-side in declaration order.
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    user_cap_fields: vec![vec!["coven-web".to_string(), "web".to_string()]],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), expected, "compiled WASM must agree");
    }

    /// The `Env` capability reads process environment variables via `get_env`,
    /// returning `Option(String)` (None when unset). Reading the environment is
    /// ambient authority, so it's capability-gated and surfaces in the footprint.
    #[test]
    fn env_capability_reads_environment_variables() {
        // A definitely-unset variable yields None.
        let out = interp(
            "fn main(console: Console, env: Env):\n    match env.get_env(\"WITCHY_NOPE_UNSET_VAR\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n",
        );
        assert_eq!(out, vec!["unset"]);
        // `get_env` needs an Env capability — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let x = get_env(c, \"X\")\n").is_err());
        // The Env requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, env: Env):\n    let x = env.get_env(\"X\")\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Env"), "Env should appear in the footprint");
    }

    /// The sandbox grants exactly the computed footprint: a program combining
    /// argv, Env, and a read-only Dir (minigrep's shape) runs confined, and its
    /// Int-returning `main` becomes the exit code rather than an output line.
    #[test]
    fn sandbox_grants_full_footprint() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_fp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data.txt"), "needle in here\nnothing\n").unwrap();
        let src_path = root.join("prog.witchy");
        std::fs::write(
            &src_path,
            "import option\n\nfn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:\n    let path = list.at(args, 0)\n    let label = match env.get_env(\"PATH\"):\n        Some(v) -> v\n        None -> \"unlabeled\"\n    for line in dir.read(path).lines():\n        if line.contains(\"needle\"):\n            console.print(label + \": \" + line)\n    0\n",
        )
        .unwrap();
        let host_path = std::env::var("PATH").expect("the test process has PATH");
        let (out, exit) = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            vec![root.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec!["data.txt".to_string()],
            None,
            Vec::new(),
            witchy_confinement::EnforcementMode::Disabled,
        )
        .expect("sandbox run");
        assert_eq!(out, vec![format!("{host_path}: needle in here")]);
        assert_eq!(exit, Some(0), "Int-returning main becomes the exit code");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #35 keystone: a WIR-native prelude helper (vs the raw-body "all features
    /// on" prelude) yields a CAPABILITY-MINIMAL module — it imports only the
    /// authority the reached helpers need. A module whose only helper is
    /// `print_str` imports only `print`, so it instantiates and runs under a
    /// print-ONLY grant. (Were it the raw-body prelude, it would import
    /// crypto.sign/dir/net/… and fail to instantiate here.) This proves the
    /// incremental WIR-helper path that unblocks the M3 flip.
    #[test]
    fn wir_native_helper_yields_capability_minimal_module() {
        use witchy_wir::wir::{
            DataSegment, Kind, WirExpr, WirFunc, WirImport, WirModule, WirNode,
        };
        use witchy_wir::wir_helpers::print_str_helper;
        // Intern "hello" at offset 1024: [i32 len=5]["hello"].
        let off = 1024u32;
        let text = "hello";
        let mut bytes = (text.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(text.as_bytes());

        let main = WirFunc {
            name: "main".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::Call {
                func: "print_str".into(),
                args: vec![WirExpr::StrPtr(off)],
            })],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::Call { func: "main".into(), args: vec![] })],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![print_str_helper(), main, run],
            memory_pages: 1,
            data: vec![DataSegment { offset: off, bytes }],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let wasm = witchy_wir::wir_encode::encode(&module, &[]);
        assert!(validates_wasm_gc(&wasm), "encoded module must validate");

        // Run with ONLY `print` granted — nothing else. Success proves the module
        // imports no other authority (else instantiate would fail).
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &wasm,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn under a print-only grant");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["hello".to_string()]);
    }

    /// The capability thesis at the WASM boundary: without the `print_int` host
    /// function granted, the compiled module imports something that isn't there
    /// and cannot even instantiate.
    #[test]
    fn compiled_program_without_capability_cannot_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let module = parser::parse_module(include_str!("../../examples/compute/src/compute.witchy")).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let result = rt.spawn(&bytes, Capabilities::none(), 4);
        assert!(result.is_err(), "ungranted module must fail to instantiate");
    }

    #[test]
    fn files_example_reads_sandboxed_file() {
        assert_eq!(
            crate::execute_file("examples/files/src/files.witchy", Vec::new()).unwrap(),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    /// The capability-rights showcase: it runs (exercising implicit + explicit
    /// `as` narrowing of a `Dir` to `Dir[Read]`) and its footprint is
    /// verb/transport-precise — the end-to-end demonstration of the feature.
    #[test]
    fn capability_rights_example_runs_and_audits() {
        assert_eq!(
            crate::execute_file("examples/capability_rights/src/capability_rights.witchy", Vec::new()).unwrap(),
            vec![
                "implicit: hello from a sandboxed Dir capability",
                "explicit: hello from a sandboxed Dir capability",
            ]
        );
        let src = std::fs::read_to_string("examples/capability_rights/src/capability_rights.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        let shown = |name: &str| {
            let e = fp.entries.iter().find(|e| e.name == name).expect("entry");
            crate::capabilities::show_caps(&e.capabilities)
        };
        assert_eq!(shown("load"), "Dir[Read]");
        assert_eq!(shown("fetch"), "Net[Connect, Tcp]");
        assert_eq!(shown("serve"), "Net[Listen]");
    }

    #[test]
    fn files_example_reads_through_capability() {
        // Run from the crate root so examples/data/greeting.txt resolves.
        assert_eq!(
            interp(include_str!("../../examples/files/src/files.witchy")),
            vec!["hello from a sandboxed Dir capability"]
        );
    }
