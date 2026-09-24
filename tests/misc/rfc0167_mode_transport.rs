//! RFC-0167 row J: one normal-to-opt call transports several rooted views.
//!
//! The expected value is calculated from source values, then checked against
//! canonical SafeWIR and admitted unoptimized Wasm with heap audits on both.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

const OPT: &str = r#"mode opt

type Carrier:
    text: &String
    whole: &List(Int)
    window: &[Int]

fn identity[a](value: a) -> a:
    value

pub fn read(carrier: Carrier) -> Int:
    let held = identity(carrier)
    let (text, window) = identity((held.text, held.window))
    (*text).length() * 1000 + (*held.whole).length() * 100 + window[0] * 10 + window[1]
"#;

const NORMAL: &str = r#"import fast

fn main() -> Int:
    var text = "ab" + "cd"
    var xs = [10, 20, 30, 40]
    let packet = fast.Carrier(text: &text, whole: &xs, window: &xs[1..3])
    text = text + "ef"
    xs.push(50)
    fast.read(packet) * 10 + text.length() + xs.length()
"#;

#[test]
fn normal_to_opt_transports_string_list_slice_and_generic_tuple_views() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(opt::active_schema_key().enabled_mask(), 0);

    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("fast".into(), OPT.as_bytes().to_vec()),
            ("main".into(), NORMAL.as_bytes().to_vec()),
        ],
        "main",
        witchy_interp::comptime::expand_compile_time,
    )
    .expect("normal caller and opt carrier API type-check");
    assert!(!witchy_lower::analysis::function_is_opt(
        checked.module(),
        "main"
    ));
    assert!(witchy_lower::analysis::function_is_opt(
        checked.module(),
        "fast.read"
    ));

    // The packet retains text "abcd", list [10,20,30,40], and slice [20,30]
    // after the owners become "abcdef" and [10,20,30,40,50]. Thus opt read
    // returns 4*1000 + 4*100 + 20*10 + 30 = 4630; normal returns
    // 4630*10 + 6 + 5 = 46311.
    let expected = 46_311;
    let (artifact, oracle) =
        codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry
                    .export()
                    .strip_prefix('f')
                    .expect("numeric entry export")
                    .parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry
                .capabilities()
                .iter()
                .copied()
                .map(InterpreterValue::entry_capability)
                .collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical SafeWIR execution")
        })
        .expect_lowered("unoptimized canonical SafeWIR admits the cross-mode carrier");
    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)]);
    assert_eq!(
        oracle.leaked_objects, 0,
        "interpreter releases retained roots"
    );

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0)
    );
}

#[test]
fn deeper_nested_list_payload_cannot_claim_the_multi_carrier_abi() {
    let checked = witchy::resolve_std_only_checked(
        r#"
mode opt

type Carrier:
    nested: &List(List(List(Int)))

fn read(carrier: Carrier) -> Int:
    (*carrier.nested).length()

fn main() -> Int:
    let xs = [[[1, 2]]]
    read(Carrier(nested: &xs))
"#,
    )
    .expect("the unadmitted payload can be expressed in checked source");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("deeper nested list view has no authenticated carrier");
    let message = refusal.to_string();
    assert!(
        message.contains("rooted view payload is outside the canonical Int carrier")
            || message.contains("rooted view payload has no admitted authenticated carrier"),
        "the canonical boundary must fail closed: {refusal}",
    );
}
