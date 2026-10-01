use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn custom_hash_keys_and_collisions_run_on_admitted_wasm() {
    // Key equality ignores the cache field. Keys 1 and 5 deliberately collide
    // under Hash, while a different cache value must still find the same row.
    let source = r#"
import cmp
import dict

type Key:
    id: Int
    cache: Int

impl PartialEq for Key:
    fn eq(self, other: Self) -> Bool:
        self.id == other.id

impl Eq for Key

impl Hash for Key:
    fn hash(let self) -> Int:
        self.id % 4

fn main(console: Console):
    var d: Dict(Key, Int) = dict.new()
    let _ = d.insert(Key(1, 10), 100)
    let _ = d.insert(Key(5, 50), 500)
    let _ = d.insert(Key(1, 99), 101)
    console.print("${d.at(Key(1, 0)) + d.at(Key(5, 0))}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("Hash key checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("canonical admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["601".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn eq_only_key_is_rejected_at_the_operation() {
    let source = r#"
import cmp
import dict

type Key derive(Eq):
    id: Int

fn main():
    var d: Dict(Key, Int) = dict.new()
    let _ = d.insert(Key(7), 42)
"#;
    let error = witchy::resolve_std_only_checked(source).err()
        .expect("Eq alone cannot key a Dict").to_string();
    assert!(error.contains("`insert` requires `Key: Hash`"), "{error}");
}
