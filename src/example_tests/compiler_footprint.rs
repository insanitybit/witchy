use super::*;
use crate::{ast, interpreter, parser, typeck};

    #[test]
    fn native_compiler_intrinsics_reject_comptime_source_strings() {
        // The compiler-service natives live above the runtime kernel in
        // `witchy-interp`; install their vtable before resolving them here.
        witchy_interp::compiler_natives::install();
        use witchy_runtime::value::NativeValue;

        let invalid = "fn main(console: Console):\n    missing(console)\n";
        let footprint = witchy_runtime::native::lookup("compiler.footprint")
            .expect("compiler.footprint native");
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(invalid.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid source must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "error must include the type error: {json}");

        let src = "comptime:\n    emit_item(quote item:\n        pub fn generated(net: Net) -> Int:\n            7\n    )\n";
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(src.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime source must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "error must name the boundary: {json}");

        let diff =
            witchy_runtime::native::lookup("compiler.diff").expect("compiler.diff native");
        let NativeValue::Str(json) = diff(&[
            NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()),
            NativeValue::Str(invalid.into()),
        ])
        .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid diff must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "diff error must include the type error: {json}");

        let NativeValue::Str(json) =
            diff(&[NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()), NativeValue::Str(src.into())])
                .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime diff must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "diff error must name the boundary: {json}");

        let doc = witchy_runtime::native::lookup("compiler.doc").expect("compiler.doc native");
        let NativeValue::Str(md) =
            doc(&[NativeValue::Str("generated".into()), NativeValue::Str(src.into())]).expect("native doc")
        else {
            panic!("compiler.doc must return a markdown string");
        };
        assert!(md.contains("doc error:"), "comptime doc must return an error comment: {md}");
        assert!(md.contains("does not support comptime"), "doc error must name the boundary: {md}");
    }

    /// `compiler.footprint` exposes witchy's own capability analyzer to witchy
    /// programs (the heart of a self-hosted package manager): it returns the
    /// rights-precise footprint as JSON, which composes with `std/json`.
    #[test]
    fn compiler_footprint_exposes_the_analyzer() {
        if !is_nightly() {
            return;
        }
        // The rights-precise footprint comes back as JSON.
        let footprint = "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"pub fn load(d: Dir[Read]) -> Result(String, CapError):\\n    d.read(\\\"x\\\")\\n\"))\n";
        let out = link_run(footprint);
        assert_eq!(wasm_run(footprint), out, "footprint interpreter/WASM parity");
        assert!(out[0].contains("\"total\":[\"Dir[Read]\"]"), "total wrong: {}", out[0]);
        assert!(out[0].contains("\"name\":\"load\""), "entry missing: {}", out[0]);
        // The output is valid JSON — it round-trips through `std/json`.
        let composed = link_run(
            "import compiler\nimport json\nfn main(console: Console):\n    match json.decode(compiler.footprint(\"pub fn serve(n: Net) -> Int:\\n    0\\n\")):\n        Ok(doc) -> console.print(\"valid\")\n        Err(e) -> console.print(\"invalid: \" + json.decode_error_message(e))\n",
        );
        assert_eq!(composed, vec!["valid"]);
        // `comptime fn` helpers are not runtime entrypoints and must not widen
        // source-string footprint reports even if their helper signature mentions
        // a capability.
        let comptime_helper = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"comptime fn helper(d: Dir[Read]) -> Int:\\n    0\\n\\npub fn visible() -> Int:\\n    1\\n\"))\n",
        );
        assert!(
            !comptime_helper[0].contains("Dir[Read]") && !comptime_helper[0].contains("helper"),
            "comptime helper leaked into footprint: {}",
            comptime_helper[0]
        );
        assert!(
            comptime_helper[0].contains("\"name\":\"visible\""),
            "visible function missing: {}",
            comptime_helper[0]
        );
        // Malformed source degrades to an error object, not a crash.
        let bad = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"fn oops(\"))\n",
        );
        assert!(bad[0].contains("\"error\""), "expected an error object: {}", bad[0]);
    }

    /// `compiler.diff` is the rights-precise block-on-widening gate (the package
    /// manager's core safety check), exposed to witchy as JSON.
    #[test]
    fn compiler_diff_is_the_widening_gate() {
        if !is_nightly() {
            return;
        }
        let diff = |old: &str, new: &str| -> String {
            let source = format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            );
            let interpreted = link_run(&source).remove(0);
            assert_eq!(wasm_run(&source), vec![interpreted.clone()], "compiler.diff parity");
            interpreted
        };
        // A connect-only client that gains `Listen` is a widening (the gate blocks).
        let widen = diff(
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
        );
        assert!(widen.contains("\"widened\":true"), "should widen: {widen}");
        assert!(widen.contains("\"added\":[\"Net[Listen]\"]"), "added wrong: {widen}");
        // The reverse (tightening to connect-only) is a safe narrowing.
        let narrow = diff(
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(narrow.contains("\"widened\":false"), "should not widen: {narrow}");
        assert!(narrow.contains("\"removed\":[\"Net[Listen]\"]"), "removed wrong: {narrow}");
    }

    /// `compiler.diff` includes build-axis and user-cap axes in the JSON report
    /// so self-hosted gates can explain which axis caused a widening.
    #[test]
    fn compiler_diff_includes_build_and_user_cap_axes() {
        if !is_nightly() {
            return;
        }
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // An empty build footprint gaining BuildExec is a build-axis widening.
        let build = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn pure() -> Int:\\n    0\\npub fn build(e: BuildExec) -> Int:\\n    0\\n",
        );
        assert!(build.contains("\"widened\":true"), "build widening should set widened: {build}");
        assert!(build.contains("\"build_added\":[\"BuildExec\"]"), "build_added should list BuildExec: {build}");
        assert!(build.contains("\"build_removed\":[]"), "build_removed should be empty: {build}");
        // Every diff includes all axes.
        let runtime = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(runtime.contains("\"build_added\":[]"), "no-build diff should have empty build_added: {runtime}");
        assert!(runtime.contains("\"user_caps_added\":[]"), "no-user-cap diff should have empty user_caps_added: {runtime}");
        assert!(runtime.contains("\"user_caps_removed\":[]"), "no-user-cap diff should have empty user_caps_removed: {runtime}");
    }

    #[test]
    fn caps_diff_gate_flags_a_widening_across_versions() {
        // The supply-chain gate end-to-end: a dependency update whose public API
        // newly demands `Net` is reported as a widening (so CI/install can block),
        // while an unchanged footprint is not.
        let dir = std::env::temp_dir().join(format!("witchy_capsdiff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = dir.join("v1.witchy");
        let v2 = dir.join("v2.witchy");
        std::fs::write(&v1, r#"
pub fn serve(console: Console) -> Int:
    0
"#).unwrap();
        std::fs::write(
            &v2,
            r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#,
        )
        .unwrap();
        assert!(
            crate::report_capability_diff(v1.to_str().unwrap(), v2.to_str().unwrap()).unwrap(),
            "newly demanding Net must be flagged as a widening"
        );
        assert!(
            !crate::report_capability_diff(v1.to_str().unwrap(), v1.to_str().unwrap()).unwrap(),
            "an unchanged footprint must not be a widening"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
