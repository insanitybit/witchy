use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

#[derive(Debug, PartialEq, Eq)]
struct CarrierDimensions {
    states: u32,
    max_lane_width: u32,
    max_slots: u32,
    wholly_direct: bool,
}

fn source(pulls: usize) -> String {
    format!(
        r#"import iter

gen fn counted(console: Console, limit: Int) -> Iter(Int):
    var i: Int = 0
    while i < limit:
        console.print("tick")
        yield i
        i = i + 1

fn main(console: Console):
    var total: Int = 0
    let values: List(Int) = iter.collect(counted(console, {pulls}))
    for value in values:
        total = total + value
    console.print("sum ${{total}}")
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
    assert_eq!(sections.len(), 1, "compiled generator has one carrier catalog");

    let bytes = &sections[0];
    assert_eq!(bytes[0], 2, "carrier ABI version");
    let mut cursor = 1;
    let states = read_u32(bytes, &mut cursor);
    let max_lane_width = read_u32(bytes, &mut cursor);
    let mut max_slots = 0;
    let mut wholly_direct = true;
    for _ in 0..states {
        let _state_id = read_u32(bytes, &mut cursor);
        cursor += 1; // state kind
        wholly_direct &= bytes[cursor] == 1;
        cursor += 1;
        let slots = read_u32(bytes, &mut cursor);
        max_slots = max_slots.max(slots);
        for _ in 0..slots {
            cursor += 1; // parameter convention
            let direct = bytes[cursor] == 1;
            wholly_direct &= direct;
            cursor += 1;
            if direct {
                let lanes = read_u32(bytes, &mut cursor) as usize;
                cursor += lanes;
            }
        }
    }
    assert_eq!(cursor, bytes.len(), "carrier catalog is decoded completely");
    CarrierDimensions { states, max_lane_width, max_slots, wholly_direct }
}

fn compiled_sample(pulls: usize) -> (Vec<String>, CarrierDimensions) {
    let checked = witchy::resolve_std_only_checked(&source(pulls))
        .expect("check RFC-0130 row-3 generator fixture");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0130 row-3 generator fixture");
    let dimensions = carrier_dimensions(&wasm);
    witchy_interp::compiler_natives::install();
    let mut runtime = Runtime::batch_quick().expect("create generator scaling runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn compiled generator scaling fixture");
    actor.run().expect("run compiled generator scaling fixture");
    (actor.output(), dimensions)
}

#[test]
fn rfc0130_acceptance_row_3_owned_frame_pulls_are_linear_and_storage_is_bounded_on_wasm() {
    let (small_output, small_carrier) = compiled_sample(8);
    let (large_output, large_carrier) = compiled_sample(64);

    assert_eq!(small_output.len(), 9, "eight pulls execute eight body segments");
    assert!(small_output[..8].iter().all(|line| line == "tick"));
    assert_eq!(small_output[8], "sum 28");
    assert_eq!(large_output.len(), 65, "64 pulls execute 64 body segments");
    assert!(large_output[..64].iter().all(|line| line == "tick"));
    assert_eq!(large_output[64], "sum 2016");
    assert_eq!(
        large_output.len() - 1,
        (small_output.len() - 1) * 8,
        "body executions scale exactly with pulls, not with replay-to-kth work",
    );

    assert!(small_carrier.wholly_direct, "owned generator frame stays in direct Wasm lanes");
    assert_eq!(small_carrier.states, 2, "one entry and one resume state");
    assert_eq!(small_carrier.max_slots, 2, "the entry/resume ABI needs at most two slots");
    assert_eq!(
        small_carrier.max_lane_width, 5,
        "the lazy owned frame needs five direct lanes",
    );
    assert_eq!(
        large_carrier, small_carrier,
        "live state, frame slots, and lane width stay bounded as pulls grow",
    );
}
