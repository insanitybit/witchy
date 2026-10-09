use super::*;
use crate::{codegen, parser};

struct ResetOptimization;
impl Drop for ResetOptimization {
    fn drop(&mut self) { crate::opt::set_for_tests(None); }
}

fn unoptimized() -> ResetOptimization {
    crate::opt::set_for_tests(Some(crate::opt::OptSet::none()));
    ResetOptimization
}

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
    /// externref through a dedicated typed worker trampoline in an isolated VM.
    #[test]
    fn vm_with_dir_typed_callback_backends_agree() {
        let _reset = unoptimized();
        let src = "import vm\nimport bytes\n\nfn reader(d: Dir, name: Bytes) -> Bytes:\n    bytes.from_string(d.read(bytes.to_string(name)) ?? \"\")\n\nfn main(console: Console, dir: Dir):\n    let out = vm.with_dir(dir, reader, bytes.from_string(\"ok.txt\"))\n    console.print(bytes.to_string(out))\n";
        let root = std::env::temp_dir().join(format!(
            "witchy_vm_with_dir_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create vm.with_dir root");
        std::fs::write(root.join("ok.txt"), "typed-worker").expect("seed vm.with_dir root");
        let checked = witchy::resolve_std_only_checked(src)
            .expect("check vm.with_dir typed callback");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit vm.with_dir typed callback");
        let mut rt = crate::runtime::Runtime::batch().expect("runtime");
        let caps = crate::runtime::Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn_admitted(&artifact, caps, 64).expect("spawn admitted callback");
        actor.run().expect("run vm.with_dir");
        assert_eq!(actor.output(), ["typed-worker"]);
        assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
        let _ = std::fs::remove_dir_all(root);
    }

    /// RFC-0005 Stage 3: a single-field capability brand over `Dir` is transparent
    /// at runtime, and `Option(ConfigDir)` is a nullable externref rather than a
    /// heap slot. This preserves the sealed smart-constructor idiom for root caps.
    #[test]
    fn branded_dir_option_runs_on_both_backends() {
        let _reset = unoptimized();
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_brandeddir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("mkdir");
        std::fs::write(root.join("data").join("greeting.txt"), "hello-branded").expect("seed");
        let src = "capability ConfigDir from Dir[Read]\n\nfn config_dir(root: Dir[Read]) -> Option(ConfigDir):\n    match root.subtree(\"data\"):\n        Ok(data) -> Some(ConfigDir(data))\n        Err(_) -> None\n\nfn load(c: ConfigDir, name: String) -> String:\n    match c:\n        ConfigDir(dir) -> dir.read(name) ?? \"unreadable\"\n\nfn main(console: Console, root: Dir[Read]):\n    match config_dir(root):\n        Some(cfg) -> console.print(load(cfg, \"greeting.txt\"))\n        None -> console.print(\"missing\")\n";
        let want = vec!["hello-branded".to_string()];
        let checked = witchy::resolve_std_only_checked(src)
            .expect("check branded Dir option");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit branded Dir option");
        let mut rt = Runtime::batch().expect("runtime");
        let caps = Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn_admitted(&artifact, caps, 64).expect("spawn admitted Dir option");
        actor.run().expect("run");
        assert_eq!(actor.output(), want);
        assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
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
            "fn main(console: Console, dir: Dir, args: List(String)) -> Int:\n    match dir.write(list.at(args, 0), \"x\"):\n        Ok(_) -> ()\n        Err(error) -> fail(\"${error}\")\n    0\n",
        )
        .unwrap();
        let err = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            vec![root.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            vec!["../secret.txt".to_string()],
            None,
            Vec::new(),
            crate::HeapGrant::Standard,
            witchy_confinement::EnforcementMode::Disabled,
        )
        .expect_err("a `..` traversal must be denied");
        assert!(
            err.contains("is not covered by this Dir capability"),
            "the denial should cite the Dir capability, got: {err}"
        );
        assert!(
            !err.contains("wasm backtrace"),
            "the raw wasmtime backtrace must not leak to the user, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
