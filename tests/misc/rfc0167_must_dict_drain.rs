//! Exact dictionary pop transfers both row slots and proves the empty remainder.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn dictionary_pop_transfers_both_row_slots() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
import dict
fn drain(var d: Dict(String, Int)) -> Int:
    match dict.__pop_extract(d):
        Some(pair) ->
            let (key, value) = pair
            value + drain(d)
        None -> 0
fn main() -> Int:
    var d: Dict(String, Int) = dict.new()
    let first = dict.insert(d, "first", 1)
    let second = dict.insert(d, "second", 2)
    drain(d)
"#).expect("ordinary dictionary pop checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("ordinary dictionary pop admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["3".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn must_dictionary_drains_through_exact_pop() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
import dict
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value

fn drain(own tickets: Dict(String, Ticket)) -> Int:
    match dict.__pop_extract(tickets):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish(ticket) + drain(tickets)
        None -> 0

fn main() -> Int:
    var tickets: Dict(String, Ticket) = dict.new()
    let first = match dict.insert_owned(tickets, "first", Ticket(1)):
        Some(pair) ->
            match move pair:
                (_, old) -> finish(old)
        None -> 0
    let second = match dict.insert_owned(tickets, "second", Ticket(2)):
        Some(pair) ->
            match move pair:
                (_, old) -> finish(old)
        None -> 0
    drain(tickets) + first + second
"#).expect("must dictionary drain checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("must dictionary drain admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["3".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn custom_key_and_must_value_transfer_together() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
import cmp
import dict
must type Key:
    id: Int
must type Ticket:
    Ticket(Int)
impl PartialEq for Key:
    fn eq(let self, let other: Self) -> Bool:
        self.id == other.id
impl Eq for Key
impl Hash for Key:
    fn hash(let self) -> Int:
        self.id
fn finish_key(own key: Key) -> Int:
    match move key:
        Key(id) -> id
fn finish_ticket(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value
fn drain(own entries: Dict(Key, Ticket)) -> Int:
    match dict.__pop_extract(entries):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish_key(key) * 10 + finish_ticket(ticket) + drain(entries)
        None -> 0
fn main() -> Int:
    var entries: Dict(Key, Ticket) = dict.new()
    let first = match dict.insert_owned(entries, Key(2), Ticket(5)):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish_key(key) + finish_ticket(ticket)
        None -> 0
    let displaced = match dict.insert_owned(entries, Key(2), Ticket(3)):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish_key(key) + finish_ticket(ticket)
        None -> 0
    drain(entries) + displaced + first
"#).expect("affine dictionary key and value check");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("affine dictionary key and value admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["30".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn exact_empty_dictionary_can_replace_borrowed_var_once() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
import dict
must type Ticket:
    Ticket(Int)
fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value
fn drain(own tickets: Dict(String, Ticket)) -> Int:
    match dict.__pop_extract(tickets):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish(ticket) + drain(tickets)
        None -> 0
fn replace(var tickets: Dict(String, Ticket)) -> Int:
    match dict.__pop_extract(tickets):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish(ticket) + replace(tickets)
        None ->
            var fresh: Dict(String, Ticket) = dict.new()
            let displaced = match dict.insert_owned(fresh, "new", Ticket(3)):
                Some(pair) ->
                    match move pair:
                        (_, old) -> finish(old)
                None -> 0
            tickets = move fresh
            displaced
fn main() -> Int:
    var tickets: Dict(String, Ticket) = dict.new()
    let first = match dict.insert_owned(tickets, "first", Ticket(1)):
        Some(pair) ->
            match move pair:
                (_, old) -> finish(old)
        None -> 0
    let second = match dict.insert_owned(tickets, "second", Ticket(2)):
        Some(pair) ->
            match move pair:
                (_, old) -> finish(old)
        None -> 0
    let old = replace(tickets)
    old * 10 + drain(tickets) + first + second
"#).expect("exact empty dictionary replacement checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("exact empty dictionary replacement admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["33".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn exact_empty_dictionary_proof_cannot_discard_a_later_obligation() {
    let source = r#"
import dict
must type Ticket:
    Ticket(Int)
fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value
fn replace(var entries: Dict(String, Ticket)) -> Int:
    match dict.__pop_extract(entries):
        Some(pair) ->
            match move pair:
                (key, ticket) -> finish(ticket) + replace(entries)
        None ->
            var fresh: Dict(String, Ticket) = dict.new()
            let displaced = match dict.insert_owned(fresh, "new", Ticket(3)):
                Some(pair) ->
                    match move pair:
                        (_, old) -> finish(old)
                None -> 0
            entries = move fresh
            entries = dict.new()
            displaced
fn main() -> Int:
    var entries: Dict(String, Ticket) = dict.new()
    replace(entries)
"#;
    let error = witchy::resolve_std_only_checked(source)
        .err().expect("second write-back would discard a must entry");
    assert!(error.to_string().contains("assignment would replace borrowed must-consume"), "{error}");
}
