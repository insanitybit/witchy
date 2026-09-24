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
    let root = super::manifest_dir();
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
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let run = interpret_program(&validated, function, arguments, InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() })
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
    let root = super::manifest_dir();
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

/// Admit every corpus program whose manifest index is `shard` modulo
/// `CORPUS_SHARDS` and check it against its expected stdout on all three legs.
///
/// One test ran all 43 programs in sequence: the slowest test in the gate
/// (55s with a cold verdict store), and a serial tail no amount of nextest
/// parallelism could split. Sharding by index keeps the manifest the only
/// list of programs -- a new entry lands in some shard automatically.
fn assert_corpus_shard(shard: usize) {
    let root = super::manifest_dir();
    for (_, entry) in corpus().entries.into_iter().enumerate()
        .filter(|(index, _)| index % CORPUS_SHARDS == shard)
    {
        let source = std::fs::read_to_string(root.join(&entry.path)).expect("corpus program");
        let output = three_legs(&source, &entry.id);
        assert_eq!(output, entry.expected_stdout, "{}: output differs from the expected stdout", entry.id);
    }
}

/// Defines one test per shard and `CORPUS_SHARDS` as their count, so the two
/// cannot drift apart and silently skip programs.
macro_rules! corpus_shards {
    ($($name:ident = $shard:literal),+ $(,)?) => {
        const CORPUS_SHARDS: usize = [$($shard),+].len();

        #[test]
        fn corpus_shards_are_exactly_zero_through_count() {
            let mut shards = vec![$($shard),+];
            shards.sort_unstable();
            assert_eq!(shards, (0..CORPUS_SHARDS).collect::<Vec<usize>>(),
                "a missing or duplicated shard number would skip corpus programs");
        }
        $(
            #[test]
            fn $name() {
                assert_corpus_shard($shard);
            }
        )+
    };
}

corpus_shards! {
    corpus_programs_agree_shard_00 = 0, corpus_programs_agree_shard_01 = 1,
    corpus_programs_agree_shard_02 = 2, corpus_programs_agree_shard_03 = 3,
    corpus_programs_agree_shard_04 = 4, corpus_programs_agree_shard_05 = 5,
    corpus_programs_agree_shard_06 = 6, corpus_programs_agree_shard_07 = 7,
    corpus_programs_agree_shard_08 = 8, corpus_programs_agree_shard_09 = 9,
    corpus_programs_agree_shard_10 = 10, corpus_programs_agree_shard_11 = 11,
    corpus_programs_agree_shard_12 = 12, corpus_programs_agree_shard_13 = 13,
    corpus_programs_agree_shard_14 = 14, corpus_programs_agree_shard_15 = 15,
}
