use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledRun {
    output: Vec<String>,
    heap_bytes: i64,
    live_cells: i64,
    rc_alloc_calls: i64,
    bump_alloc_calls: i64,
    reowns: i64,
}

fn source(pulls: usize) -> String {
    format!(
        r#"import iter

gen fn counted(console: Console, limit: Int) -> Iter(Int):
    var i: Int = 0
    while i < limit:
        if i % 2 == 0:
            console.print("top even")
        else:
            console.print("top odd")
        yield i
        i = i + 1

type Counter:
    limit: Int

impl Counter:
    gen fn counted(self, console: Console) -> Iter(Int):
        var i: Int = 0
        while i < self.limit:
            if i % 2 == 0:
                console.print("method even")
            else:
                console.print("method odd")
            yield i
            i = i + 1

fn main(console: Console):
    let top_total = counted(console, {pulls}).sum()
    console.print("top sum ${{top_total}}")
    let counter = Counter({pulls})
    let method_total = counter.counted(console).sum()
    console.print("method sum ${{method_total}}")
"#,
    )
}

fn compile(pulls: usize) -> witchy_wir::AdmittedCanonicalArtifact {
    let checked = witchy::resolve_std_only_checked(&source(pulls))
        .expect("check RFC-0130 owned-frame fixture");
    codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compile RFC-0130 owned-frame fixture")
}

fn run(artifact: &witchy_wir::AdmittedCanonicalArtifact) -> CompiledRun {
    let mut runtime = Runtime::batch_quick().expect("create generator acceptance runtime");
    let mut actor = runtime
        .spawn_admitted(
            artifact,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn compiled generator acceptance fixture");
    actor
        .run()
        .expect("run compiled generator acceptance fixture");
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
    CompiledRun {
        output: actor.output(),
        heap_bytes: actor.heap_bytes().unwrap_or(0),
        live_cells: actor.live_cells().unwrap_or(0),
        rc_alloc_calls: actor.rc_alloc_calls().unwrap_or(0),
        bump_alloc_calls: actor.bump_alloc_calls().unwrap_or(0),
        reowns: actor.reowns().unwrap_or(0),
    }
}

fn expected_output(pulls: usize) -> Vec<String> {
    let mut output = Vec::with_capacity(pulls * 2 + 2);
    for prefix in ["top", "method"] {
        for i in 0..pulls {
            let parity = if i % 2 == 0 { "even" } else { "odd" };
            output.push(format!("{prefix} {parity}"));
        }
        output.push(format!("{prefix} sum {}", pulls * (pulls - 1) / 2));
    }
    output
}

fn is_nightly() -> bool {
    std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_FULL_SUITE").is_some_and(|v| v == "1")
}

#[test]
fn rfc0130_rows_1_to_3_generators_resume_owned_frames_once_with_linear_work_on_wasm() {
    if !is_nightly() {
        return;
    }
    const SMALL_PULLS: usize = 8;
    const LARGE_PULLS: usize = 64;

    let small_artifact = compile(SMALL_PULLS);
    let large_artifact = compile(LARGE_PULLS);

    let small = (0..3).map(|_| run(&small_artifact)).collect::<Vec<_>>();
    let large = (0..3).map(|_| run(&large_artifact)).collect::<Vec<_>>();
    assert!(
        small.windows(2).all(|pair| pair[0] == pair[1]),
        "small counters are deterministic",
    );
    assert!(
        large.windows(2).all(|pair| pair[0] == pair[1]),
        "large counters are deterministic",
    );
    let small = &small[0];
    let large = &large[0];

    assert_eq!(small.output, expected_output(SMALL_PULLS));
    assert_eq!(large.output, expected_output(LARGE_PULLS));
    assert_eq!(
        large.output.len() - 2,
        (small.output.len() - 2) * 8,
        "pre-yield effects execute once per pull rather than replaying a growing prefix",
    );

    assert_eq!(
        large.live_cells, small.live_cells,
        "completed traversals retain bounded live state",
    );
    assert!(
        large.heap_bytes <= small.heap_bytes * 8,
        "eight times the pulls must use at most eight times the linear-memory frontier: small={}, large={}",
        small.heap_bytes,
        large.heap_bytes,
    );
    assert_eq!(
        large.rc_alloc_calls, small.rc_alloc_calls,
        "pull growth must not add RC allocation work",
    );
    assert_eq!(
        large.bump_alloc_calls, small.bump_alloc_calls,
        "pull growth must not add bump allocation work",
    );
    assert_eq!(
        large.reowns, small.reowns,
        "pull growth must not add ownership repair work",
    );
}
