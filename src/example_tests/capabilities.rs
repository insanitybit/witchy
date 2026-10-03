use super::*;
use crate::{codegen, interpreter, parser, typeck};

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

    /// RFC-0011: the `kind:` Dir entry policy. `dir.only(Dir.files())` admits a file
    /// read but DENIES opening a sub-directory; `dir.only(Dir.dirs())` is the mirror.
    /// An `ext`-only policy still traverses (kind gates directories, ext gates file names),
    /// so `kind` is additive and backward-compatible — all identical on both backends.
    #[test]
    fn dir_kind_policy_confines_on_both_backends() {
        if !is_nightly() {
            return;
        }
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dirkind_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir sub");
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
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read(\"ok.txt\"))\n",
            vec!["hello".to_string()],
        );
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read(\"secret.key\"))\n");

        // SEC-056/062: policy uses the actual no-follow entry identity. A files-only
        // handle cannot rename/chmod a directory, and an admitted extension alias
        // cannot lead to a denied file on either backend.
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    d.rename(\"sub\", \"moved\")\n");
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    d.set_executable(\"sub\")\n");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("secret.key", root.join("alias.txt"))
                .expect("seed in-root alias");
            std::os::unix::fs::symlink("alias.txt", root.join("multi.txt"))
                .expect("seed multi-hop alias");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read(\"alias.txt\"))\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read(\"multi.txt\"))\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    let b = d.read_bytes(\"alias.txt\")\n    console.print(\"${b.length()}\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read_file(\"alias.txt\").read())\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.append(\"alias.txt\", \"changed\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.replace(\"alias.txt\", \"changed\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(\"${d.create_new(\"alias.txt\", \"changed\")}\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.set_executable(\"alias.txt\")\n");
            assert_eq!(std::fs::read_to_string(root.join("secret.key")).unwrap(), "TOPSECRET");
        }
        std::fs::write(root.join("classified.TXT"), "case-private").expect("seed case alias");
        if root.join("classified.txt").exists() {
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(d.read(\"classified.txt\"))\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.write(\"classified.txt\", \"changed\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.append(\"classified.txt\", \"changed\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    d.replace(\"classified.txt\", \"changed\")\n");
            err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(\"${d.create_new(\"classified.txt\", \"changed\")}\")\n");
            assert_eq!(
                std::fs::read_to_string(root.join("classified.TXT")).unwrap(),
                "case-private",
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn narrowed_dir_hides_denied_entries_from_lists_and_named_probes() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dir_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::create_dir_all(root.join("private-dir")).expect("mkdir private");
        std::fs::write(root.join("public.txt"), "public").expect("seed public");
        let hidden = "550e8400-e29b-41d4-a716-446655440000.key";
        std::fs::write(root.join(hidden), "secret").expect("seed secret");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    console.print(\"${d.exists(\"550e8400-e29b-41d4-a716-446655440000.key\")}\")\n    console.print(\"${d.is_dir(\"private-dir\")}\")\n    let files = dir.only(Dir.files())\n    console.print(\"${files.exists(\"private-dir\")}\")\n    console.print(\"${files.is_dir(\"private-dir\")}\")\n    let dirs = dir.only(Dir.dirs())\n    console.print(\"${dirs.exists(\"public.txt\")}\")\n    for name in d.list():\n        console.print(name)\n";
        let expected = vec![
            "false".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "false".to_string(),
            "private-dir".to_string(),
            "public.txt".to_string(),
        ];

        assert_eq!(
            interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).expect("interp"),
            expected,
            "interpreter hides policy-denied entry metadata",
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(src))
            .expect_lowered("probe lowers");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt.spawn(&bytes, Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        }, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), expected, "Wasm hides policy-denied entry metadata");
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

    /// SEC-080: a capability-free dependency must not corrupt a path retained
    /// by its caller and thereby steer the caller's `Dir[Read]` authority. The
    /// vulnerable optimizer removed the `scratch[i]` check after `i` changed
    /// inside the loop, so the `-5` store rewrote `publisher_path` in `main`.
    #[test]
    fn capability_free_dependency_cannot_rewrite_caller_path_via_bounds_elision_sec_080() {
        use crate::runtime::{Capabilities, Runtime};

        let attacker_src = r#"pub fn slice(text: String) -> String:
    let padding = [0]
    var scratch = [text]
    var i = 0
    while i < scratch.length():
        match i:
            0 -> i = 0 - 5
            _ -> ()
        scratch[i] = "evil.json"
        i = i + 7
    if padding.length() == 0:
        return "unreachable"
    text
"#;
        let app_src = r#"import attacker

fn main(console: Console, root: Dir[Read]):
    let publisher_path = ["safe.json"]
    let _ignored = attacker.slice("input")
    console.print(root.read(publisher_path[0]))
"#;
        let attacker = parser::parse_module(attacker_src).expect("attacker parse");
        let footprint = crate::capabilities::analyze(&attacker);
        let slice = footprint
            .entries
            .iter()
            .find(|entry| entry.name == "slice")
            .expect("slice footprint");
        assert!(slice.capabilities.is_empty(), "slice must receive no host authority");

        let app = parser::parse_module(app_src).expect("app parse");
        let linked = crate::pipeline::link(
            vec![("attacker".to_string(), attacker), ("app".to_string(), app)],
            "app",
        )
        .expect("link");
        typeck::check(&linked).expect("typecheck");

        let root = std::env::temp_dir().join(format!(
            "witchy-sec-080-no-cap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("safe.json"), "{\"source\":\"safe\"}").expect("seed safe");
        std::fs::write(root.join("evil.json"), "{\"source\":\"evil\",\"secret\":\"escaped\"}")
            .expect("seed evil");
        let root_text = root.to_str().expect("utf8 root");

        assert!(
            interpreter::run_module(linked.clone(), root_text, Vec::new()).is_err(),
            "the source semantics must reject the negative list index"
        );

        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the optimized binary path lowers this program");
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
        assert!(actor.run().is_err(), "optimized WASM must trap before the caller's read");
        assert!(actor.output().is_empty(), "the dependency redirected the caller's Dir authority");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// SEC-080: a dependency given only a benign `File[Read]` must not be able
    /// to manufacture some other authority. The vulnerable optimizer removed
    /// the `scratch[i]` check after `i` changed inside the loop, so the `-2`
    /// store overwrote the adjacent empty `List(Console)` length. Indexing that
    /// list then produced a scalar Console token and let the dependency print,
    /// even though Console was retained by `main` and absent from `read_file`'s
    /// capability footprint.
    #[test]
    fn file_only_dependency_cannot_forge_console_via_bounds_elision_sec_080() {
        use crate::runtime::{Capabilities, FsRights, Runtime};

        let attacker_src = r#"pub fn read_file(file: File[Read]) -> String:
    let consoles: List(Console) = []
    var scratch = ["seed"]
    var i = 0
    while i < scratch.length():
        match i:
            0 -> i = 0 - 2
            _ -> ()
        scratch[i] = "forged"
        i = i + 4
    let fake = list.at(consoles, 0)
    fake.print("FILE-ONLY-CAPABILITY-ESCAPE")
    file.read()
"#;
        let app_src = r#"import attacker

fn main(console: Console, config: File[Read]):
    let _ignored = attacker.read_file(config)
"#;
        let attacker = parser::parse_module(attacker_src).expect("attacker parse");
        let footprint = crate::capabilities::analyze(&attacker);
        let read_file = footprint
            .entries
            .iter()
            .find(|entry| entry.name == "read_file")
            .expect("read_file footprint");
        assert_eq!(
            crate::capabilities::show_caps(&read_file.capabilities),
            "File[Read]",
            "the dependency must receive no Console authority"
        );

        let app = parser::parse_module(app_src).expect("app parse");
        let linked = crate::pipeline::link(
            vec![("attacker".to_string(), attacker), ("app".to_string(), app)],
            "app",
        )
        .expect("link");
        typeck::check(&linked).expect("typecheck");

        let root = std::env::temp_dir().join(format!(
            "witchy-sec-080-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        let benign = root.join("config.json");
        std::fs::write(&benign, "{\"mode\":\"safe\"}").expect("seed file");

        assert!(
            interpreter::run_module_files(linked.clone(), &root, vec![benign.clone()]).is_err(),
            "the source semantics must reject the negative list index"
        );

        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the optimized binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    file_grants: vec![benign],
                    file_rights: vec![FsRights::new(true, false)],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(actor.run().is_err(), "optimized WASM must trap before forging Console");
        assert!(actor.output().is_empty(), "the dependency printed through forged authority");

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

    /// The sandbox grants exactly the computed footprint: a program combining
    /// argv, an `Env` narrowed to the named `PATH`, and a read-only Dir
    /// (minigrep's shape) runs confined, and its
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
            Some(vec!["PATH".to_string()]),
            None,
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

    /// The capability-rights showcase: it runs (exercising implicit + explicit
    /// `as` narrowing of a `Dir` to `Dir[Read]`) and its footprint is
    /// verb/transport-precise — the end-to-end demonstration of the feature.
    #[test]
    fn capability_rights_example_runs_and_audits() {
        let src = std::fs::read_to_string("examples/capability_rights/src/capability_rights.witchy").unwrap();
        let (checked, _) = crate::link_file_checked("examples/capability_rights/src/capability_rights.witchy")
            .expect("link and typecheck");
        let run_grant = crate::capabilities::run_grant(checked.module());
        assert!(
            !run_grant.contains_key("Net"),
            "the public `serve` helper must not grant Net to this example's main"
        );
        assert_eq!(
            crate::execute_file("examples/capability_rights/src/capability_rights.witchy", Vec::new()).unwrap(),
            vec![
                "implicit: hello from a sandboxed Dir capability",
                "explicit: hello from a sandboxed Dir capability",
            ]
        );
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        let shown = |name: &str| {
            let e = fp.entries.iter().find(|e| e.name == name).expect("entry");
            crate::capabilities::show_caps(&e.capabilities)
        };
        assert_eq!(shown("load"), "Dir[Read]");
        assert_eq!(shown("fetch"), "Net[Connect, Tcp]");
        assert_eq!(shown("serve"), "Net[Listen]");
    }
