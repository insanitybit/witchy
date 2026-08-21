use super::*;
use crate::{codegen, interpreter, parser};

    /// (RFC-0032) `vm.par_map(xs, f)` maps a capture-free function over a list. On the
    /// interpreter it is the sequential oracle; on the compiled backend it runs across
    /// OS-thread VMs. Because results are collected by input index and `f` is a closed,
    /// deterministic top-level function, the
    /// two backends produce identical output (parity by determinism).
    #[test]
    fn vm_par_map_backends_agree() {
        let src = "import vm\n\nfn dbl(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    let prior: List(fn(Int) -> Int) = [fn(n: Int): n + 1]\n    console.print(\"${list.at(prior, 0)(6)}\")\n    let ys = vm.par_map([1, 2, 3, 4, 5], dbl)\n    console.print(\"${ys}\")\n    console.print(\"${list.length(ys)}\")\n";
        let expected = ["7", "[2, 4, 6, 8, 10]", "5"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) `vm.par_map` over `String` elements: each string is a flat
    /// `[len][bytes]` value, so it crosses to a worker VM by a plain byte copy (in via
    /// the worker's `__galloc`, result back out) — no marshaling. A witchy `String` is
    /// always valid UTF-8, so the round-trip is lossless. Both backends must agree.
    #[test]
    fn vm_par_map_string_backends_agree() {
        let src = "import vm\n\nfn shout(s: String) -> String:\n    s + \"!\"\n\nfn main(console: Console):\n    let prior: List(fn(String) -> String) = [fn(s: String): s + \"?\"]\n    console.print(list.at(prior, 0)(\"warm\"))\n    let ys = vm.par_map([\"a\", \"bb\", \"ccc\"], shout)\n    console.print(\"${ys}\")\n";
        let expected = ["warm?", "[a!, bb!, ccc!]"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) Cross-VM channels: `vm.serve(init, requests, handler)` runs a stateful
    /// service on a long-lived isolated worker VM — it threads `state` through the request
    /// stream (here a running byte concatenation) and emits each new state. Lock-step and
    /// deterministic, so the interpreter (sequential scan) and the compiled backend
    /// (persistent worker VM) produce identical responses.
    #[test]
    fn vm_serve_stateful_service_agrees() {
        let src = "import vm\nimport bytes\n\nfn step(state: Bytes, req: Bytes) -> Bytes:\n    bytes.concat(state, req)\n\nfn main(console: Console):\n    let prior: List(fn(Bytes, Bytes) -> Bytes) = [fn(state: Bytes, _req: Bytes): state]\n    console.print(bytes.to_string(list.at(prior, 0)(bytes.from_string(\"warm\"), bytes.from_string(\"ignored\"))))\n    let reqs = [bytes.from_string(\"a\"), bytes.from_string(\"b\"), bytes.from_string(\"c\")]\n    let outs = vm.serve(bytes.from_string(\"\"), reqs, step)\n    for o in outs:\n        console.print(bytes.to_string(o))\n";
        let expected = ["warm", "a", "ab", "abc"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032/RFC-0005) `vm.with_dir(dir, f, input)` transports the exact Dir
    /// externref through a dedicated typed worker trampoline. The interpreter is
    /// the sequential oracle and the compiled backend uses a fresh isolated VM.
    #[test]
    fn vm_with_dir_typed_callback_backends_agree() {
        let src = "import vm\nimport bytes\n\nfn reader(d: Dir, name: Bytes) -> Bytes:\n    bytes.from_string(d.read(bytes.to_string(name)))\n\nfn main(console: Console, dir: Dir):\n    let out = vm.with_dir(dir, reader, bytes.from_string(\"ok.txt\"))\n    console.print(bytes.to_string(out))\n";
        let root = std::env::temp_dir().join(format!(
            "witchy_vm_with_dir_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create vm.with_dir root");
        std::fs::write(root.join("ok.txt"), "typed-worker").expect("seed vm.with_dir root");
        let root_str = root.to_str().expect("utf8 root");
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), root_str, Vec::new()).expect("interp"),
            ["typed-worker"],
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("vm.with_dir lowers through its typed trampoline");
        let mut rt = crate::runtime::Runtime::batch().expect("runtime");
        let caps = crate::runtime::Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn(&bin, caps, 64).expect("spawn");
        actor.run().expect("run vm.with_dir");
        assert_eq!(actor.output(), ["typed-worker"]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// RFC-0005 Stage 3: a single-field capability brand over `Dir` is transparent
    /// at runtime, and `Option(ConfigDir)` is a nullable externref rather than a
    /// heap slot. This preserves the sealed smart-constructor idiom for root caps.
    #[test]
    fn branded_dir_option_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_brandeddir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("mkdir");
        std::fs::write(root.join("data").join("greeting.txt"), "hello-branded").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "capability ConfigDir from Dir[Read]\n\nfn config_dir(root: Dir[Read]) -> Option(ConfigDir):\n    if root.exists(\"data\"):\n        Some(ConfigDir(root.subtree(\"data\")))\n    else:\n        None\n\nfn load(c: ConfigDir, name: String) -> String:\n    match c:\n        ConfigDir(dir) -> dir.read(name)\n\nfn main(console: Console, root: Dir[Read]):\n    match config_dir(root):\n        Some(cfg) -> console.print(load(cfg, \"greeting.txt\"))\n        None -> console.print(\"missing\")\n";
        let want = vec!["hello-branded".to_string()];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers branded Dir options");
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

    /// (RFC-0032) `vm.par_map` over `Bytes`: binary payloads cross to worker VMs by a
    /// RAW (non-lossy) byte copy. Maps a top-level fn over a list of Bytes in parallel;
    /// both backends agree (the interp oracle runs the sequential `List.map` body).
    #[test]
    fn vm_par_map_bytes_backends_agree() {
        let src = "import vm\nimport bytes\n\nfn tag(b: Bytes) -> Bytes:\n    bytes.concat(b, bytes.from_string(\"!\"))\n\nfn main(console: Console):\n    let xs = [bytes.from_string(\"a\"), bytes.from_string(\"bb\"), bytes.from_string(\"ccc\")]\n    let ys = vm.par_map(xs, tag)\n    console.print(bytes.to_string(list.at(ys, 0)))\n    console.print(bytes.to_string(list.at(ys, 2)))\n    console.print(\"${bytes.length(list.at(ys, 1))}\")\n";
        let expected = ["a!", "ccc!", "3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// A build step runs in the **WASM sandbox**: it is compiled, instantiated
    /// with only the BuildOut/BuildRead host functions linked, reads a confined
    /// schema and writes generated source — and a `..` write traps via the same
    /// confinement as a runtime `Dir`.
    #[test]
    fn build_step_runs_in_the_wasm_sandbox() {
        let root = std::env::temp_dir().join(format!("witchy_wasm_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let schema = root.join("schema");
        std::fs::create_dir_all(&schema).unwrap();
        std::fs::write(schema.join("svc.txt"), "Greeter").unwrap();

        let module = parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    let nl = \"\\n\"\n    out.write_out(\"api.witchy\", \"pub fn service() -> String:\" + nl + \"    \\\"\" + schema.read_build(\"svc.txt\") + \"\\\"\" + nl)\n",
        )
        .expect("parse");
        let generated = crate::run_build_step_sandboxed(
            module,
            root.join("out"),
            vec![schema.clone()],
        )
        .expect("sandboxed build step runs");
        assert_eq!(generated, vec!["api.witchy".to_string()]);
        let body = std::fs::read_to_string(root.join("out/api.witchy")).unwrap();
        assert!(body.contains("\"Greeter\""), "generated source embeds the schema value: {body}");

        // A `..` escape traps inside the sandbox, exactly like a runtime Dir.
        let escaper = parser::parse_module(
            "fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let err = crate::run_build_step_sandboxed(escaper, root.join("out2"), Vec::new())
            .expect_err("a `..` write must trap in the sandbox");
        assert!(err.contains("escapes the Dir capability"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A confinement violation in the WASM sandbox surfaces the same clean
    /// message both backends print — the root cause, not
    /// wasmtime's "error while executing at wasm backtrace…" wrapper.
    #[test]
    fn sandbox_confinement_error_is_clean() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src_path = root.join("reader.witchy");
        std::fs::write(
            &src_path,
            "fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:\n    console.print(dir.read(list.at(args, 0)))\n    0\n",
        )
        .unwrap();
        let err = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            vec![root.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec!["../secret.txt".to_string()],
            None,
            Vec::new(),
            witchy_confinement::EnforcementMode::Disabled,
        )
        .expect_err("a `..` traversal must be denied");
        assert!(
            err.contains("escapes the Dir capability"),
            "the denial should cite the Dir capability, got: {err}"
        );
        assert!(
            !err.contains("wasm backtrace"),
            "the raw wasmtime backtrace must not leak to the user, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (RFC-0045) `witchy parity` on an aborting program passes only when both
    /// backends produce the same complete location-prefixed diagnostic.
    #[test]
    fn parity_file_agrees_on_matching_aborts() {
        let path = std::env::temp_dir().join(format!("witchy_verify_abort_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 9)}\")\n",
        )
        .unwrap();
        let outcome = crate::parity_check(path.to_str().unwrap());
        assert!(
            matches!(outcome, crate::ParityOutcome::BothErrorAgree { .. }),
            "both backends must abort with the same complete diagnostic: {}",
            outcome.message()
        );
        let _ = std::fs::remove_file(&path);
    }
