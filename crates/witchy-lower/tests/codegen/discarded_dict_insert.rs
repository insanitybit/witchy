use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{Operation, check_initial_with_witness};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }

fn checked(source: &str) -> CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        witchy_syntax::linker::ComptimeExpander::new(|_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
    ).expect("checked dictionary source")
}

#[test]
fn discarded_insert_lowers_at_the_caller_and_still_admits() {
    let _reset = Reset;
    for enabled in [false, true] {
        let options = witchy_syntax::opt::OptSet::all();
        witchy_syntax::opt::set_for_tests(Some(if enabled { options } else {
            options.without(witchy_syntax::opt::Opt::DirectCall)
        }));
        let input = checked("fn main() -> Int:\n    var values = dict.with_capacity(2)\n    let _ = dict.insert(values, 1, 7)\n    dict.get_or(values, 1, 0)\n");
        let source = input.canonical_source().unwrap();
        let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&input);
        let function_id = |name: &str| candidate.source_abi_bindings().entries().iter().find_map(|binding| {
            let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
            (declaration.name == name).then_some(binding.function())
        }).unwrap();
        let main = candidate.functions().iter().find(|function| function.id == function_id("main")).unwrap();
        let insert = function_id("dict.insert__Int__Int");
        let calls = main.blocks.iter().flat_map(|block| &block.operations)
            .filter(|operation| matches!(operation, Operation::Call { callee, .. } if *callee == insert)).count();
        assert_eq!(calls, usize::from(!enabled));
        check_initial_with_witness(candidate, &source.view()).expect("discarded dictionary insert retains source authority");
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn discarded_insert_preserves_effects_snapshots_and_used_results() {
    let _reset = Reset;
    use witchy_syntax::opt::{Opt, OptSet};
    let cases = [
        (r#"fn next(var count: Int) -> Int:
    count = count + 1
    count
fn main() -> Int:
    var values = dict.with_capacity(2)
    var effects = 0
    let _ = dict.insert(values, next(effects), next(effects))
    let first = dict.get_or(values, 1, 0)
    let _ = dict.insert(values, 1, next(effects))
    let prior = dict.insert(values, 1, 9)
    let old = match prior:
        Some(value) -> value
        None -> 0
    first * 100 + effects * 10 + old
"#, 233),
        (r#"type Box:
    Box(String)
fn main() -> Int:
    var values = dict.with_capacity(2)
    let _ = dict.insert(values, 1, Box("old"))
    let before = values
    let _ = dict.insert(values, 1, Box("new"))
    let old = dict.get_or(before, 1, Box("missing"))
    let current = dict.get_or(values, 1, Box("missing"))
    match (old, current):
        (Box(left), Box(right)) -> if left == "old" && right == "new": 1 else: 0
"#, 1),
        (r#"fn adjusted(var values: unique Dict(Int, Int), key: Int, value: Int) -> Option(Int):
    let replacement = value + 5
    dict.insert(values, key, replacement)
fn main() -> Int:
    var values = dict.with_capacity(2)
    let _ = adjusted(values, 1, 7)
    dict.get_or(values, 1, 0)
"#, 12),
    ];
    for options in [OptSet::none(), OptSet::all().without(Opt::DirectCall), OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        for (source, expected) in cases {
            assert_eq!(super::canonical_pipeline::run_callback_with_heap_audit(source, true)
                .expect("admitted dictionary execution with complete heap cleanup"), expected);
        }
    }
}
