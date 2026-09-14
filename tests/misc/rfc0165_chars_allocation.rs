//! Dynamic string.chars must not allocate/copy every preceding list prefix.
//! These are source programs, not hand-built allocator or encoding kernels.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

// 16 MiB is the existing encoding fixture's native memory fence. A live chars
// result for 32 Ki ASCII scalars fits comfortably; exact-length prefix rebuilding
// requests more than 4 GiB cumulatively before accounting for the child Strings.
const MEMORY_PAGES: usize = 256;

fn three_legs_under_memory_cap(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("chars source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes chars and real encoding wrappers");
    assert_eq!(source_output, expected, "independent source expectations");

    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
            let arguments = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            let result = interpret_entry_program(
                validated, function, arguments, &[],
                InterpreterConfig { max_steps: 100_000_000, ..Default::default() },
            ).expect("independent canonical oracle executes chars and encoding");
            assert_eq!(result.output, expected, "oracle values, not allocator parity");
            assert_eq!(result.leaked_objects, 0, "oracle retains no owned objects");
        },
    ).expect_lowered("checked chars and public encoding source admit");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact, Capabilities { print: true, quiet: true, ..Capabilities::none() }, MEMORY_PAGES,
    ).expect("admitted source instantiates within the memory fence");
    vm.run().expect("dynamic chars and decoding complete within 16 MiB");
    assert_eq!(vm.output(), expected, "bounded native execution preserves all values");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn checked_chars_allocation_ascii_and_public_decoder_fit_bounded_memory() {
    // Runtime doubling deliberately avoids lower_string_chars' literal-only
    // pre-sized path. Keep the chars result live across the public decoder,
    // which must still run its real base64-valid wrapper over the same input.
    three_legs_under_memory_cap(r#"
import bytes
import encoding

fn main(console: Console):
    var encoded = "AAAA"
    var rounds = 0
    while rounds < 13:
        encoded = encoded + encoded
        rounds = rounds + 1
    console.print("input ${encoded.length()}")
    let characters = encoded.chars()
    console.print("chars ${characters.length()} ${characters[0]} ${characters[32767]}")
    match encoding.base64_decode_bytes_string(encoded):
        Ok(data) -> console.print("decoded ${data.length()} ${bytes.at(data, 0)} ${bytes.at(data, 24575)}")
        Err(_) -> console.print("unexpected decode error")
    console.print("retained ${characters.length()} ${characters[32767]}")
"#, &["input 32768", "chars 32768 A A", "decoded 24576 0 0", "retained 32768 A"]);
}

#[test]
fn checked_chars_allocation_utf8_snapshots_and_invalid_base64_preserve_values() {
    // Includes one-, two-, three- and four-byte UTF-8 scalars, then a combining
    // mark as a distinct scalar (not grapheme segmentation). The snapshot is a
    // negative control against treating a shared list as a private accumulator.
    three_legs_under_memory_cap(r#"
import bytes
import encoding

fn main(console: Console):
    var text = "Aé→🙂é"
    var rounds = 0
    while rounds < 6:
        text = text + text
        rounds = rounds + 1
    let original = text
    var characters = text.chars()
    let snapshot = characters
    let retained = snapshot[1]
    characters[1] = "X"
    characters.push("!")
    text = "replaced"
    console.print("snapshot ${snapshot.length()} ${snapshot[0]} ${snapshot[1]} ${snapshot[2]} ${snapshot[3]}")
    console.print("tail " + snapshot[382] + "|" + snapshot[383])
    console.print("mutated ${characters.length()} ${characters[1]} ${characters[384]}")
    console.print("retained " + retained)
    console.print("original ${original.chars().length()} changed " + text)
    let empty = ""
    console.print("empty ${empty.chars().length()}")
    let decoded = encoding.base64_decode_bytes_string("w6nwn5mC")
    for invalid in ["A", "A=AA", "A===", "é", "AA🙂A"]:
        match encoding.base64_decode_bytes_string(invalid):
            Ok(_) -> console.print("unexpected decode success")
            Err(_) -> console.print("invalid")
    match decoded:
        Ok(data) -> console.print("decoded " + bytes.to_string_lossy(data))
        Err(_) -> console.print("unexpected retained decode error")
"#, &[
        "snapshot 384 A é → 🙂", "tail e|́", "mutated 385 X !", "retained é",
        "original 384 changed replaced", "empty 0",
        "invalid", "invalid", "invalid", "invalid", "invalid", "decoded é🙂",
    ]);
}
