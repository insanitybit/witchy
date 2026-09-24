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

