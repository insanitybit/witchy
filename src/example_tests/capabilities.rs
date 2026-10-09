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

    /// The admitted canonical artifact for a multi-module program.
    fn admit_linked(sources: &[(&str, &str)], entry: &str) -> witchy_wir::AdmittedCanonicalArtifact {
        let checked = checked_modules(sources, entry);
        codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical pipeline lowers this program")
    }

    /// Run `src` as unoptimized admitted Wasm with a read/write `Dir` grant on
    /// `root`; a trap is `Err` with its root cause.
    fn run_admitted_dir(src: &str, root: &std::path::Path) -> Result<Vec<String>, String> {
        run_admitted(src, crate::runtime::Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.to_path_buf()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        })
    }

    /// Run `src` as unoptimized admitted Wasm under `caps`; a trap is `Err`
    /// with its root cause.
    fn run_admitted(src: &str, caps: crate::runtime::Capabilities) -> Result<Vec<String>, String> {
        struct ResetOptimization;
        impl Drop for ResetOptimization {
            fn drop(&mut self) { crate::opt::set_for_tests(None); }
        }
        crate::opt::set_for_tests(Some(crate::opt::OptSet::none()));
        let _reset = ResetOptimization;
        let checked = witchy::resolve_std_only_checked(src).expect("check Dir program");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit Dir program");
        let mut rt = crate::runtime::Runtime::batch().expect("runtime");
        let mut vm = rt.spawn_admitted(&artifact, caps, 64).expect("spawn admitted Dir program");
        vm.run().map_err(|error| error.root_cause().to_string())?;
        Ok(vm.output())
    }

    /// The `CapError` classes `dir.read` answers, rendered independently of
    /// `show`, for the admitted-Wasm Dir tests.
    const SHOW_DIR_READ: &str = "fn shown(r: Result(String, CapError)) -> String:\n    match r:\n        Ok(text) -> \"ok: \" + text\n        Err(Denied(m)) -> \"denied: \" + m\n        Err(NotFound(m)) -> \"missing: \" + m\n        Err(NotAFile(m)) -> \"not a file: \" + m\n        Err(AlreadyExists(m)) -> \"exists: \" + m\n        Err(Io(m)) -> \"io: \" + m\n\n";

    /// The Dir family runs confined as admitted Wasm: read/exists/is_dir/
    /// subtree/write/make_dir/list round-trip in a temp directory. RFC-0126
    /// rule 3: `dir.read` answers `NotFound` and `NotAFile` for what the
    /// capability can see, and `Denied` for an escape or a policy refusal
    /// whether or not anything exists behind it, so the variant is never an
    /// existence oracle. No answer the guest sees names a host path.
    #[test]
    fn dir_capability_compiles_to_wasm_and_confines() {
        let base = std::env::temp_dir().join(format!("witchy_wasm_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        let src = format!("{SHOW_DIR_READ}fn main(console: Console, dir: Dir):\n    console.print(shown(dir.read(\"a.txt\")))\n    console.print(\"${{dir.exists(\"a.txt\") ?? false}}\")\n    console.print(\"${{dir.exists(\"missing.txt\") ?? true}}\")\n    let sub = dir.subtree(\"sub\") ?? dir\n    console.print(shown(sub.read(\"b.txt\")))\n    let _ = dir.write(\"out.txt\", \"written\")\n    console.print(shown(dir.read(\"out.txt\")))\n    let _ = dir.make_dir(\"made\")\n    console.print(\"${{dir.is_dir(\"made\") ?? false}}\")\n    for name in dir.list() ?? []:\n        console.print(\"entry: \" + name)\n");
        let want = [
            "ok: alpha", "true", "false", "ok: beta", "ok: written", "true",
            "entry: a.txt", "entry: made", "entry: out.txt", "entry: sub",
        ];
        assert_eq!(run_admitted_dir(&src, &root).expect("run Dir program"), want);

        std::fs::write(base.join("outside.txt"), "escaped contents").expect("seed outside");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../outside.txt", root.join("link-out.txt")).expect("link out");
            std::os::unix::fs::symlink("../missing.txt", root.join("link-missing.txt")).expect("link missing");
        }
        let denied = |path: &str| format!("denied: `{path}` is not covered by this Dir capability");
        // (receiver, path, expected): one program reads every row.
        let mut rows = vec![
            ("dir", "missing.txt", "missing: `missing.txt` was not found".to_string()),
            ("dir", "sub", "not a file: `sub` is not a file".to_string()),
            ("dir", "../outside.txt", denied("../outside.txt")),
            ("dir", "../missing.txt", denied("../missing.txt")),
            ("dir", "/etc/hosts", denied("/etc/hosts")),
        ];
        if cfg!(unix) {
            rows.push(("dir", "link-out.txt", denied("link-out.txt")));
            rows.push(("dir", "link-missing.txt", denied("link-missing.txt")));
        }
        // Policy refusals run end to end in
        // `dir_only_entry_policy_confines_in_admitted_wasm`.
        let mut body = String::from("fn main(console: Console, dir: Dir):\n");
        for (receiver, path, _) in &rows {
            body.push_str(&format!("    console.print(shown({receiver}.read(\"{path}\")))\n"));
        }
        let out = run_admitted_dir(&format!("{SHOW_DIR_READ}{body}"), &root).expect("a failed read is a value");
        let want: Vec<String> = rows.iter().map(|(_, _, want)| want.clone()).collect();
        assert_eq!(out, want);
        // The write family and the probes answer `CapError` values the same
        // way (RFC-0126 rule 3); every class is pinned at the host by
        // `dir_write_family_answers_failures_as_statuses`.
        let effects = "fn done(r: Result((), CapError)) -> String:\n    match r:\n        Ok(_) -> \"ok\"\n        Err(Denied(m)) -> \"denied: \" + m\n        Err(NotFound(m)) -> \"missing: \" + m\n        Err(NotAFile(m)) -> \"not a file: \" + m\n        Err(AlreadyExists(_)) -> \"exists\"\n        Err(Io(m)) -> \"io: \" + m\n\nfn probed(r: Result(Bool, CapError)) -> String:\n    match r:\n        Ok(found) -> \"${found}\"\n        Err(e) -> show(e)\n\nfn main(console: Console, dir: Dir):\n    console.print(done(dir.write(\"../escape.txt\", \"x\")))\n    console.print(done(dir.write(\"sub\", \"x\")))\n    console.print(done(dir.append(\"nowhere/x.txt\", \"x\")))\n    console.print(done(dir.create_new(\"a.txt\", \"x\")))\n    console.print(done(dir.create_new(\"fresh.txt\", \"x\")))\n    console.print(done(dir.make_dir(\"../made\")))\n    console.print(done(dir.rename(\"fresh.txt\", \"/etc/moved\")))\n    console.print(probed(dir.exists(\"../outside.txt\")))\n    console.print(probed(dir.is_dir(\"sub\")))\n";
        let want = vec![
            "denied: `../escape.txt` is not covered by this Dir capability".to_string(),
            "not a file: `sub` is not a file".to_string(),
            "missing: `nowhere/x.txt` was not found (or its parent was not)".to_string(),
            "exists".to_string(), "ok".to_string(),
            "denied: `../made` is not covered by this Dir capability".to_string(),
            "denied: `/etc/moved` is not covered by this Dir capability".to_string(),
            "denied: `../outside.txt` is not covered by this Dir capability".to_string(),
            "true".to_string(),
        ];
        let out = run_admitted_dir(effects, &root).expect("a failed write is a value");
        assert_eq!(out, want);
        assert!(!base.join("escape.txt").exists() && !base.join("made").exists());
        for line in &out {
            assert!(!line.contains(base.to_str().expect("utf8 base")), "a guest message disclosed a host path: {line}");
        }
        let host = base.to_str().expect("utf8 base");
        for line in &out {
            assert!(!line.contains(host), "a guest message disclosed a host path: {line}");
            assert!(!line.contains("escaped contents"), "a guest message carried an excluded file: {line}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// RFC-0012/RFC-0126: `main`'s `File` parameters are launch-supplied in
    /// admitted Wasm, each from its grant ordinal among the `File`
    /// parameters, so the i-th `File` is backed by the i-th `--file` grant.
    /// A `File[Read]` grant reads and refuses nothing it was not given.
    #[test]
    fn file_entry_parameters_bind_positionally_in_admitted_wasm() {
        let base = std::env::temp_dir().join(format!("witchy_file_entry_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("mkdir");
        std::fs::write(base.join("first.txt"), "one").expect("seed first");
        std::fs::write(base.join("second.txt"), "two").expect("seed second");
        let src = "fn main(console: Console, a: File[Read], b: File[Read]):\n    console.print(a.read() ?? \"unreadable\")\n    console.print(b.read() ?? \"unreadable\")\n";
        let out = run_admitted(src, crate::runtime::Capabilities {
            print: true,
            quiet: true,
            file_grants: vec![base.join("first.txt"), base.join("second.txt")],
            ..Default::default()
        }).expect("both File parameters are granted");
        assert_eq!(out, vec!["one".to_string(), "two".to_string()]);
        let missing = run_admitted(src, crate::runtime::Capabilities {
            print: true,
            quiet: true,
            file_grants: vec![base.join("first.txt")],
            ..Default::default()
        }).expect_err("an ungranted File parameter is refused at launch");
        assert!(missing.contains("requires a File capability for parameter 1"), "{missing}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// RFC-0011 entry policies through `dir.only(...)` in admitted Wasm. A name
    /// the policy refuses is `Denied` present or absent, an entry it cannot see
    /// answers like a missing one (`NotFound`, or `Ok(false)` from a probe), an
    /// escape is `Denied`, and a visible entry works. Narrowing composes and
    /// never widens: a disjoint refinement denies everything. Every class
    /// (aliases, case folds, each write) is pinned at the host by
    /// `dir_read_at_answers_failures_as_statuses` and
    /// `dir_write_family_answers_failures_as_statuses`.
    #[test]
    fn dir_only_entry_policy_confines_in_admitted_wasm() {
        let base = std::env::temp_dir().join(format!("witchy_wasm_dir_only_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("keep.bin"), "binary").expect("seed bin");
        std::fs::write(root.join("sub/y.txt"), "hello").expect("seed y");
        std::fs::write(base.join("outside.txt"), "escaped contents").expect("seed outside");
        #[cfg(unix)]
        std::os::unix::fs::symlink("keep.bin", root.join("alias.txt")).expect("alias");
        let denied = |path: &str| format!("denied: `{path}` is not covered by this Dir capability");
        let missing = |path: &str| format!("missing: `{path}` was not found");
        let unwritable = |path: &str| format!("missing: `{path}` was not found (or its parent was not)");
        // (expression, expected): one program evaluates every row.
        let mut rows: Vec<(String, String)> = vec![
            ("shown(text.read(\"a.txt\"))".into(), "ok: alpha".into()),
            ("shown(text.read(\"sub/y.txt\"))".into(), "ok: hello".into()),
            ("shown(text.read(\"keep.bin\"))".into(), denied("keep.bin")),
            ("shown(text.read(\"absent.bin\"))".into(), denied("absent.bin")),
            ("shown(text.read(\"../outside.txt\"))".into(), denied("../outside.txt")),
            ("shown(text.read(\"keep.bin/x.txt\"))".into(), missing("keep.bin/x.txt")),
            ("shown(files.read(\"sub\"))".into(), missing("sub")),
            ("shown(both.read(\"a.txt\"))".into(), "ok: alpha".into()),
            ("shown(both.read(\"keep.bin\"))".into(), denied("keep.bin")),
            ("shown(none.read(\"a.txt\"))".into(), denied("a.txt")),
            ("shown(none.only(Dir.ext(\".txt\")).read(\"a.txt\"))".into(), denied("a.txt")),
            ("done(text.write(\"new.txt\", \"x\"))".into(), "ok".into()),
            ("done(text.write(\"new.bin\", \"x\"))".into(), denied("new.bin")),
            ("done(text.write(\"../escape.txt\", \"x\"))".into(), denied("../escape.txt")),
            ("done(files.write(\"sub\", \"x\"))".into(), unwritable("sub")),
            ("probed(text.exists(\"a.txt\"))".into(), "true".into()),
            ("probed(text.exists(\"keep.bin\"))".into(), "false".into()),
            ("probed(text.exists(\"../outside.txt\"))".into(), denied("../outside.txt")),
            ("probed(files.is_dir(\"sub\"))".into(), "false".into()),
        ];
        if cfg!(unix) {
            rows.push(("shown(text.read(\"alias.txt\"))".into(), missing("alias.txt")));
            rows.push(("done(text.write(\"alias.txt\", \"x\"))".into(), unwritable("alias.txt")));
        }
        let mut src = format!("{SHOW_DIR_READ}fn done(r: Result((), CapError)) -> String:\n    match r:\n        Ok(_) -> \"ok\"\n        Err(e) -> shown(Err(e))\n\nfn probed(r: Result(Bool, CapError)) -> String:\n    match r:\n        Ok(found) -> \"${{found}}\"\n        Err(e) -> shown(Err(e))\n\n");
        src.push_str("fn main(console: Console, dir: Dir):\n    let text = dir.only(Dir.ext(\".txt\"))\n    let files = dir.only(Dir.files())\n    let both = files.only(Dir.ext(\".txt\"))\n    let none = text.only(Dir.ext(\".bin\"))\n");
        for (expression, _) in &rows {
            src.push_str(&format!("    console.print({expression})\n"));
        }
        let out = run_admitted_dir(&src, &root).expect("a policy refusal is a value");
        let want: Vec<String> = rows.iter().map(|(_, want)| want.clone()).collect();
        assert_eq!(out, want);
        assert_eq!(std::fs::read_to_string(root.join("keep.bin")).expect("keep.bin"), "binary", "a hidden file was written");
        assert!(!root.join("new.bin").exists() && !base.join("escape.txt").exists());
        let host = base.to_str().expect("utf8 base");
        for line in &out {
            assert!(!line.contains(host), "a guest message disclosed a host path: {line}");
            assert!(!line.contains("escaped contents") && !line.contains("binary"), "a guest message carried a hidden file: {line}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// RFC-0011: `dir.subtree(path)` narrows a `Dir` to a subtree, chained
    /// `.subtree(...).subtree(...)` alike, and the same `..` confinement holds
    /// inside it: an escape from a subtree is `Denied`. `subtree` itself
    /// answers `Result(Dir, CapError)` (RFC-0126 rule 3).
    #[test]
    fn dir_subtree_method_narrows_on_both_backends() {
        let root = std::env::temp_dir().join(format!("witchy_subtree_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deep")).expect("mkdir deep");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");
        std::fs::write(root.join("sub/deep/c.txt"), "gamma").expect("seed c");
        let src = format!("{SHOW_DIR_READ}fn opened(r: Result(Dir, CapError)) -> String:\n    match r:\n        Ok(_) -> \"opened\"\n        Err(e) -> show(e)\n\nfn main(console: Console, dir: Dir):\n    let s = dir.subtree(\"sub\") ?? dir\n    console.print(shown(s.read(\"b.txt\")))\n    console.print(shown((s.subtree(\"deep\") ?? s).read(\"c.txt\")))\n    console.print(shown(s.read(\"../a.txt\")))\n    console.print(opened(dir.subtree(\"missing\")))\n    console.print(opened(dir.subtree(\"../outside\")))\n    console.print(opened(s.subtree(\"..\")))\n");
        assert_eq!(
            run_admitted_dir(&src, &root).expect("run"),
            [
                "ok: beta", "ok: gamma", "denied: `../a.txt` is not covered by this Dir capability",
                "not found: `missing` was not found",
                "denied: `../outside` is not covered by this Dir capability",
                "denied: `..` is not covered by this Dir capability",
            ],
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0012: `dir.read_file`/`dir.write_file` navigate a `Dir` to a
    /// confined `File` leaf, and `file.read()`/`file.write(data)` operate on
    /// it. `file.read()` answers `Result(String, CapError)` (RFC-0126 rule 3);
    /// its failure classes are pinned at the host by
    /// `file_read_at_answers_failures_as_statuses`. Opening answers
    /// `Result(File, CapError)`: a missing file is `NotFound` and a path
    /// outside the `Dir` is `Denied`.
    #[test]
    fn file_capability_compiles_to_wasm_and_confines() {
        let root = std::env::temp_dir().join(format!("witchy_wasm_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("note.txt"), "alpha").expect("seed");

        let src = "fn shown(r: Result(String, CapError)) -> String:\n    match r:\n        Ok(text) -> \"ok: \" + text\n        Err(e) -> \"failed: \" + show(e)\n\nfn read_through(dir: Dir, name: String) -> Result(String, CapError):\n    dir.read_file(name)?.read()\n\nfn main(console: Console, dir: Dir):\n    console.print(shown(read_through(dir, \"note.txt\")))\n    match dir.write_file(\"out.txt\"):\n        Ok(out) ->\n            match out.write(\"beta\"):\n                Ok(_) -> ()\n                Err(e) -> console.print(show(e))\n        Err(e) -> console.print(show(e))\n    console.print(shown(read_through(dir, \"out.txt\")))\n    console.print(shown(read_through(dir, \"missing.txt\")))\n    console.print(shown(read_through(dir, \"../escape.txt\")))\n";
        assert_eq!(run_admitted_dir(src, &root).expect("run"), [
            "ok: alpha", "ok: beta",
            "failed: not found: `missing.txt` was not found",
            "failed: denied: `../escape.txt` is not covered by this Dir capability",
        ]);

        let _ = std::fs::remove_dir_all(&root);
    }

    // A `File` entry parameter (the `--file` grant) cannot yet be supplied by
    // the admitted launch protocol, and `file.write` answers a `CapError`
    // result that only admitted Wasm builds; the installed trusted-app test
    // covers a granted `File`, and `file_capability_compiles_to_wasm_and_confines`
    // covers `file.write` on a navigated `File`.

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
    // The path the caller would hand `root`: no Dir operation lowers on
    // this legacy optimized path, so the steered path itself is the escape.
    console.print(publisher_path[0])
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

        let bytes = admit_linked(&[("attacker", attacker_src), ("app", app_src)], "app");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(
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
        let bytes = admitted_artifact(src);
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(
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
        let wbytes = admitted_artifact(esc);
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2
            .spawn_admitted(
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
        let base = std::env::temp_dir().join(format!("witchy_multidir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).expect("mkdir a");
        std::fs::create_dir_all(&dir_b).expect("mkdir b");
        std::fs::write(dir_a.join("f.txt"), "from-A").expect("seed a");
        std::fs::write(dir_b.join("f.txt"), "from-B").expect("seed b");

        // Both Dirs name `f.txt`, but each resolves within its own subtree.
        let src = "fn main(console: Console, da: Dir, db: Dir):\n    console.print(da.read(\"f.txt\") ?? \"unreadable\")\n    console.print(db.read(\"f.txt\") ?? \"unreadable\")\n";
        let out = run_admitted(src, crate::runtime::Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(dir_a.clone()),
            dir_roots: vec![dir_b.clone()],
            dir_read: true,
            ..Default::default()
        })
        .expect("run");
        assert_eq!(out, ["from-A", "from-B"]);

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
        let admit = |src: &str| {
            let checked = witchy::resolve_std_only_checked(src).expect("check");
            codegen::compile_checked_module_artifact(&checked).expect_lowered("the canonical route admits this program")
        };
        let writer = admit("fn main(console: Console, dir: Dir):\n    let _ = dir.write(\"x.txt\", \"data\")\n    console.print(\"wrote\")\n");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn_admitted(
            &writer,
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
        let reader = admit("fn main(console: Console, dir: Dir):\n    console.print(\"${dir.exists(\"x.txt\") ?? false}\")\n");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn_admitted(
            &reader,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.and_then(|mut vm| vm.run()).is_err(), "a Dir program must be refused before it runs without a Dir grant");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The enforcement half: an admitted module that takes a Clock/Env but was
    /// NOT granted one must fail before it runs — the host function simply is
    /// not linked, so the authority is structurally absent.
    #[test]
    fn ungranted_clock_and_env_fail_to_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let srcs = [
            "fn main(console: Console, clock: Clock):\n    console.print(\"${clock.now()}\")\n",
            "fn main(console: Console, env: Env):\n    match env.get_env(\"X\"):\n        Ok(_) -> console.print(\"answered\")\n        Err(_) -> console.print(\"denied\")\n",
        ];
        for src in srcs {
            let checked = witchy::resolve_std_only_checked(src).expect("check");
            let artifact = codegen::compile_checked_module_artifact(&checked)
                .expect_lowered("the canonical route admits this program");
            let mut rt = Runtime::batch().expect("runtime");
            // The entry's root capability is minted at launch, before `main`.
            let denied = rt
                .spawn_admitted(&artifact, Capabilities { print: true, ..Default::default() }, 4)
                .and_then(|mut vm| vm.run().map(|()| vm.output()));
            assert!(denied.is_err(), "ungranted Clock/Env must be refused before it runs: {denied:?}");
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
            "import option\n\nfn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:\n    let path = list.at(args, 0)\n    let label = (env.get_env(\"PATH\") ?? None) ?? \"unlabeled\"\n    for line in (dir.read(path) ?? \"\").lines():\n        if line.contains(\"needle\"):\n            console.print(label + \": \" + line)\n    0\n",
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
            crate::HeapGrant::Standard,
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
        let bytes = admitted_artifact(include_str!("../../examples/compute/src/compute.witchy"));
        let mut rt = Runtime::new().expect("runtime");
        let result = rt.spawn_admitted(&bytes, Capabilities::none(), 4).and_then(|mut vm| vm.run());
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
