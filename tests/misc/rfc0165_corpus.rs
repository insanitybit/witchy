//! RFC-0165 B6/B9: the digest-bound foundation corpus. `rfcs/0165-safewir-corpus.json`
//! names every program, its SHA-256, and an independently written expected
//! stdout. Each program must admit through canonical SafeWIR; the SafeWIR
//! oracle, the admitted Wasm, and the expected stdout must agree; nothing
//! may leak. A changed program (or expectation) changes the digest, so the
//! corpus cannot drift silently.
use sha2::{Digest, Sha256};
use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[derive(serde::Deserialize)]
struct Corpus {
    entries: Vec<Entry>,
}

#[derive(serde::Deserialize)]
struct Entry {
    id: String,
    path: String,
    sha256: String,
    expected_stdout: Vec<String>,
}

fn corpus() -> Corpus {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("rfcs/0165-safewir-corpus.json")).expect("corpus manifest");
    serde_json::from_str(&text).expect("corpus manifest parses")
}

fn three_legs(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).unwrap_or_else(|e| panic!("{label}: {e}"));
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("{label}: the foundation subset admits this program"));
    let entry = artifact.entry().unwrap_or_else(|| panic!("{label}: a program has an entry"));
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap_or_else(|e| panic!("{label}: {e}"));
    let arguments = entry.capabilities().iter().map(|_| InterpreterValue::Capability(0)).collect();
    let run = interpret_program(&validated, function, arguments, InterpreterConfig { max_steps: 20_000_000_000 })
        .unwrap_or_else(|e| panic!("{label}: oracle refused: {e:?}"));
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted artifact");
    vm.run().unwrap_or_else(|e| panic!("{label}: wasm run failed: {e}"));
    let wasm_lines = vm.output();
    // The host prints an `Int`/`Float` entry result after the entry returns;
    // the oracle reports it as a result value. Compare it as the last line.
    let mut oracle_lines = run.output.clone();
    match run.results.first() {
        Some(InterpreterValue::Int(n)) => oracle_lines.push(n.to_string()),
        Some(InterpreterValue::FloatBits(bits)) => oracle_lines.push(witchy_syntax::fmt::render_float(f64::from_bits(*bits))),
        _ => {}
    }
    assert_eq!(oracle_lines, wasm_lines, "{label}: the oracle and the admitted wasm printed different lines");
    assert_eq!(run.leaked_objects, 0, "{label}: the oracle saw objects survive the entry");
    let leaks = vm.canonical_heap_leaks().expect("heap audit").expect("canonical module");
    assert_eq!(leaks, 0, "{label}: the wasm heap kept unreleased objects");
    wasm_lines
}

#[test]
fn corpus_digests_are_current() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = corpus();
    assert!(!corpus.entries.is_empty());
    let mut ids = std::collections::BTreeSet::new();
    for entry in &corpus.entries {
        assert!(ids.insert(entry.id.clone()), "duplicate corpus id {}", entry.id);
        let source = std::fs::read(root.join(&entry.path)).unwrap_or_else(|e| panic!("{}: {e}", entry.path));
        let digest = Sha256::digest(&source).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert_eq!(digest, entry.sha256, "{}: the program changed without a manifest update", entry.id);
    }
}

#[test]
fn corpus_programs_admit_and_agree_with_their_expected_output() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in corpus().entries {
        let source = std::fs::read_to_string(root.join(&entry.path)).expect("corpus program");
        let output = three_legs(&source, &entry.id);
        assert_eq!(output, entry.expected_stdout, "{}: output differs from the expected stdout", entry.id);
    }
}
