//! A record field declared `unique` (RFC-0167 "Unique owned storage") makes
//! the record affine and its field uniquely owned, so a `var self` method
//! updates the field in place. Reads borrow the field; value semantics hold.

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

const BAG: &str = "type Bag:\n    items: unique List(Int)\n    n: Int\n\n\
impl Bag:\n    pub fn add(var self, x: Int):\n        self.items.push(x)\n        self.n = self.n + 1\n\n    pub fn total(let self) -> Int:\n        var t = 0\n        for x in self.items:\n            t = t + x\n        t\n\n";

#[test]
fn a_unique_field_is_copied_from_a_readable_source() {
    let out = output(&format!("{BAG}fn main(console: Console):\n    let xs = [1, 2]\n    var b = Bag(xs, 0)\n    b.add(3)\n    b.add(4)\n    let first = b.items[0]\n    console.print(\"${{xs}} ${{b.n}} ${{first}} ${{b.total()}}\")\n    let c = move b\n    console.print(\"${{c.items.length()}}\")\n"));
    assert_eq!(out, ["[1, 2] 2 1 10", "4"]);
}

#[test]
fn a_var_self_method_grows_a_unique_list_field() {
    let out = output(&format!("{BAG}fn main(console: Console):\n    var b = Bag([], 0)\n    var i = 0\n    while i < 2000:\n        b.add(i)\n        i = i + 1\n    console.print(\"${{b.items.length()}} ${{b.n}} ${{b.items.at(1999)}} ${{b.total()}}\")\n"));
    assert_eq!(out, ["2000 2000 1999 1999000"]);
}

#[test]
fn a_unique_dict_field_updates_on_one_branch() {
    let out = output("type Index:\n    counts: unique Dict(Int, Int)\n    order: unique List(Int)\n\n\
impl Index:\n    pub fn see(var self, k: Int):\n        if !self.counts.contains_key(k):\n            self.order.push(k)\n        self.counts.update(k, 0, fn(c: Int) -> Int: c + 1)\n\n    pub fn count(let self, k: Int) -> Int:\n        self.counts.get_or(k, 0)\n\n\
fn main(console: Console):\n    var ix = Index(dict.new(), [])\n    var i = 0\n    while i < 300:\n        ix.see(i % 7)\n        i = i + 1\n    console.print(\"${ix.order.length()} ${ix.count(0)} ${ix.count(6)} ${ix.order.at(6)}\")\n");
    assert_eq!(out, ["7 43 42 6"]);
}

#[test]
fn a_unique_qualified_receiver_updates_in_place() {
    let out = output("type Bag:\n    items: unique List(Int)\n\n\
impl Bag:\n    pub fn add(var self: unique Bag, x: Int):\n        self.items.push(x)\n\n\
fn main(console: Console):\n    var b = Bag([])\n    b.add(5)\n    b.add(6)\n    console.print(\"${b.items.length()} ${b.items.at(1)}\")\n");
    assert_eq!(out, ["2 6"]);
}
