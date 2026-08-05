use super::*;

    /// RFC-0090 makes proper self-tail calls a language guarantee, not an
    /// optimizer choice. This depth is far beyond either backend's ordinary
    /// call stack, and the argument swap pins simultaneous parameter rebinding.
    #[test]
    fn proper_self_tail_calls_use_constant_stack_on_both_backends() {
        let src = r#"
fn swap_down(n: Int, a: Int, b: Int) -> Int:
    match n:
        0 -> ((a * 10) + b)
        _ -> swap_down((n - 1), b, a)

fn main(console: Console):
    console.print("${swap_down(5000001, 2, 7)}")
"#;
        let want = vec!["72".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled WIR loop");
    }

    /// RFC-0090 direct recursive components use one typed dispatcher. Different
    /// source signatures occupy disjoint banks, and every edge still stages its
    /// arguments before changing logical functions.
    #[test]
    fn proper_mutual_tail_calls_use_constant_stack_on_both_backends() {
        let src = r#"
fn left(own n: Int, a: Int, b: Int) -> Int:
    match n:
        0 -> ((a * 10) + b)
        _ -> right((n - 1), b, a, "right")

fn right(own n: Int, a: Int, b: Int, label: String) -> Int:
    if n == 0:
        return (a * 10) + b
    return left((n - 1), b, a)

fn main(console: Console):
    console.print("${left(250001, 2, 7)}")
"#;
        let want = vec!["72".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter SCC trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled WIR SCC dispatcher");
    }

    /// Tail lowering must preserve the ordinary call dispatcher. Stdlib
    /// intrinsic declarations are recursive placeholders, not executable
    /// recursion, including when reached from a function value.
    #[test]
    fn proper_tail_calls_preserve_intrinsic_dispatch_on_both_backends() {
        let src = r#"
import list
import vm

fn upper(s: String) -> String:
    s.to_upper()

fn parallel_once(xs: List(Int)) -> List(Int):
    vm.par_map(xs, fn(n: Int): n + 1)

fn invoke(f: fn(List(Int)) -> List(Int), xs: List(Int)) -> List(Int):
    f(xs)

fn main(console: Console):
    console.print(upper("witchy"))
    let shouted = ["a", "b"].map(fn(s: String): s.to_upper())
    console.print(shouted.join("-"))
    console.print("${invoke(parallel_once, [1, 2])}")
"#;
        let want = vec![
            "WITCHY".to_string(),
            "A-B".to_string(),
            "[2, 3]".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter builtin dispatch");
        assert_eq!(run_on_wasm(src), want, "compiled builtin dispatch");
    }

    /// Generic templates and bounded trait methods are specialized before WIR
    /// proper-call lowering, so their concrete recursive edges use the same loops.
    #[test]
    fn specialized_generic_and_trait_tail_calls_are_proper_on_both_backends() {
        let src = r#"
fn keep(value: a, n: Int) -> a:
    if n == 0:
        value
    else:
        keep(value, n - 1)

trait Countdown:
    fn down(self, n: Int) -> Int

type Counter:
    value: Int

impl Countdown for Counter:
    fn down(self, n: Int) -> Int:
        if n == 0:
            self.value
        else:
            self.down(n - 1)

fn bounded(value: a, n: Int) -> Int where a: Countdown:
    value.down(n)

fn main(console: Console):
    console.print(keep("generic", 100001))
    console.print("${bounded(Counter(11), 100001)}")
"#;
        let want = vec!["generic".to_string(), "11".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter specialized trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled specialized loops");
    }
