//! Run explicitly with `cargo test --test dict_runtime_bench -- --ignored --nocapture`.
//! Compilation and VM construction are outside each timed run.

use std::time::Instant;

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

struct Case {
    name: &'static str,
    source: &'static str,
    expected: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "integer_hits_100k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.with_capacity(1024)
    var i = 0
    while i < 100000:
        let key = i % 1000
        let _ = dict.insert(d, key, dict.get_or(d, key, 0) + 1)
        i = i + 1
    console.print("${d.length()}:${d.get_or(0, 0)}")
"#,
        expected: "1000:100",
    },
    Case {
        name: "integer_growth_5k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    var i = 0
    while i < 5000:
        let _ = dict.insert(d, i, i + 1)
        i = i + 1
    console.print("${d.length()}:${d.get_or(4999, 0)}")
"#,
        expected: "5000:5000",
    },
    Case {
        name: "integer_preallocated_5k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.with_capacity(10000)
    var i = 0
    while i < 5000:
        let _ = dict.insert(d, i, i + 1)
        i = i + 1
    console.print("${d.length()}:${d.get_or(4999, 0)}")
"#,
        expected: "5000:5000",
    },
    Case {
        name: "integer_growth_20k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    var i = 0
    while i < 20000:
        let _ = dict.insert(d, i, i + 1)
        i = i + 1
    console.print("${d.length()}:${d.get_or(19999, 0)}")
"#,
        expected: "20000:20000",
    },
    Case {
        name: "dense_integer_hits_6k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.with_capacity(8192)
    for i in 0..6000:
        let _ = dict.insert(d, i, i)
    var total = 0
    var i = 0
    while i < 100000:
        total = total + dict.get_or(d, i % 6000, 0)
        i = i + 1
    console.print("${total}")
"#,
        expected: "295950000",
    },
    Case {
        name: "sparse_integer_hits_6k",
        source: r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.with_capacity(16384)
    for i in 0..6000:
        let _ = dict.insert(d, i, i)
    var total = 0
    var i = 0
    while i < 100000:
        total = total + dict.get_or(d, i % 6000, 0)
        i = i + 1
    console.print("${total}")
"#,
        expected: "295950000",
    },
    Case {
        name: "string_updates_10k",
        source: r#"
fn main(console: Console):
    var d: Dict(String, Int) = dict.new()
    var i = 0
    while i < 10000:
        let key = "key-${i % 100}"
        let _ = dict.insert(d, key, dict.get_or(d, key, 0) + 1)
        i = i + 1
    console.print("${d.length()}:${d.get_or("key-0", 0)}")
"#,
        expected: "100:100",
    },
    Case {
        name: "string_build_10k",
        source: r#"
fn main(console: Console):
    var total = 0
    var i = 0
    while i < 10000:
        let key = "key-${i % 100}"
        total = total + key.length()
        i = i + 1
    console.print("${total}")
"#,
        expected: "59000",
    },
    Case {
        name: "string_lookups_10k",
        source: r#"
fn main(console: Console):
    var d: Dict(String, Int) = dict.new()
    for i in 0..100:
        let _ = dict.insert(d, "key-${i}", i)
    var total = 0
    var i = 0
    while i < 10000:
        let key = "key-${i % 100}"
        total = total + dict.get_or(d, key, 0)
        i = i + 1
    console.print("${total}")
"#,
        expected: "495000",
    },
    Case {
        name: "string_inserts_10k",
        source: r#"
fn main(console: Console):
    var d: Dict(String, Int) = dict.new()
    var i = 0
    while i < 10000:
        let key = "key-${i % 100}"
        let _ = dict.insert(d, key, i)
        i = i + 1
    console.print("${d.length()}:${d.get_or("key-0", 0)}")
"#,
        expected: "100:9900",
    },
    Case {
        name: "string_update_one_search_10k",
        source: r#"
fn inc(x: Int) -> Int:
    x + 1

fn main(console: Console):
    var d: Dict(String, Int) = dict.new()
    var i = 0
    while i < 10000:
        let key = "key-${i % 100}"
        dict.update(d, key, 0, inc)
        i = i + 1
    console.print("${d.length()}:${d.get_or("key-0", 0)}")
"#,
        expected: "100:100",
    },
];

#[test]
fn admitted_dictionary_runtime_excludes_compilation() {
    if std::env::var_os("WITCHY_DICT_BENCH").is_none() {
        return;
    }
    for case in CASES {
        let compile_start = Instant::now();
        let checked = witchy::resolve_std_only_checked(case.source).expect("type-check workload");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit workload");
        let compile_ms = compile_start.elapsed().as_secs_f64() * 1000.0;
        let mut runtime = Runtime::batch().expect("runtime");
        let mut samples = Vec::new();
        for iteration in 0..23 {
            let mut vm = runtime.spawn_admitted(
                &artifact,
                Capabilities { print: true, quiet: true, ..Capabilities::none() },
                128,
            ).expect("admitted instance");
            let start = Instant::now();
            vm.run().expect("admitted execution");
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(vm.output(), vec![case.expected.to_string()], "{}", case.name);
            if iteration >= 3 {
                samples.push(elapsed_ms);
            }
        }
        samples.sort_by(f64::total_cmp);
        let median = (samples[9] + samples[10]) / 2.0;
        eprintln!(
            "{} compile_ms={:.3} run_median_ms={:.3} run_min_ms={:.3} run_max_ms={:.3}",
            case.name, compile_ms, median, samples[0], samples[19],
        );
    }
}
