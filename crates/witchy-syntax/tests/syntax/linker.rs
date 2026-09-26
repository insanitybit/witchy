    use witchy_syntax::linker::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    #[test]
    fn std_registry_matches_bundled_source_files() {
        let std_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../../std");
        let files: std::collections::BTreeSet<String> = std::fs::read_dir(std_dir)
            .expect("std directory")
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("witchy") {
                    return None;
                }
                path.file_stem()?.to_str().map(str::to_string)
            })
            .collect();
        let registered: std::collections::BTreeSet<String> =
            STD_MODULES.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(registered.len(), STD_MODULES.len(), "STD_MODULES contains a duplicate");
        assert_eq!(registered, files, "STD_MODULES must match std/*.witchy");
        for module in STD_MODULES {
            assert!(std_source(module).is_some(), "missing bundled source for `{module}`");
        }
    }

    #[test]
    fn persistent_std_cache_prunes_old_generations() {
        const GENERATIONS: usize = 70;
        const MAX_GENERATIONS: usize = 64;

        if let Some(root) = std::env::var_os("WITCHY_STDLIB_CACHE_PRUNE_PROBE") {
            let version_root = std::path::PathBuf::from(root).join("v1");
            std::fs::create_dir_all(&version_root).unwrap();
            for generation in 0..GENERATIONS {
                std::fs::create_dir(version_root.join(format!("{generation:064x}"))).unwrap();
            }
            std::fs::create_dir(version_root.join("not-a-cache-generation")).unwrap();

            link_main("import option\n\nfn main():\n    ()\n").unwrap();

            let entries = std::fs::read_dir(&version_root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>();
            let generations = entries
                .iter()
                .filter_map(|name| name.to_str())
                .filter(|name| {
                    name.len() == 64
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .count();
            assert!(generations <= MAX_GENERATIONS, "found {generations} cache generations");
            assert!(entries.iter().any(|name| name == "not-a-cache-generation"));
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "witchy-stdlib-cache-prune-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "linker::persistent_std_cache_prunes_old_generations"])
            .env("WITCHY_TEST_STDLIB_CACHE_DIR", &root)
            .env("WITCHY_STDLIB_CACHE_PRUNE_PROBE", &root)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn noop_expand(
        _: &str,
        _: &mut Module,
        _: &[(String, Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    /// Link `lib` (module `sealed_lib`) with `user` (module `user`, the entry) and
    /// return the link error message, if any.
    fn link_lib_user(lib: &str, user: &str) -> Result<(), String> {
        link_lib_user_with_mode(lib, user, LinkMode::Production)
    }

    fn link_lib_user_with_mode(lib: &str, user: &str, mode: LinkMode) -> Result<(), String> {
        let libm = witchy_syntax::parser::parse_module(lib).expect("lib parses");
        let userm = witchy_syntax::parser::parse_module(user).expect("user parses");
        link_with_mode(
            vec![("sealed_lib".to_string(), libm), ("user".to_string(), userm)],
            "user",
            noop_expand,
            mode,
        )
        .map(|_| ())
        .map_err(|e| e.message)
    }

    fn link_main(src: &str) -> Result<(), String> {
        let module = witchy_syntax::parser::parse_module(src).expect("parses");
        link(vec![("main".to_string(), module)], "main", noop_expand)
            .map(|_| ())
            .map_err(|e| e.message)
    }

    #[test]
    fn async_generated_calls_keep_their_module_when_a_local_shadows_it() {
        let source = "async fn value() -> Int:\n    7\n\nasync fn main(console: Console):\n    let task = 1\n    let result = value().await\n    console.print(\"${result + task}\")\n";
        let parsed = witchy_syntax::parser::parse_module(source).expect("async source parses");
        let linked = link(vec![("main".to_string(), parsed)], "main", noop_expand)
            .expect("generated task calls link despite the local binding");
        let mut generated_task_calls = 0;
        for item in &linked.items {
            if let Item::Function(function) = item {
                let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(
                    &function.body,
                    &mut |expression| {
                        if let Expr::Call { name, .. } = expression
                            && name == "task.and_then"
                        {
                            generated_task_calls += 1;
                        }
                        if let Expr::MethodCall { receiver, method, .. } = expression
                            && method == "and_then"
                            && matches!(receiver.as_ref(), Expr::Var(name) if name == "task")
                        {
                            panic!("generated task.and_then became a call on the local task");
                        }
                        Ok(())
                    },
                );
            }
        }
        assert!(generated_task_calls > 0, "await must produce a task.and_then call");
    }

    #[test]
    fn user_module_cannot_shadow_bundled_std_module() {
        let bytes = witchy_syntax::parser::parse_module("pub fn unrelated() -> Int:\n    1\n")
            .expect("local bytes parses");
        let main = witchy_syntax::parser::parse_module(
            "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${bytes.length(b)}\")\n",
        )
        .expect("main parses");
        let err = link(
            vec![("bytes".to_string(), bytes), ("main".to_string(), main)],
            "main",
            noop_expand,
        )
        .expect_err("default in-memory linking must reserve bundled std module names")
        .message;
        assert!(
            err.contains("module `bytes` uses a reserved standard-library name")
                && err.contains("one canonical owner"),
            "{err}"
        );
    }

    #[test]
    fn user_source_cannot_declare_compiler_private_names() {
        let err = link_main("type __Hidden:\n    Hidden(Int)\n").unwrap_err();
        assert!(err.contains("type `__Hidden`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Foo__Int:\n    Foo__Int(Int)\n").unwrap_err();
        assert!(err.contains("type `Foo__Int`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Sneak = __anon00000000010000000001120\n").unwrap_err();
        assert!(err.contains("type `__anon00000000010000000001120`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Box(value__type):\n    Box(value__type)\n").unwrap_err();
        assert!(
            err.contains("type parameter `value__type`") && err.contains("reserved for the compiler"),
            "{err}"
        );

        let err = link_main("type Record:\n    field__name: Int\n").unwrap_err();
        assert!(err.contains("field `field__name`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("fn Point__show(x: Int) -> String:\n    \"x\"\n").unwrap_err();
        assert!(
            err.contains("function `Point__show`") && err.contains("reserved for the compiler"),
            "{err}"
        );

        let err = link_main(
            "type Point:\n    value: Int\n\nimpl Point:\n    fn hidden__method(self) -> Int:\n        self.value\n",
        )
        .unwrap_err();
        assert!(
            err.contains("method `hidden__method`") && err.contains("reserved for the compiler"),
            "{err}"
        );

        let err = link_main(
            "fn main(console: Console):\n    let __target = 1\n    console.print(\"${__target}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `__target`"), "{err}");

        let err = link_main(
            "fn main(console: Console):\n    let user__target = 1\n    console.print(\"${user__target}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `user__target`"), "{err}");

        let err = link_main(
            "fn main(console: Console):\n    let __compr0 = 1\n    console.print(\"${__compr0}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `__compr0`"), "{err}");

        let err = witchy_syntax::parser::parse_module(
            "fn main(console: Console):\n    for __fv0 in [1]:\n        console.print(\"${__fv0}\")\n",
        )
        .expect_err("source loop variables cannot use generated compiler names");
        assert!(err.message.contains("identifier `__fv0` is reserved for the compiler"), "{err:?}");

        let err = witchy_syntax::parser::parse_module(
            "fn main(console: Console):\n    var xs = [1]\n    for var __fv0 in xs:\n        __fv0 = __fv0 + 1\n",
        )
        .expect_err("source for-var variables cannot use generated compiler names");
        assert!(err.message.contains("identifier `__fv0` is reserved for the compiler"), "{err:?}");

        let err = link_main(
            "fn main(console: Console):\n    let (__fortuple0, n) = (1, 2)\n    console.print(\"${n}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("pattern binding `__fortuple0`"), "{err}");

        link_main(
            "fn main(console: Console):\n    let xs = [n * 2 for n in [1, 2, 3]]\n    console.print(\"${xs}\")\n",
        )
        .expect("parser-generated list-comprehension names stay legal");

        link_main(
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var n in xs:\n        n = n + 1\n    console.print(\"ok\")\n",
        )
        .expect("parser-generated for-var index names stay legal");
    }

    #[test]
    fn private_bridge_intrinsics_are_std_only() {
        let err = link_main(
            "fn main(console: Console):\n    let s = __unerase(__erase(1))\n    console.print(\"${s}\")\n",
        )
        .unwrap_err();
        assert!(
            err.contains("`__unerase` is a compiler-private intrinsic")
                || err.contains("`__erase` is a compiler-private intrinsic"),
            "{err}"
        );

        let err = link_main(
            "fn main(console: Console):\n    let b = __bytes_from_string(\"x\")\n    console.print(\"x\")\n",
        )
        .unwrap_err();
        assert!(err.contains("`__bytes_from_string` is a compiler-private intrinsic"), "{err}");

        let err = link_main(
            "import task\n\nfn main(console: Console):\n    let _raw = task.__channel_open(0)\n    console.print(\"x\")\n",
        )
        .unwrap_err();
        assert!(err.contains("`__channel_open` is a compiler-private intrinsic"), "{err}");

        let bytes = witchy_syntax::parser::parse_module(std_source("bytes").expect("std bytes")).expect("bytes parses");
        link(vec![("bytes".to_string(), bytes)], "bytes", noop_expand)
            .expect("std/bytes may use the private bytes bridge");

        let task = witchy_syntax::parser::parse_module(std_source("task").expect("std task")).expect("task parses");
        let chan = witchy_syntax::parser::parse_module(std_source("chan").expect("std chan")).expect("chan parses");
        link(
            vec![("task".to_string(), task), ("chan".to_string(), chan)],
            "chan",
            noop_expand,
        )
        .expect("std/chan may use task's private channel bridge");
    }

    fn assert_raw_channel_privacy(source: &str, bridge: &str, admitted: &mut Vec<String>) {
        match link_main(source) {
            Ok(()) => {
                eprintln!("production link admitted external raw bridge: {bridge}");
                admitted.push(bridge.to_string());
            }
            Err(error) => assert_eq!(
                error,
                format!("`{bridge}` is a compiler-private intrinsic for std/chan or std/task`; use the public stdlib surface instead"),
                "the source must fail at the intended privacy boundary"
            ),
        }
    }

    #[test]
    fn raw_channel_privacy_external_bridge_calls_rejected() {
        let mut admitted = Vec::new();
        for (bridge, result, call) in [
            ("__channel_send_i64", "()", "task.__channel_send_i64(id, 7)"),
            ("__channel_recv_i64", "Option(Int)", "task.__channel_recv_i64(id)"),
            ("__channel_select2_map", "Option(String)",
                "task.__channel_select2_map(id, id, fn(_tag, _raw): task.done(None))"),
        ] {
            let source = format!(
                "import chan\nimport task\nfrom chan import Receiver\n\nfn raw(rx: Receiver(String)) -> task.Task({result}):\n    match rx:\n        Receiver(id) -> {call}\n\nfn main():\n    let _ = 0\n"
            );
            assert_raw_channel_privacy(&source, bridge, &mut admitted);
        }
        assert!(admitted.is_empty(), "external raw bridges admitted: {admitted:?}");
    }

    #[test]
    fn raw_channel_privacy_external_function_values_rejected() {
        let mut admitted = Vec::new();
        for (module, bridge) in [
            ("task", "__spawn_raw"),
            ("task", "__join_raw"),
            ("task", "__cancel_raw"),
            ("task", "__handle_from_raw"),
            ("task", "__handle_into_raw"),
            ("task", "__channel_send_i64"),
            ("task", "__channel_recv_i64"),
            ("task", "__channel_select2_map"),
            ("chan", "select2_frame_payload"),
            ("chan", "select2_frame_decode"),
            ("chan", "select3_frame_payload"),
            ("chan", "select3_frame_decode"),
            ("chan", "select4_frame_payload"),
            ("chan", "select4_frame_decode"),
        ] {
            let source = format!(
                "import {module}\n\nfn main():\n    let raw = {module}.{bridge}\n    let _ = raw\n"
            );
            assert_raw_channel_privacy(&source, bridge, &mut admitted);
        }
        assert!(admitted.is_empty(), "external raw function values admitted: {admitted:?}");
    }

    #[test]
    fn raw_channel_privacy_external_decoders_rejected() {
        let mut admitted = Vec::new();
        for arity in [2, 3, 4] {
            for kind in ["payload", "decode"] {
                let bridge = format!("select{arity}_frame_{kind}");
                let selected = if arity == 2 { "Selected".to_string() } else { format!("Selected{arity}") };
                let (result, arguments) = if kind == "payload" {
                    ("Option(String)".to_string(), "None")
                } else {
                    (format!("chan.{selected}(String)"), "0, None")
                };
                // The caller chooses String without possessing a Receiver(String).
                let source = format!(
                    "import chan\n\nfn recover() -> {result}:\n    chan.{bridge}({arguments})\n\nfn main():\n    let _ = recover()\n"
                );
                assert_raw_channel_privacy(&source, &bridge, &mut admitted);
            }
        }
        assert!(admitted.is_empty(), "external unbound message decoders admitted: {admitted:?}");
    }

    #[test]
    fn raw_channel_privacy_wrong_payload_route_rejected() {
        let source = "import chan\nimport task\nfrom chan import Receiver\n\nfn wrong(rx: Receiver(Int)) -> task.Task(Option(String)):\n    match rx:\n        Receiver(id) -> task.__channel_select2_map(id, id, fn(_tag, raw): task.done(chan.select2_frame_payload(raw)))\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(1).await\n    chan.send(tx, 7).await\n    let recovered = wrong(rx).await\n    console.print(\"${recovered}\")\n";
        let mut admitted = Vec::new();
        assert_raw_channel_privacy(source, "__channel_select2_map", &mut admitted);
        assert!(admitted.is_empty(), "Int-to-String raw decoder route passed production linking");
    }

    #[test]
    fn raw_channel_privacy_bare_imports_still_rejected() {
        let mut admitted = Vec::new();
        assert_raw_channel_privacy(
            "from chan import select2_frame_payload\n\nfn recover() -> Option(String):\n    select2_frame_payload(None)\n\nfn main():\n    let value = recover()\n",
            "select2_frame_payload", &mut admitted,
        );
        assert_raw_channel_privacy(
            "from chan import select2_frame_payload\n\nfn main():\n    let callback = select2_frame_payload\n",
            "select2_frame_payload", &mut admitted,
        );
        assert!(admitted.is_empty(), "bare import bypassed declaration privacy: {admitted:?}");
    }

    #[test]
    fn source_render_intrinsic_is_private() {
        let err = link_main("fn main(console: Console):\n    console.print(__render(1))\n")
            .expect_err("source-spellable render intrinsic is compiler-private");
        assert!(
            err.contains("`__render` is compiler-private")
                && err.contains("string interpolation")
                && err.contains("show.render"),
            "{err}"
        );

        link_main("fn main(console: Console):\n    console.print(\"${1}\")\n")
            .expect("interpolation still emits the generated render intrinsic");
    }

    #[test]
    fn module_private_functions_are_not_cross_module_api() {
        let lib = "fn hidden(n: Int) -> Int:\n    n + 1\n\n\
                   pub fn shown(n: Int) -> Int:\n    hidden(n)\n";

        let ok = "import sealed_lib\n\n\
                  fn main(console: Console):\n    console.print(\"${sealed_lib.shown(1)}\")\n";
        link_lib_user(lib, ok).expect("public function may call its private helper");

        let hidden_call = "import sealed_lib\n\n\
                           fn main(console: Console):\n    console.print(\"${sealed_lib.hidden(1)}\")\n";
        let err = link_lib_user(lib, hidden_call).expect_err("private function must not be module-callable");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");

        let hidden_ref = "import sealed_lib\n\n\
                          fn main(console: Console):\n    let f = sealed_lib.hidden\n    console.print(\"x\")\n";
        let err = link_lib_user(lib, hidden_ref).expect_err("private function must not be a module value");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");
    }

    #[test]
    fn static_evaluation_origin_constructors_remain_private_in_production() {
        let glamour = "fn interactive_with_origin(origin: String, program: (), initial: ()) -> ():\n    ()\n\n\
                       fn client_region_with_origin(origin: String, program: (), fallback: ()) -> ():\n    ()\n";
        for constructor in ["interactive_with_origin", "client_region_with_origin"] {
            let main = format!(
                "import glamour\n\nfn main() -> ():\n    glamour.{constructor}(\"origin\", (), ())\n"
            );
            let modules = vec![
                ("glamour".to_string(), witchy_syntax::parser::parse_module(glamour).expect("parse Glamour fixture")),
                ("main".to_string(), witchy_syntax::parser::parse_module(&main).expect("parse caller fixture")),
            ];
            let error = link_with_mode(modules, "main", noop_expand, LinkMode::Production)
                .expect_err("ordinary production source cannot call compiler-owned origin constructor")
                .to_string();
            assert!(
                error.contains(&format!("function `glamour.{constructor}` is private")),
                "{error}",
            );
        }
    }

    #[test]
    fn normal_importer_cannot_name_an_opt_type_that_contains_a_reference() {
        let api = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\ntype Holder:\n    Holder(&'a String)\n\npub fn make(text: &'a String) -> Holder:\n    Holder(text)\n",
        )
        .expect("legacy reference carrier is available only to the migration parser");
        let normal = witchy_syntax::parser::parse_module(
            "import api\n\nfn retain(holder: api.Holder) -> Int:\n    0\n\nfn main() -> Int:\n    0\n",
        )
        .expect("normal importer parses");

        let error = link(
            vec![("api".into(), api), ("normal".into(), normal)],
            "normal",
            noop_expand,
        )
        .expect_err("normal source must not name a reference-bearing opt type");
        assert!(
            error.message.contains("non-rooted-reference opt type `api.Holder`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn imported_exclusive_reference_result_is_not_erased_at_an_async_boundary() {
        let views = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\npub fn edit(text: &'a mut String) -> &'a mut String:\n    text\n",
        )
        .expect("exclusive-reference module parses");
        let user = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport views\n\nasync fn bad(console: Console):\n    var text = \"x\"\n    let editable = views.edit(&mut text)\n    let _ = task.done(0).await\n    console.print(editable)\n",
        )
        .expect("consumer parses");
        let error = link(
            vec![("views".into(), views), ("user".into(), user)],
            "user",
            noop_expand,
        )
        .expect_err("exclusive references must remain visible across imports");
        assert!(error.message.contains("remains live across `await`"), "{}", error.message);
    }

    #[test]
    fn sealing_rejects_module_qualified_constructor_and_pattern() {
        // BUG-313: a module-qualified constructor/pattern must not evade the RFC-0002
        // seal check — a sealed capability may be minted/destructured only in its
        // declaring module, on EVERY spelling (fail-closed).
        let lib = "capability Vault from Net\n\n\
                   pub fn make(net: Net) -> Vault:\n    Vault(net)\n\n\
                   pub fn zone(v: Vault) -> String:\n    match v:\n        Vault(n) -> \"z\"\n";

        // CONSTRUCT via `lib.Vault(...)` — rejected.
        let forge = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let forged = sealed_lib.Vault(net)\n    console.print(\"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("qualified construct must be rejected");
        assert!(err.contains("sealed capability") && err.contains("Vault"), "{err}");

        // DESTRUCTURE via `match v: lib.Vault(...)` — rejected.
        let destr = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let v = sealed_lib.make(net)\n    \
                     match v:\n        sealed_lib.Vault(inner) -> console.print(\"leak\")\n";
        let err = link_lib_user(lib, destr).expect_err("qualified destructure must be rejected");
        assert!(err.contains("sealed capability") && err.contains("destructure"), "{err}");

        // Legit: hold and pass a Vault through the lib's exported functions — allowed.
        let holder = "from sealed_lib import Vault\nimport sealed_lib\n\n\
                      fn use_it(v: Vault, console: Console):\n    console.print(sealed_lib.zone(v))\n\n\
                      fn main(console: Console, net: Net):\n    \
                      let v = sealed_lib.make(net)\n    use_it(v, console)\n";
        assert!(link_lib_user(lib, holder).is_ok(), "hold+pass must be allowed");
    }

    #[test]
    fn sealing_rejects_grantable_mint_from_paramless_main() {
        // BUG-313: a grantable root capability minted by `lib.RootCap(...)` from a
        // param-less main forges authority from nothing — rejected.
        let lib = "grantable capability UiRoot:\n    policy: String\n";
        let forge = "import sealed_lib\n\n\
                     fn main(console: Console):\n    \
                     let forged = sealed_lib.UiRoot(\"admin\")\n    console.print(\"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("grantable mint must be rejected");
        assert!(err.contains("sealed capability") && err.contains("UiRoot"), "{err}");
    }

    #[test]
    fn test_mode_keeps_sealed_capabilities_strict() {
        let lib = "capability Vault from Net\n\n\
                   pub fn make(net: Net) -> Vault:\n    Vault(net)\n";

        let forge = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let forged = sealed_lib.Vault(net)\n    console.print(\"x\")\n";
        let err = link_lib_user_with_mode(lib, forge, LinkMode::Test)
            .expect_err("test mode must not forge sealed capabilities");
        assert!(err.contains("sealed capability") && err.contains("construct"), "{err}");

        let destr = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let v = sealed_lib.make(net)\n    \
                     match v:\n        sealed_lib.Vault(inner) -> console.print(\"leak\")\n";
        let err = link_lib_user_with_mode(lib, destr, LinkMode::Test)
            .expect_err("test mode must not unwrap sealed capabilities");
        assert!(err.contains("sealed capability") && err.contains("destructure"), "{err}");
    }

    #[test]
    fn test_mode_does_not_relax_imported_dependency_modules() {
        let lib = witchy_syntax::parser::parse_module(
            "sealed type Box:\n    BoxData(Int)\n",
        )
        .expect("lib parses");
        let helper = witchy_syntax::parser::parse_module(
            "import sealed_lib\n\n\
             pub fn fake() -> sealed_lib.Box:\n    sealed_lib.BoxData(1)\n",
        )
        .expect("helper parses");
        let user = witchy_syntax::parser::parse_module(
            "import helper\n\n\
             fn main(console: Console):\n    let b = helper.fake()\n    console.print(\"x\")\n",
        )
        .expect("user parses");

        let err = link_with_mode(
            vec![
                ("sealed_lib".to_string(), lib),
                ("helper".to_string(), helper),
                ("user".to_string(), user),
            ],
            "user",
            noop_expand,
            LinkMode::Test,
        )
        .expect_err("only the entry test module receives sealed data construction privilege")
        .message;
        assert!(err.contains("sealed type") && err.contains("BoxData") && err.contains("helper"), "{err}");
    }


    /// A dev/test build reads bundled sources from THIS checkout's `std/`,
    /// located at run time, never from a path baked into a shared compiled unit.
    #[test]
    fn bundled_sources_come_from_this_checkout() {
        let std_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../../std");
        let on_disk = std::fs::read_to_string(std_dir.join("list").with_extension("witchy")).expect("std list");
        assert_eq!(std_source("list"), Some(on_disk.as_str()));
        if std::env::var_os("WITCHY_STD_DIR").is_none() {
            if let Some(dir) = bundled_source_dir() {
                assert_eq!(dir.canonicalize().unwrap(), std_dir.canonicalize().unwrap());
            }
        }
    }

    /// The executable outranks an inherited CARGO_MANIFEST_DIR, and the current
    /// directory is never consulted. Each probe re-runs this test binary with a
    /// decoy checkout as its current directory or its CARGO_MANIFEST_DIR; the
    /// decoy must lose to the checkout the executable lives in, and a copy of
    /// the executable outside any checkout must fail rather than take the cwd's.
    #[test]
    fn bundled_source_dir_prefers_the_executable_and_ignores_the_cwd() {
        if let Some(expected) = std::env::var_os("WITCHY_STD_RESOLVE_PROBE") {
            let dir = bundled_source_dir().expect("disk-backed build");
            assert_eq!(dir.canonicalize().unwrap(), std::path::PathBuf::from(expected));
            assert!(!std_source("list").unwrap().contains("decoy"));
            return;
        }
        if std::env::var_os("WITCHY_STD_DIR").is_some() || bundled_source_dir().is_none() {
            return; // an explicit override, or a build that embeds std
        }
        let std_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("../../std")
            .canonicalize()
            .unwrap();
        let exe = std::env::current_exe().unwrap();
        if !exe.canonicalize().unwrap().starts_with(std_dir.parent().unwrap()) {
            return; // a target dir outside the checkout: only CARGO_MANIFEST_DIR can find std
        }
        let decoy = std::env::temp_dir().join(format!("witchy-std-decoy-{}", std::process::id()));
        std::fs::create_dir_all(decoy.join("std")).unwrap();
        std::fs::create_dir_all(decoy.join("crates/witchy-syntax")).unwrap();
        for module in ["list", "option", "string"] {
            std::fs::write(decoy.join("std").join(module).with_extension("witchy"), "// decoy
").unwrap();
        }
        let outside = std::env::temp_dir().join(format!("witchy-std-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let copied = outside.join("syntax-probe");
        std::fs::copy(&exe, &copied).unwrap();
        let probe = |exe: &std::path::Path, manifest: Option<std::path::PathBuf>| {
            let mut command = std::process::Command::new(exe);
            command
                .args(["--exact", "linker::bundled_source_dir_prefers_the_executable_and_ignores_the_cwd"])
                .current_dir(&decoy)
                .env_remove("WITCHY_STD_DIR")
                .env_remove("CARGO_MANIFEST_DIR")
                .env("WITCHY_STD_RESOLVE_PROBE", &std_dir);
            if let Some(manifest) = manifest {
                command.env("CARGO_MANIFEST_DIR", manifest);
            }
            command.output().unwrap()
        };
        let outputs = [probe(&exe, None), probe(&exe, Some(decoy.join("crates/witchy-syntax")))];
        let stranded = probe(&copied, None);
        let _ = std::fs::remove_dir_all(&decoy);
        let _ = std::fs::remove_dir_all(&outside);
        let report = format!("{}{}", String::from_utf8_lossy(&stranded.stdout), String::from_utf8_lossy(&stranded.stderr));
        assert!(!stranded.status.success() && report.contains("set WITCHY_STD_DIR"), "{report}");
        for output in outputs {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
            assert!(stdout.contains("1 passed"), "the probe did not run: {stdout}");
        }
    }

    /// `WITCHY_STD_DIR` overrides where a dev/test build reads std from. The
    /// directory is resolved once per process, so the override runs in a child.
    #[test]
    fn witchy_std_dir_overrides_bundled_sources() {
        if let Some(expected) = std::env::var_os("WITCHY_STD_DIR_PROBE") {
            assert_eq!(std_source("list"), expected.to_str());
            assert_eq!(std_source("option"), Some("// option\n"));
            return;
        }
        if bundled_source_dir().is_none() {
            return; // this build embeds std; there is no directory to override
        }
        let dir = std::env::temp_dir().join(format!("witchy-std-dir-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = "// a list module that is not this checkout's\n";
        for (module, text) in [("list", list), ("option", "// option\n"), ("string", "")] {
            std::fs::write(dir.join(module).with_extension("witchy"), text).unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "linker::witchy_std_dir_overrides_bundled_sources", "--test-threads=1"])
            .env("WITCHY_STD_DIR", &dir)
            .env("WITCHY_STD_DIR_PROBE", list)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed"), "the probe did not run: {stdout}");
    }
