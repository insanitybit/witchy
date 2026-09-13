//! Stateless source hashes use a pointer-free request and admitted byte slices.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};

#[test]
fn canonical_crypto_hash_known_answers_and_argument_snapshots_three_legs() {
    let source = r#"import crypto
import list
fn sha256(input: String) -> String:
    "ordinary function"
fn contents_after_change(var paths: List(String)) -> List(String):
    paths = ["changed"]
    ["x"]
fn mutate(console: Console, var paths: List(String)) -> List(String):
    console.print("mutation")
    paths.push("added")
    ["body"]
fn main(console: Console):
    console.print(crypto.sha256(""))
    console.print(crypto.sha256("abc"))
    console.print(crypto.rune_hash([], []))
    console.print(crypto.rune_hash(["b", "a"], ["\r\n", "x"]))
    console.print(crypto.rune_hash(["a", "b"], ["x", "\n"]))
    console.print(crypto.rune_hash(["unicode"], ["RUNE_UNICODE_CONTENT"]))
    console.print(sha256("abc"))
    var paths = ["a"]
    let same = crypto.rune_hash(paths, contents_after_change(paths)) == crypto.rune_hash(["a"], ["x"])
    console.print("${same}")
    console.print(paths[0])
    var incoming = ["entry"]
    console.print(crypto.rune_hash(incoming, mutate(console, incoming)))
    console.print("${list.length(incoming)}")
"#.replace("RUNE_UNICODE_CONTENT", &"\u{e9}".repeat(140));
    let expected = [
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        // Independently computed from u64 LE field lengths, not the u32 wire frame.
        "sha256:f965ae8c321459eb2995837d180168577024a3af3afb3ade01ebb9492ecf538e",
        "sha256:f965ae8c321459eb2995837d180168577024a3af3afb3ade01ebb9492ecf538e",
        // 140 Unicode scalars occupy 280 UTF-8 bytes: length word 18 01 00 00.
        "sha256:e094c3bffd7942387e1ae852a8543d84756f66391260e0c2b7f04f8e304d28c2",
        "ordinary function", "true", "changed",
        "mutation", "sha256:d68bcbb8915b2f4668b82841cdf2e421d935b02fccbc6a92e55f2e20d89558c0", "2",
    ];
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let root = crate::temp_dir::TempDir::new("canonical-crypto-hash");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("source hashes use catalog-authenticated byte slices");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}
