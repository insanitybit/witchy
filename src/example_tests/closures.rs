use super::*;
use crate::{codegen, interpreter, parser, typeck};

    #[test]
    fn closures_capturing_loop_var_backends_agree() {
        // Closures created in a loop each capture that iteration's value of the
        // loop variable (by value), are stored in a list, and called back. Both
        // backends agree — no shared-loop-variable surprise.
        let src = r#"
fn main(console: Console):
    var fs = []
    for i in [1, 2, 3]:
        list.push(fs, fn(x: Int): (x + i))
    let f0 = list.at(fs, 0)
    let f2 = list.at(fs, 2)
    console.print("${f0(10)}")
    console.print("${f2(10)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["11", "13"]);
    }

    #[test]
    fn closure_capturing_closure_backends_agree() {
        // A closure that captures another closure and calls it through a
        // higher-order function. Both backends agree.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let g = fn(x: Int): (x + 1)
    let h = fn(y: Int): (apply(g, y) * 2)
    console.print("${apply(h, 5)}")
    console.print("${apply(h, 20)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
    }

    /// Pushing onto a list of closures leaves an earlier alias of the list
    /// unchanged.
    #[test]
    fn closure_list_push_keeps_aliases() {
        let src = r#"fn main(console: Console):
    let k = 10
    var fs = [fn(x: Int) -> Int: x + k]
    let snapshot = fs
    fs.push(fn(x: Int) -> Int: x * 100)
    var sum = 0
    for f in fs:
        sum = sum + f(1)
    console.print("${snapshot.length()} ${fs.length()} ${sum}")
"#;
        assert_eq!(run_on_wasm(src), vec!["1 2 111"]);
    }

    /// A list of closures carried through a loop, aliased mid-loop and pushed
    /// a bound closure each iteration, keeps value semantics for the alias.
    #[test]
    fn closure_list_loop_alias_keeps_value_semantics() {
        let src = r#"fn main(console: Console):
    let k = 10
    let add = fn(x: Int) -> Int: x + k
    var fs = [add]
    var snapshots = 0
    var first = [add]
    for i in [1, 2, 3]:
        if i == 2:
            first = fs
        fs.push(add)
        snapshots = snapshots + first.length()
    var sum = 0
    for f in fs:
        sum = sum + f(1)
    console.print("${first.length()} ${fs.length()} ${snapshots} ${sum}")
"#;
        assert_eq!(run_on_wasm(src), vec!["2 4 5 44"]);
    }

    /// A closure list pushed in a loop and also read, captured and iterated
    /// there stays a shared loop lane: each of those uses aliases the token.
    #[test]
    fn closure_list_read_in_loop_after_push() {
        let src = r#"fn main(console: Console):
    var fs = [fn(x: Int) -> Int: x + 1]
    var i = 0
    var total = 0
    while i < 3:
        fs.push(fn(x: Int) -> Int: x * 2)
        total = total + fs[0](i)
        let g = fn(x: Int) -> Int: fs.length() + x
        total = total + g(0)
        for f in fs:
            total = total + f(1)
        i = i + 1
    console.print("${total} ${fs.length()}")
"#;
        assert_eq!(run_on_wasm(src), vec!["33 4"]);
    }

    /// A function may return a `unique` list of closures, and that list
    /// passes to and from other functions with value semantics.
    #[test]
    fn unique_closure_list_crosses_function_boundaries() {
        let src = r#"fn repeated(f: fn(Int) -> Int, n: Int) -> unique List(fn(Int) -> Int):
    var out = []
    for _ in list.range(n):
        out.push(f)
    out

fn total(fs: List(fn(Int) -> Int), x: Int) -> Int:
    var sum = 0
    for f in fs:
        sum = sum + f(x)
    sum

fn main(console: Console):
    let k = 10
    var fs = repeated(fn(x: Int) -> Int: x + k, 3)
    let snapshot = fs
    fs.push(fn(x: Int) -> Int: x * 100)
    let mapped = [1, 2].map(fn(n: Int) -> fn(Int) -> Int: fn(x: Int) -> Int: x * n)
    console.print("${snapshot.length()} ${fs.length()} ${total(snapshot, 1)} ${total(fs, 1)} ${total(mapped, 5)}")
"#;
        assert_eq!(run_on_wasm(src), vec!["3 4 33 133 15"]);
    }
