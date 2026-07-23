use super::*;
use crate::{interpreter, parser, typeck};

    /// `caps_audit` is a capability auditor written *in witchy*: it reads a source
    /// file (`Dir[Read]`), computes its footprint via `compiler.footprint`, parses
    /// the JSON with `std/json`, and prints the total — a self-hosted slice of
    /// `witchy caps`, proving the toolchain is usable from within the language.
    #[test]
    fn caps_audit_example_audits_a_rune_in_witchy() {
        assert_eq!(
            crate::execute_file("examples/caps_audit/src/caps_audit.witchy", Vec::new()).unwrap(),
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        // The auditor itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/caps_audit/src/caps_audit.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `caps_guard` is the supply-chain gate written *in witchy*: it reads two
    /// versions of a rune, asks `compiler.diff` whether the new one widens the
    /// footprint, prints a BLOCK/OK verdict, AND exits non-zero on a widening
    /// (the sample upgrade adds `Listen`, so it BLOCKs and exits 2 — wireable into
    /// CI). The whole gate is self-hosted.
    #[test]
    fn caps_guard_example_blocks_a_widening_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/caps_guard/src/caps_guard.witchy", Vec::new(), Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(output, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
        let src = std::fs::read_to_string("examples/caps_guard/src/caps_guard.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// The strict decode the PM widening gate uses for a VERIFIED signed record's
    /// `runtime_footprint` (BUG-499): a well-formed array decodes; malformed JSON,
    /// a missing/non-array field, or a non-string element must be an `Err`, never a
    /// silent empty list (which the gate would read as "demands no authority" and
    /// wave the upgrade through). A genuinely-empty array is still `Ok([])`. This
    /// pins the contract on both backends; `pm.strict_footprint` implements it.
    #[test]
    fn pm_strict_footprint_decode_contract_backends_agree() {
        let src = r#"import json
import list
from json import Json

type FootprintError:
    NotJson
    MissingField
    NotArray
    NonStringElement

fn footprint_error_message(e: FootprintError) -> String:
    match e:
        NotJson -> "not valid JSON"
        MissingField -> "no field"
        NotArray -> "not an array"
        NonStringElement -> "non-string element"

fn strict_footprint(record: String) -> Result(List(String), FootprintError):
    match json.decode(record):
        Err(_e) -> Err(NotJson)
        Ok(doc) ->
            match json.get(doc, "runtime_footprint"):
                None -> Err(MissingField)
                Some(field) ->
                    match json.as_array(field):
                        None -> Err(NotArray)
                        Some(items) -> strict_strings(items)

fn strict_strings(items: List(Json)) -> Result(List(String), FootprintError):
    var out = []
    for it in items:
        match json.as_string(it):
            None -> return Err(NonStringElement)
            Some(s) -> list.push(out, s)
    Ok(out)

fn show(r: Result(List(String), FootprintError)) -> String:
    match r:
        Ok(xs) -> "ok:[" + list.join(xs, ",") + "]"
        Err(e) -> "err:" + footprint_error_message(e)

fn tag(record: String) -> String:
    match strict_footprint(record):
        Ok(_xs) -> "ok"
        Err(e) ->
            match e:
                NotJson -> "typed:not-json"
                MissingField -> "typed:missing"
                NotArray -> "typed:not-array"
                NonStringElement -> "typed:non-string"

fn main(console: Console):
    console.print(show(strict_footprint("{\"runtime_footprint\":[\"Net[Connect]\",\"Dir[Read]\"]}")))
    console.print(show(strict_footprint("{not json")))
    console.print(show(strict_footprint("{\"other\":1}")))
    console.print(show(strict_footprint("{\"runtime_footprint\":\"Net\"}")))
    console.print(show(strict_footprint("{\"runtime_footprint\":[\"Net\",5]}")))
    console.print(show(strict_footprint("{\"runtime_footprint\":[]}")))
    console.print(tag("{\"runtime_footprint\":[\"Net\",5]}"))
"#;
        let want = vec![
            "ok:[Net[Connect],Dir[Read]]",
            "err:not valid JSON",
            "err:no field",
            "err:not an array",
            "err:non-string element",
            "ok:[]",
            "typed:non-string",
        ];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `coven_check` is the package manager's `check_declared`, self-hosted: it
    /// reads a rune's `witchy.toml` (`std/toml`) and its source, asks the compiler
    /// what the code demands (`compiler.footprint`), and verifies the manifest's
    /// `[capabilities]` admits every demanded cap *rights-precisely*. The sample
    /// manifest admits `Net[Connect]`, but the code demands full `Net` (it also
    /// listens), so it flags the under-declaration and exits 1 even though the
    /// `Net` *kind* is declared — the case a kind-level check would miss.
    #[test]
    fn coven_check_example_flags_under_declared_manifest_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/coven_check/src/coven_check.witchy", Vec::new(), Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(
            output,
            vec!["UNDER-DECLARED: code demands Net not admitted by [capabilities]"]
        );
        assert_eq!(code, 1, "an under-declared manifest must exit 1");
        // The checker itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/coven_check/src/coven_check.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `projects/pm` is the package manager itself, written in witchy. `pm audit`
    /// prints the capability footprint a source file demands — the self-hosted
    /// `witchy caps`, dispatched from a real CLI (`args: List(String)`).
    fn linked_pm() -> crate::ast::Module {
        let sources = [
            ("pm", "projects/pm/src/pm.witchy"),
            ("coven_proto", "projects/coven/src/coven_proto.witchy"),
            ("coven_json", "projects/coven/src/coven_json.witchy"),
            ("coven_validate", "projects/coven/src/coven_validate.witchy"),
        ];
        let modules = sources
            .iter()
            .map(|(name, path)| {
                let src = std::fs::read_to_string(path).expect("read pm module");
                ((*name).to_string(), parser::parse_module(&src).expect("parse pm module"))
            })
            .collect();
        crate::pipeline::link(modules, "pm").expect("link pm")
    }

    fn run_pm(root: impl AsRef<std::path::Path>, args: Vec<String>) -> (Vec<String>, i32) {
        let linked = linked_pm();
        typeck::check(&linked).expect("typeck pm");
        interpreter::run_module_exit(linked, root, Vec::new(), args, None).expect("run pm")
    }

    #[test]
    fn pm_audits_a_files_footprint() {
        let (out, code) = run_pm(".", vec!["audit".into(), "examples/data/sample_rune.witchy".into()]);
        assert_eq!(
            out,
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        assert_eq!(code, 0);
        // pm reads/writes project files, prints, `add` fetches over the network,
        // `run` drives the compiler via Exec, `publish` reads COVEN_ID_TOKEN via
        // Env, and `add`'s staging-cooldown gate reads the wall clock (Clock) —
        // Clock, Console, Dir, Env, Exec, Net. `compiler.*` is a host introspection
        // intrinsic, not a runtime capability.
        let src = std::fs::read_to_string("projects/pm/src/pm.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Clock, Console, Dir, Env, Exec, Net");
    }

    /// `pm guard <old> <new>` is the supply-chain gate: it asks `compiler.diff`
    /// whether the upgrade widens authority and exits 2 on a widening (wireable
    /// into CI). The sample upgrade adds `Listen`, so it BLOCKs.
    #[test]
    fn pm_guard_blocks_a_widening() {
        let (out, code) = run_pm(
            ".",
            vec![
                "guard".into(),
                "examples/data/sample_rune.witchy".into(),
                "examples/data/sample_rune_v2.witchy".into(),
            ],
        );
        assert_eq!(out, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
    }

    /// `pm check <dir>` recomputes a rune's footprint from source and fails if the
    /// manifest's `[capabilities]` does not admit it — rights-precisely. The
    /// `leaky` fixture declares only `Console` but its code reads files, so the
    /// undeclared `Dir[Read]` is caught and the gate exits 2.
    #[test]
    fn pm_check_blocks_an_under_declared_rune() {
        let (out, code) = run_pm(".", vec!["check".into(), "projects/pm/tests/fixtures/leaky".into()]);
        assert_eq!(
            out,
            vec!["BLOCK: code demands authority not admitted by [capabilities]: Dir[Read]"]
        );
        assert_eq!(code, 2, "an under-declared rune must exit 2");
    }

    /// pm passes its *own* `check`: its manifest declares exactly `Console, Dir`,
    /// which is what the code demands — the package manager is consistent with
    /// itself, proving the self-hosted gate is honest.
    #[test]
    fn pm_passes_its_own_check() {
        let (out, code) = run_pm(".", vec!["check".into(), "projects/pm".into()]);
        assert_eq!(out, vec!["OK: declared footprint admits the code, nothing unused"]);
        assert_eq!(code, 0);
    }

    /// `pm new <name>` scaffolds a runnable rune (manifest + src stub) using the
    /// *write* Dir capability, confined to the workspace root. The scaffold is
    /// real: the generated rune both passes its own `check` and runs.
    #[test]
    fn pm_new_scaffolds_a_runnable_rune() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_new_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let linked = linked_pm();
        typeck::check(&linked).expect("typeck");
        let (out, code) = interpreter::run_module_exit(
            linked,
            &tmp,
            Vec::new(),
            vec!["new".into(), "widget".into()],
            None,
        )
        .expect("run");
        assert_eq!(code, 0);
        assert!(out.iter().any(|l| l.contains("created rune `widget`")));

        let manifest = std::fs::read_to_string(tmp.join("widget/witchy.toml"))
            .expect("manifest was written");
        assert!(manifest.contains("name = \"widget\""));
        assert!(manifest.contains("runtime = [\"Console\"]"));
        let src = std::fs::read_to_string(tmp.join("widget/src/widget.witchy"))
            .expect("src stub was written");
        assert!(src.contains("hello from widget"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `pm deps <dir>` lists a rune's dependencies and their source — read
    /// straight from `[dependencies]`'s inline tables (`toml.table`/`inline_get`).
    #[test]
    fn pm_lists_dependencies() {
        let (out, code) = run_pm(".", vec!["deps".into(), "examples/projects/ledger/ledger".into()]);
        assert_eq!(out, vec!["money -> path:../money"]);
        assert_eq!(code, 0);
    }

    /// `pm info <dir>` summarizes a rune: name, version, declared vs. recomputed
    /// footprint. Run on the pm itself — its declared `[capabilities]` exactly
    /// match what the code demands (Console, Dir, Net), the self-consistency the
    /// `check` gate enforces.
    #[test]
    fn pm_info_summarizes_a_rune() {
        let (out, code) = run_pm(".", vec!["info".into(), "projects/pm".into()]);
        assert_eq!(
            out,
            vec![
                "name:     pm",
                "version:  0.1.0",
                "declared: Console, Dir, Net, Exec, Env, Clock",
                "actual:   Clock, Console, Dir, Env, Exec, Net",
            ]
        );
        assert_eq!(code, 0);
    }

    /// The interop milestone: `pm verify` recomputes each dependency's content
    /// hash and checks it against the *committed, coven-generated* `witchy.lock`.
    /// It passes — the self-hosted pm's hashing is byte-identical to coven's
    /// store, so a witchy-checked lock and a coven-written one agree.
    #[test]
    fn pm_verify_validates_a_coven_generated_lockfile() {
        let (out, code) = run_pm(".", vec!["verify".into(), "examples/projects/ledger/ledger".into()]);
        assert_eq!(out, vec!["OK: every locked hash matches the dependency sources"]);
        assert_eq!(code, 0);
    }

    /// `pm gate` is the supply-chain gate: after a dependency is locked, an edit
    /// to its source that *widens* its capability footprint is BLOCKed (exit 2),
    /// with the new authority attributed to the rune that introduced it.
    /// Explicitly accepting those caps (like `--allow-cap`) folds them into the
    /// baseline and clears the block.
    #[test]
    fn pm_gate_blocks_a_dependency_that_widens_authority() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_gate_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("app/src")).unwrap();
        std::fs::create_dir_all(tmp.join("lib/src")).unwrap();
        std::fs::write(
            tmp.join("app/witchy.toml"),
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"lib\" = { path = \"../lib\" }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("app/src/app.witchy"),
            "fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/witchy.toml"),
            "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        // lib starts pure: no capabilities.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s\n",
        )
        .unwrap();

        let run_pm = |args: Vec<String>| -> (Vec<String>, i32) {
            let linked = linked_pm();
            typeck::check(&linked).expect("typeck");
            interpreter::run_module_exit(linked, &tmp, Vec::new(), args, None).expect("run")
        };

        run_pm(vec!["lock".into(), "app".into()]);
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        // lib's source widens to demand Console + Net — gate must BLOCK and name lib.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn main(console: Console, net: Net):\n    let s = net.connect(\"example.com:80\")\n    console.print(\"connected\")\n",
        )
        .unwrap();
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(
            out,
            vec![
                "BLOCK: dependencies demand new authority: Console, Net",
                "  Console <- lib",
                "  Net <- lib",
            ]
        );
        assert_eq!(code, 2, "a widening dependency must exit 2");

        // Accepting both new caps clears the gate.
        let (out, code) = run_pm(vec![
            "gate".into(),
            "app".into(),
            "Console".into(),
            "Net".into(),
        ]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
