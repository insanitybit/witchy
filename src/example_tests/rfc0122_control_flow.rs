use super::*;

/// Returning a place selected by control flow must carry the selected runtime
/// place, rather than a copy or the function's temporary return slot.
#[test]
fn conditional_exclusive_reference_return_preserves_each_runtime_place_on_both_backends() {
    let src = "mode opt\n\n\
        type Pair:\n\
            left: Int\n\
            right: Int\n\n\
        fn select(pair: &'a mut Pair, first: Bool) -> &'a mut Int:\n\
            if first:\n\
                &mut pair.left\n\
            else:\n\
                &mut pair.right\n\n\
        fn main(console: Console):\n\
            var first = Pair(1, 2)\n\
            let left = select(&mut first, true)\n\
            *left = 9\n\
            console.print(\"${*left}\")\n\
            var second = Pair(3, 4)\n\
            let right = select(&mut second, false)\n\
            *right = 8\n\
            console.print(\"${*right}\")\n";
    let want = ["9", "8"];

    assert_eq!(link_run(src), want, "interpreter preserves the selected place");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled code preserves the selected place");
}
