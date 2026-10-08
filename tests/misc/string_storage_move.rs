//! Storing a string moves its buffer only where the source is dead; every
//! binding that can still be read keeps its own value.

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

#[test]
fn dying_substring_keys_count_every_kmer() {
    let out = output("fn main(console: Console):\n    let seq = \"ACGTACGTAC\"\n    var counts = dict.new()\n    var j = 0\n    while j + 3 <= seq.length():\n        let kmer = seq.substring(j, j + 3)\n        counts.update(kmer, 0, fn(c: Int): c + 1)\n        j = j + 1\n    console.print(\"${counts.length()} ${counts.get_or(\"ACG\", 0)} ${counts.get_or(\"TAC\", 0)}\")\n");
    assert_eq!(out, ["4 2 2"]);
}

#[test]
fn a_key_read_after_insertion_keeps_its_bytes() {
    let out = output("fn main(console: Console):\n    let seq = \"hello world\"\n    var seen = dict.new()\n    let word = seq.substring(0, 5)\n    let _ = seen.insert(word, 1)\n    let louder = word.to_upper()\n    console.print(\"${word} ${louder} ${seen.get_or(\"hello\", 0)}\")\n");
    assert_eq!(out, ["hello HELLO 1"]);
}

#[test]
fn a_pushed_element_read_after_the_push_is_unchanged() {
    let out = output("fn main(console: Console):\n    var kept = []\n    var i = 0\n    while i < 3:\n        let s = \"item${i}\".substring(0, 5)\n        kept.push(s)\n        console.print(s)\n        i = i + 1\n    console.print(\"${kept}\")\n");
    assert_eq!(out, ["item0", "item1", "item2", "[item0, item1, item2]"]);
}

#[test]
fn a_string_stored_twice_owns_two_values() {
    let out = output("fn main(console: Console):\n    let s = \"abcdef\".substring(1, 4)\n    var xs = []\n    xs.push(s)\n    xs.push(s)\n    console.print(\"${xs} ${xs[0] == xs[1]}\")\n");
    assert_eq!(out, ["[bcd, bcd] true"]);
}
