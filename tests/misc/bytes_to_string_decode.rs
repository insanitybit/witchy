//! `bytes.to_string` is Rust's `from_utf8_lossy`: well-formed input decodes
//! to itself (validated first, then copied once), and each maximal malformed
//! subpart becomes one U+FFFD. Expected results come from Rust's decoder.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn output(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    actor.output().to_vec()
}

/// Each case decoded by the compiled program, printed as `length [text]`.
fn assert_decodes(cases: &[&[u8]]) {
    let calls = cases.iter()
        .map(|case| format!("    show(console, [{}])\n", case.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")))
        .collect::<String>();
    let source = format!("import bytes\n\n\
fn show(console: Console, xs: List(Int)):\n    let b: Bytes = bytes.from_list(xs) ?? bytes.repeat(0, 0)\n    let s = b.to_string()\n    console.print(\"${{s.length()}} [${{s}}]\")\n\n\
fn main(console: Console):\n{calls}");
    let expected = cases.iter()
        .map(|case| {
            let text = String::from_utf8_lossy(case);
            format!("{} [{text}]", text.len())
        })
        .collect::<Vec<_>>();
    assert_eq!(output(&source), expected);
}

#[test]
fn well_formed_bytes_decode_to_themselves() {
    assert_decodes(&[b"", b"hi", "h\u{e9}!".as_bytes(), "\u{20ac}\u{1f600}".as_bytes()]);
}

#[test]
fn malformed_lead_and_continuation_bytes_are_replaced() {
    assert_decodes(&[&[0xFF], &[65, 0x80, 66], &[104, 0xC0, 0xAF, 105], &[0xF4, 0x90, 0x80, 0x80, 120]]);
}

#[test]
fn truncated_and_surrogate_sequences_are_replaced() {
    assert_decodes(&[&[0xE2, 0x82], &[0xE2, 0x82, 65], &[0xED, 0xA0, 0x80], &[0xF0, 0x9F, 0x98]]);
}

#[test]
fn a_malformed_byte_after_valid_text_keeps_the_prefix() {
    assert_decodes(&["caf\u{e9} ".as_bytes().iter().copied().chain([0xC3, 33]).collect::<Vec<_>>().as_slice()]);
}
