use super::CompilerServices;
use crate::runtime::{Capabilities, Runtime, Vm};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

struct Docs(Arc<AtomicUsize>);

impl CompilerServices for Docs {
    fn footprint(&self, _: &str) -> wasmtime::Result<String> { unreachable!() }
    fn footprint_diff(&self, _: &str, _: &str) -> wasmtime::Result<String> { unreachable!() }
    fn doc(&self, name: &str, source: &str) -> wasmtime::Result<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(format!("markdown:{name}:{source}"))
    }
    fn doc_result_json(&self, name: &str, source: &str) -> wasmtime::Result<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(format!("json:{name}:{source}"))
    }
}

fn setup() -> (Vm, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::new().unwrap();
    runtime.compiler_services = Arc::new(Docs(calls.clone()));
    let mut vm = runtime.spawn(r#"(module
      (import "witchy" "compiler_doc_into" (func $doc (param i32 i32 i32 i32 i32 i32) (result i32)))
      (import "witchy" "compiler_doc_result_json_into" (func $json (param i32 i32 i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "run"))
      (func (export "doc") (param i32 i32 i32 i32 i32 i32) (result i32)
        local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 call $doc)
      (func (export "json") (param i32 i32 i32 i32 i32 i32) (result i32)
        local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 call $json))"#,
        Capabilities::none(), 1).unwrap();
    vm.store.data_mut().pending = Some(b"unrelated pending data".to_vec());
    (vm, calls)
}

fn invoke(vm: &mut Vm, export: &str, [a, b, c, d, e, f]: [i32; 6]) -> wasmtime::Result<i32> {
    let function = vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i32>(&mut vm.store, export).unwrap();
    let result = function.call(&mut vm.store, (a, b, c, d, e, f));
    assert_eq!(vm.store.data().pending.as_deref(), Some(b"unrelated pending data".as_slice()));
    result
}

pub fn bounded_compiler_doc_preflights_every_range_before_provider() {
    for export in ["doc", "json"] {
        let (mut vm, calls) = setup();
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
        memory.data_mut(&mut vm.store).fill(b'x');
        let original = memory.data(&vm.store).to_vec();
        for pair in [0, 2, 4] {
            for (pointer, length) in [(-1, 1), (0, -1), (i32::MAX, i32::MAX), (65535, 2), (65537, 0)] {
                let mut args = [1, 3, 129, 4, 1024, 32];
                args[pair] = pointer;
                args[pair + 1] = length;
                assert!(invoke(&mut vm, export, args).is_err());
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(memory.data(&vm.store), original.as_slice());
            }
        }
    }
}

pub fn bounded_compiler_doc_rejects_invalid_utf8_without_provider_or_writes() {
    for export in ["doc", "json"] {
        let (mut vm, calls) = setup();
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
        for position in [1, 129] {
            for invalid in [b"\xff".as_slice(), b"\xc2".as_slice(), b"\xc0\x80".as_slice()] {
                memory.data_mut(&mut vm.store).fill(b'x');
                memory.write(&mut vm.store, position, invalid).unwrap();
                let mut args = [1, 3, 129, 4, 1024, 32];
                args[if position == 1 { 1 } else { 3 }] = invalid.len() as i32;
                let before = memory.data(&vm.store).to_vec();
                assert!(invoke(&mut vm, export, args).is_err());
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(memory.data(&vm.store), before.as_slice());
            }
        }
    }
}
