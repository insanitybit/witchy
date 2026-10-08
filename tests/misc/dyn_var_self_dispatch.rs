//! A `var self` trait method called through `dyn Trait` mutates the boxed
//! payload and writes it back into the receiver, for a local, a `var`
//! parameter, and a record field, across more than one implementation.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const COUNTERS: &str = "trait Counter:\n    fn bump(var self, by: Int) -> Int\n    fn total(self) -> Int\n\n\
type Tally:\n    n: Int\n\n\
type Log:\n    items: List(Int)\n\n\
impl Counter for Tally:\n    fn bump(var self, by: Int) -> Int:\n        self.n = self.n + by\n        self.n\n    fn total(self) -> Int:\n        self.n\n\n\
impl Counter for Log:\n    fn bump(var self, by: Int) -> Int:\n        self.items.push(by)\n        self.items.length()\n    fn total(self) -> Int:\n        self.items.fold(0, fn(acc, x): acc + x)\n\n";

fn output(main: &str) -> Vec<String> {
    let source = format!("{COUNTERS}{main}");
    let checked = witchy::resolve_std_only_checked(&source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
    actor.output().to_vec()
}

#[test]
fn var_self_dispatch_writes_back_locals_and_var_parameters() {
    let out = output("fn drive(var c: dyn Counter, n: Int) -> Int:\n    var i = 0\n    var last = 0\n    while i < n:\n        last = c.bump(i)\n        i = i + 1\n    last\n\n\
fn main(console: Console):\n    var a: dyn Counter = Tally(0)\n    var b: dyn Counter = Log([])\n    let la = drive(a, 5)\n    let lb = drive(b, 5)\n    let before = a.total()\n    let again = a.bump(100)\n    console.print(\"${la} ${lb} ${before} ${a.total()} ${b.total()} ${again}\")\n");
    assert_eq!(out, ["10 5 10 110 10 110"]);
}

#[test]
fn var_self_dispatch_writes_back_a_record_field() {
    let out = output("type Holder:\n    c: dyn Counter\n\n\
fn main(console: Console):\n    var h = Holder(Tally(0))\n    let x = h.c.bump(1)\n    let y = h.c.bump(1)\n    console.print(\"${x} ${y} ${h.c.total()}\")\n");
    assert_eq!(out, ["1 2 2"]);
}

#[test]
fn var_self_dispatch_on_an_indexed_receiver_names_the_rewrite() {
    let source = format!("{COUNTERS}fn main(console: Console):\n    var xs: List(dyn Counter) = [Tally(0)]\n    let i = 0\n    console.print(\"${{xs[i].bump(1)}}\")\n");
    let error = match witchy::resolve_std_only_checked(&source) {
        Err(error) => format!("{error:?}"),
        Ok(checked) => match codegen::compile_checked_module_artifact(&checked) {
            codegen::LoweringOutcome::Lowered(_) => panic!("an indexed `var self` receiver is refused"),
            codegen::LoweringOutcome::Unsupported(reason) => reason.to_string(),
            codegen::LoweringOutcome::Rejected(error) => error.to_string(),
        },
    };
    assert!(error.contains("needs a variable or field receiver"), "{error}");
}
