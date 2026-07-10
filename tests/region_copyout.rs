//! Reclamation tests for pointer-valued `region:` results.

use witchy::stats;

#[test]
fn recursive_adt_region_result_is_copied_out() {
    let source = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn main(console: Console):\n    let stack = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    console.print(__render(stack == Push(1, Push(2, Empty))))\n";

    let result = stats::compute(source).expect("compile and run recursive ADT region");
    assert_eq!(result.output, ["true"]);
    assert_eq!(
        result.region_copy_bytes, 44,
        "two 20-byte Push blocks and one 4-byte Empty block must be copied out",
    );
}

#[test]
fn dict_region_result_recursively_copies_keys_and_values() {
    let source = "fn main(console: Console):\n    let values = region -> Dict(String, List(Int)):\n        var xs = []\n        xs = list.push(xs, 1)\n        xs = list.push(xs, 2)\n        var key = \"\"\n        key = key + \"a\"\n        key = key + \"b\"\n        var out = dict.new()\n        out = dict.insert(out, key, xs)\n        out\n    var noise = []\n    for i in 0..100:\n        noise = list.push(noise, i)\n    console.print(__render(list.length(dict.get_or(values, \"ab\", []))))\n";

    let result = stats::compute(source).expect("compile and run Dict region");
    assert_eq!(result.output, ["2"]);
    assert_eq!(
        result.region_copy_bytes, 50,
        "the 24-byte Dict, 6-byte key, and 20-byte list value must all be copied",
    );
}
