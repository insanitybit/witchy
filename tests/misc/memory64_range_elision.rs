//! RFC-0179 R2: the vendored Wasmtime bounds checks drop the explicit check on
//! a 64-bit memory access whose index value is bounded inside the reserved
//! range, and every access outside the memory still traps.

use wasmtime::{Config, Engine, Instance, Module, Store};

const RESERVATION: u64 = 8 << 30;

fn engine() -> Engine {
    engine_with(RESERVATION, 32 << 20)
}

fn engine_with(reservation: u64, guard: u64) -> Engine {
    let mut config = Config::new();
    config.wasm_memory64(true);
    config.memory_reservation(reservation);
    config.memory_guard_size(guard);
    config.memory_reservation_for_growth(0);
    config.memory_may_move(false);
    Engine::new(&config).unwrap()
}

/// One page of memory; `index` turns the i64 parameter into the address, and
/// each of the `loads` reads a distinct offset so none is merged away.
fn module(index: &str, loads: usize) -> String {
    let body: String = (0..loads)
        .map(|k| format!("local.get 0 {index} i32.load offset={}\n", 4 * k))
        .collect();
    let drops = "drop\n".repeat(loads.saturating_sub(1));
    format!(
        "(module (memory i64 1)
           (func (export \"load\") (param i64) (result i32) {body} {drops}))"
    )
}

const ZERO_EXTENDED: &str = "i32.wrap_i64 i64.extend_i32_u";
const SHIFTED_PAST_RESERVATION: &str = "i32.wrap_i64 i64.extend_i32_u i64.const 3 i64.shl";

fn load(engine: &Engine, wat: &str, address: u64) -> Result<i32, wasmtime::Error> {
    let module = Module::new(engine, wat)?;
    let mut store = Store::new(engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;
    let load = instance.get_typed_func::<i64, i32>(&mut store, "load")?;
    load.call(&mut store, address as i64)
}

#[test]
fn bounded_index_needs_no_explicit_check() {
    let engine = engine();
    let code = |index| {
        let module = Module::new(&engine, module(index, 64)).unwrap();
        module.functions().map(|function| function.len).sum::<usize>()
    };
    let (bounded, unbounded) = (code(ZERO_EXTENDED), code(""));
    // 64 accesses: each unbounded one pays a compare and a Spectre select.
    assert!(
        bounded + 256 < unbounded,
        "bounded {bounded} code bytes, unbounded {unbounded}"
    );
}

fn code_bytes(engine: &Engine, wat: &str) -> usize {
    Module::new(engine, wat).unwrap().functions().map(|function| function.len).sum()
}

#[test]
fn elision_stops_at_reservation_plus_guard() {
    // A zero-extended index reaches 2^32 - 1, so even the offset-0 load ends
    // past 2^32. Reservation + guard of 4 GiB + 64 KiB covers every load;
    // exactly 4 GiB covers none, and the checks come back.
    let wat = module(ZERO_EXTENDED, 64);
    let covered = code_bytes(&engine_with(4 << 30, 64 << 10), &wat);
    let short = code_bytes(&engine_with((4 << 30) - (64 << 10), 64 << 10), &wat);
    assert!(covered + 256 < short, "covered {covered} code bytes, one page short {short}");
}

#[test]
fn bounded_index_still_traps_outside_the_memory() {
    let engine = engine();
    let wat = module(ZERO_EXTENDED, 1);
    assert_eq!(load(&engine, &wat, 16).unwrap(), 0);
    // Past the one accessible page but inside the reservation: the guard
    // region faults.
    assert!(load(&engine, &wat, 1 << 20).is_err());
    assert!(load(&engine, &wat, u32::MAX as u64).is_err());
}

#[test]
fn unbounded_and_overlarge_indices_keep_their_check() {
    let engine = engine();
    assert!(load(&engine, &module("", 1), 1 << 40).is_err());
    assert!(load(&engine, &module("", 1), u64::MAX).is_err());
    // A bound past the reservation (32 GiB > 8 GiB) is not elided.
    assert!(load(&engine, &module(SHIFTED_PAST_RESERVATION, 1), u32::MAX as u64).is_err());
}

/// R4's address shape, `(uextend(ref) << 3) + uextend(offset)` plus a static
/// offset, on the large-heap engine's configuration (48 GiB reservation, no
/// guard region as for any 64-bit memory, memory never moves, Spectre
/// mitigation at its default). CVE-2026-34971 miscompiled exactly this shape
/// on aarch64, so every load is checked against a Rust model of the memory:
/// in bounds it reads the model's bytes, otherwise it traps.
const LARGE_RESERVATION: u64 = 48 << 30;
const SCALED_PAGES: u64 = 2;

fn scaled_module() -> String {
    let address = "local.get 0 i64.extend_i32_u i64.const 3 i64.shl local.get 1 i64.extend_i32_u i64.add";
    format!(
        "(module (memory (export \"memory\") i64 {SCALED_PAGES})
           (func (export \"byte\") (param i32 i32) (result i64) {address} i64.load8_u)
           (func (export \"word\") (param i32 i32) (result i64) {address} i64.load32_u offset=4)
           (func (export \"long\") (param i32 i32) (result i64) {address} i64.load offset=8)
           (func (export \"field\") (param i32 i32) (result i64) {address} i64.const 12 i64.add i64.load32_u offset=4))"
    )
}

#[test]
fn scaled_reference_loads_need_no_explicit_check() {
    // (ref << 3) + uextend(offset) reaches 2^35 + 2^32, so a field past it
    // ends beyond 36 GiB: only the 48 GiB reservation covers every access
    // without a guard region.
    let wat = scaled_module();
    let covered = code_bytes(&engine_with(LARGE_RESERVATION, 0), &wat);
    let short = code_bytes(&engine_with(36 << 30, 0), &wat);
    assert!(covered + 24 < short, "covered {covered} code bytes, 36 GiB reservation {short}");
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(256)))]
    #[test]
    fn scaled_reference_loads_match_the_memory_model(
        loads in proptest::collection::vec((
            proptest::prop_oneof![0u32..16_400, proptest::prelude::any::<u32>(), (u32::MAX - 16)..=u32::MAX],
            proptest::prop_oneof![0u32..64, 131_000u32..131_100, proptest::prelude::any::<u32>()],
            0usize..4,
        ), 1..64)
    ) {
        use std::sync::OnceLock;
        static ENGINE: OnceLock<(Engine, Module)> = OnceLock::new();
        let (engine, module) = ENGINE.get_or_init(|| {
            let engine = engine_with(LARGE_RESERVATION, 0);
            let module = Module::new(&engine, scaled_module()).unwrap();
            (engine, module)
        });
        let mut store = Store::new(engine, ());
        let instance = Instance::new(&mut store, module, &[]).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let model: Vec<u8> = (0..SCALED_PAGES * 65536).map(|at| (at * 131 % 251) as u8).collect();
        memory.write(&mut store, 0, &model).unwrap();
        for (reference, offset, kind) in loads {
            // `field` adds a constant the engine moves into the static offset.
            let (export, at, size) = [("byte", 0, 1), ("word", 4, 4), ("long", 8, 8), ("field", 16, 4)][kind];
            let load = instance.get_typed_func::<(i32, i32), i64>(&mut store, export).unwrap();
            let address = (u64::from(reference) << 3) + u64::from(offset) + at;
            let got = load.call(&mut store, (reference as i32, offset as i32));
            if address + size <= model.len() as u64 {
                let mut bytes = [0u8; 8];
                bytes[..size as usize].copy_from_slice(&model[address as usize..(address + size) as usize]);
                proptest::prop_assert_eq!(got.unwrap(), i64::from_le_bytes(bytes), "{} at {:#x}", export, address);
            } else {
                proptest::prop_assert!(got.is_err(), "{} at {:#x} read outside the memory", export, address);
            }
        }
    }
}
