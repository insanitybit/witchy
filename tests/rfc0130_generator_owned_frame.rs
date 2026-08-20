use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CarrierDimensions {
    states: u32,
    entry_states: u32,
    segment_states: u32,
    max_lane_width: u32,
    max_slots: u32,
    wholly_direct: bool,
}

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

fn read_u32(bytes: &[u8], cursor: &mut usize) -> u32 {
    let end = *cursor + 4;
    let value = u32::from_le_bytes(
        bytes[*cursor..end]
            .try_into()
            .expect("carrier u32 remains in bounds"),
    );
    *cursor = end;
    value
}

fn carrier_dimensions(wasm: &[u8]) -> CarrierDimensions {
    let sections = wasmparser::Parser::new(0)
        .parse_all(wasm)
        .filter_map(|payload| match payload.expect("valid generator Wasm") {
            wasmparser::Payload::CustomSection(section)
                if section.name() == "witchy.suspension-carrier" =>
            {
                Some(section.data().to_vec())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sections.len(),
        1,
        "compiled generators have one carrier catalog",
    );

    let bytes = &sections[0];
    assert_eq!(bytes[0], 2, "current-master suspension-carrier ABI version");
    let mut cursor = 1;
    let states = read_u32(bytes, &mut cursor);
    let max_lane_width = read_u32(bytes, &mut cursor);
    let mut entry_states = 0;
    let mut segment_states = 0;
    let mut max_slots = 0;
    let mut wholly_direct = true;
    for _ in 0..states {
        let _state_id = read_u32(bytes, &mut cursor);
        match bytes[cursor] {
            0 => entry_states += 1,
            1 => segment_states += 1,
            kind => panic!("unknown carrier state kind {kind}"),
        }
        cursor += 1;
        wholly_direct &= bytes[cursor] == 1;
        cursor += 1;
        let slots = read_u32(bytes, &mut cursor);
        max_slots = max_slots.max(slots);
        for _ in 0..slots {
            cursor += 1; // parameter convention
            let direct = bytes[cursor] == 1;
            cursor += 1;
            if direct {
                let lanes = read_u32(bytes, &mut cursor) as usize;
                cursor += lanes;
            }
        }
    }
    assert_eq!(cursor, bytes.len(), "carrier catalog is decoded completely");
    CarrierDimensions {
        states,
        entry_states,
        segment_states,
        max_lane_width,
        max_slots,
        wholly_direct,
    }
}

fn compile(pulls: usize) -> (Vec<u8>, CarrierDimensions) {
    let checked = witchy::resolve_std_only_checked(&source(pulls))
        .expect("check RFC-0130 owned-frame fixture");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0130 owned-frame fixture");
    let dimensions = carrier_dimensions(&wasm);
    (wasm, dimensions)
}

fn run(wasm: &[u8]) -> CompiledRun {
    witchy_interp::compiler_natives::install();
    let mut runtime = Runtime::batch_quick().expect("create generator acceptance runtime");
    let mut actor = runtime
        .spawn(
            wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn compiled generator acceptance fixture");
    actor.run().expect("run compiled generator acceptance fixture");
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

#[test]
fn rfc0130_rows_1_to_3_generators_resume_owned_frames_once_with_linear_work_on_wasm() {
    const SMALL_PULLS: usize = 8;
    const LARGE_PULLS: usize = 64;

    let (small_wasm, small_carrier) = compile(SMALL_PULLS);
    let (large_wasm, large_carrier) = compile(LARGE_PULLS);
    assert_eq!(
        large_carrier, small_carrier,
        "frame dimensions stay fixed as pulls grow",
    );
    assert_eq!(
        small_carrier.states, 3,
        "one top-level entry and two generator resume segments are catalogued",
    );
    assert_eq!(
        (small_carrier.entry_states, small_carrier.segment_states),
        (1, 2),
        "the top-level and inherent generators each own a compiled resume segment",
    );
    assert_eq!(
        small_carrier.max_slots, 2,
        "no state needs more than the two source inputs",
    );
    assert_eq!(
        small_carrier.max_lane_width, 4,
        "the phase-aware owned resume frame has four fixed direct lanes",
    );
    assert!(
        small_carrier.wholly_direct,
        "the owned generator frames stay in fixed direct Wasm lanes",
    );

    let small = (0..3).map(|_| run(&small_wasm)).collect::<Vec<_>>();
    let large = (0..3).map(|_| run(&large_wasm)).collect::<Vec<_>>();
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
