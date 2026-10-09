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

/// Loads whose address is `base + (uextend(y) << k)` with k unlike the access
/// size (the address-mode rule), plus a second zero-extended addend (the
/// scaled-first address form), at every static-offset shape: zero, unaligned
/// (simm9), aligned (uimm12) and large (register). Each reads the model's
/// bytes in bounds and traps outside them.
#[test]
fn scaled_index_loads_match_the_memory_model() {
    const PAGES: u64 = 20;
    let mut config = Config::new();
    config.wasm_memory64(true);
    config.memory_reservation(48 << 30);
    config.memory_guard_size(0);
    config.memory_reservation_for_growth(0);
    config.memory_may_move(false);
    let engine = Engine::new(&config).unwrap();
    let offsets = [0u64, 3, 1000, 1 << 20];
    let mut funcs = String::new();
    for (shift, load) in [(1u32, "i64.load32_u"), (2, "i64.load"), (3, "i64.load16_u")] {
        for offset in offsets {
            funcs += &format!("(func (export \"one_{shift}_{offset}\") (param i32 i32) (result i64)
                local.get 0 i64.extend_i32_u i64.const {shift} i64.shl {load} offset={offset})\n");
            funcs += &format!("(func (export \"two_{shift}_{offset}\") (param i32 i32) (result i64)
                local.get 0 i64.extend_i32_u i64.const {shift} i64.shl local.get 1 i64.extend_i32_u i64.add {load} offset={offset})\n");
        }
    }
    let module = Module::new(&engine, format!("(module (memory (export \"memory\") i64 {PAGES}) {funcs})")).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let model: Vec<u8> = (0..PAGES * 65536).map(|at| (at * 167 % 253) as u8).collect();
    memory.write(&mut store, 0, &model).unwrap();
    for (shift, width) in [(1u32, 4u64), (2, 8), (3, 2)] {
        for offset in offsets {
            for (y, z) in [(0u32, 0u32), (1, 1), (37, 5), (100_000, 3), (163_000, 0), (0, 1_300_000), (u32::MAX, 0), (5, u32::MAX)] {
                for (name, second) in [("one", 0u64), ("two", u64::from(z))] {
                    let f = instance.get_typed_func::<(i32, i32), i64>(&mut store, &format!("{name}_{shift}_{offset}")).unwrap();
                    let address = (u64::from(y) << shift) + second + offset;
                    let got = f.call(&mut store, (y as i32, z as i32));
                    if address + width <= model.len() as u64 {
                        let mut bytes = [0u8; 8];
                        bytes[..width as usize].copy_from_slice(&model[address as usize..(address + width) as usize]);
                        assert_eq!(got.unwrap(), i64::from_le_bytes(bytes), "{name}_{shift}_{offset} y={y} z={z}");
                    } else {
                        assert!(got.is_err(), "{name}_{shift}_{offset} y={y} z={z} read outside the memory");
                    }
                }
            }
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
