use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn borrowed_equivalent_queries_find_colliding_keys_on_admitted_wasm() {
    let source = r#"
import cmp
import dict
import string

type Key:
    id: Int
    cache: Int

type Probe:
    id: Int

impl PartialEq for Key:
    fn eq(let self, let other: Self) -> Bool:
        self.id == other.id
impl Eq for Key
impl Hash for Key:
    fn hash(let self) -> Int:
        self.id % 4
impl Equivalent(Key) for Probe:
    fn equivalent(self: &Probe, key: &Key) -> Bool:
        (*self).id == (*key).id
    fn equivalent_hash(self: &Probe) -> Int:
        (*self).id % 4

fn main(console: Console):
    var d: Dict(Key, Int) = dict.new()
    let _ = dict.insert(d, Key(1, 10), 100)
    let _ = dict.insert(d, Key(5, 50), 500)
    let hit_probe = Probe(5)
    let miss_probe = Probe(9)
    let hit = dict.contains_by(d, &hit_probe)
    let miss = dict.contains_by(d, &miss_probe)
    var strings: Dict(String, Int) = dict.new()
    let _ = dict.insert(strings, "abc", 1)
    let needle = "abc"
    let text = string.as_str(&needle)
    let string_hit = dict.contains_by(strings, text)
    console.print("${hit}:${miss}:${string_hit}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("Equivalent queries check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("Equivalent queries admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["true:false:true"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn retained_string_view_hash_and_equality_match_owned_strings() {
    let source = r#"
import cmp
import string

fn main(console: Console):
    let empty = ""
    let first = "é🙂"
    let same = "é🙂"
    let different = "é"
    let empty_view = string.as_str(&empty)
    let first_view = string.as_str(&first)
    let same_view = string.as_str(&same)
    let different_view = string.as_str(&different)
    let hashes = string.hash_view(empty_view) == empty.hash() && string.hash_view(first_view) == first.hash()
    let equals = string.equal_views(first_view, same_view) && !string.equal_views(first_view, different_view)
    console.print("${hashes}:${equals}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("view operations check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("view operations admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted instance");
    vm.run().expect("view operations execute");
    assert_eq!(vm.output(), vec!["true:true"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
