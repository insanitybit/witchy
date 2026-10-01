//! Measures the host-call cost of a hashbrown-backed Int -> Int dictionary.
//! Run with `cargo test --test hashbrown_dict_probe -- --nocapture`.

use hashbrown::HashMap;
use std::time::Instant;
use wasmtime::{Caller, Engine, Linker, Module, Store};

#[test]
fn host_hashbrown_dict_kernel() -> wasmtime::Result<()> {
    const ITERATIONS: i32 = 3_000_000;
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        r#"(module
          (import "dict" "get" (func $get (param i32) (result i64)))
          (import "dict" "put" (func $put (param i32 i64)))
          (func (export "run") (result i64)
            (local $i i32) (local $k i32) (local $total i64)
            (block $done
              (loop $next
                local.get $i
                i32.const 3000000
                i32.ge_s
                br_if $done
                local.get $i
                i32.const 1000
                i32.rem_s
                local.set $k
                local.get $k
                local.get $k
                call $get
                i64.const 1
                i64.add
                call $put
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $next))
            (local.set $i (i32.const 0))
            (block $sum_done
              (loop $sum_next
                local.get $i
                i32.const 1000
                i32.ge_s
                br_if $sum_done
                local.get $total
                local.get $i
                call $get
                i64.add
                local.set $total
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $sum_next))
            local.get $total))"#,
    )?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("dict", "get", |caller: Caller<'_, HashMap<i32, i64>>, key: i32| {
        *caller.data().get(&key).unwrap_or(&0)
    })?;
    linker.func_wrap(
        "dict",
        "put",
        |mut caller: Caller<'_, HashMap<i32, i64>>, key: i32, value: i64| {
            caller.data_mut().insert(key, value);
        },
    )?;
    let mut store = Store::new(&engine, HashMap::with_capacity(1000));
    let instance = linker.instantiate(&mut store, &module)?;
    let run = instance.get_typed_func::<(), i64>(&mut store, "run")?;
    let start = Instant::now();
    let total = run.call(&mut store, ())?;
    let elapsed = start.elapsed();
    assert_eq!(total, i64::from(ITERATIONS));
    assert_eq!(store.data().len(), 1000);
    eprintln!("host hashbrown kernel: {elapsed:?} for {ITERATIONS} updates");
    Ok(())
}
