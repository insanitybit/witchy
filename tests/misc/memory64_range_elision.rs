//! RFC-0179 R2: the vendored Wasmtime bounds checks drop the explicit check on
//! a 64-bit memory access whose index value is bounded inside the reserved
//! range, and every access outside the memory still traps.

use wasmtime::{Config, Engine, Instance, Module, Store};

const RESERVATION: u64 = 8 << 30;

fn engine() -> Engine {
    let mut config = Config::new();
    config.wasm_memory64(true);
    config.memory_reservation(RESERVATION);
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
