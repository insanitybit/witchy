//! RFC-0179 T6: the vendored aarch64 backend lowers `y + (uextend(x) << k)`,
//! k in 1-4, to one `add xd, xn, wm, uxtw #k` (the encoding's imm3 field).
//! Every shift computes the same value as Rust, and on aarch64 a foldable
//! shift compiles smaller than one the form cannot take.

use wasmtime::{Config, Engine, Instance, Module, Store};

fn module(shift: u32) -> String {
    format!(
        "(module (func (export \"f\") (param i32 i64) (result i64)
           local.get 1 local.get 0 i64.extend_i32_u i64.const {shift} i64.shl i64.add))"
    )
}

fn engine() -> Engine {
    Engine::new(&Config::new()).unwrap()
}

#[test]
fn shifted_zero_extended_add_matches_rust() {
    let engine = engine();
    for shift in 1..=5u32 {
        let module = Module::new(&engine, module(shift)).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let f = instance.get_typed_func::<(i32, i64), i64>(&mut store, "f").unwrap();
        for (x, y) in [(0u32, 0i64), (1, 7), (u32::MAX, 1), (0x8000_0000, -1), (0x1234_5678, i64::MAX), (u32::MAX, i64::MIN)] {
            let expected = y.wrapping_add(((x as u64) << shift) as i64);
            assert_eq!(f.call(&mut store, (x as i32, y)).unwrap(), expected, "shift {shift}, x {x:#x}, y {y}");
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn foldable_shift_is_one_instruction() {
    let engine = engine();
    let size = |shift| Module::new(&engine, module(shift)).unwrap().functions().map(|f| f.len).sum::<usize>();
    // uxtw #3 folds into the add; a shift of 5 needs its own instruction.
    assert!(size(3) < size(5), "shift 3: {} bytes, shift 5: {} bytes", size(3), size(5));
}
