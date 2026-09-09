//! Wasmtime adapters for compiler services exposed to trusted Witchy programs.
//!
//! The adapters read guest strings, call the [`CompilerServices`]
//! implementation injected through [`VmState`], and stage the result for
//! `fill_pending`. The kernel depends on this interface — not on the
//! compiler's implementation modules — so post-compilation enforcement can
//! shed its parser/type/WIR implementation dependencies as the
//! implementation moves above the kernel.

use super::{memory_of, read_slice_str, read_wstr, VmState};
use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::intrinsics;

/// The compiler services a trusted Witchy program may call.
pub(crate) trait CompilerServices: Send + Sync {
    /// The capability-footprint JSON of one source string.
    fn footprint(&self, src: &str) -> Result<String>;
    /// The footprint-diff JSON of two source strings.
    fn footprint_diff(&self, old_src: &str, new_src: &str) -> Result<String>;
    /// Markdown API docs for `src` under heading `name` (`witchy doc` output).
    fn doc(&self, name: &str, src: &str) -> Result<String>;
    /// The inspectable `compiler.try_doc` JSON result.
    fn doc_result_json(&self, name: &str, src: &str) -> Result<String>;
}

/// The default implementation: the same `compiler.*` natives the interpreter
/// exposes, resolved through the shared native registry at call time. The
/// intrinsic names double as the diagnostic labels, so the pre-injection
/// error messages are preserved byte for byte.
pub(crate) struct RegistryCompilerServices;

impl RegistryCompilerServices {
    fn call(name: &'static str, args: &[crate::value::NativeValue]) -> Result<String> {
        let f = crate::native::lookup(name)
            .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
        match f(args).map_err(|e| Error::msg(e.message))? {
            crate::value::NativeValue::Str(s) => Ok(s),
            _ => Err(Error::msg(format!("{name} did not return a String"))),
        }
    }
}

impl CompilerServices for RegistryCompilerServices {
    fn footprint(&self, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(intrinsics::COMPILER_FOOTPRINT, &[Value::Str(src.to_string())])
    }

    fn footprint_diff(&self, old_src: &str, new_src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DIFF,
            &[Value::Str(old_src.to_string()), Value::Str(new_src.to_string())],
        )
    }

    fn doc(&self, name: &str, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DOC,
            &[Value::Str(name.to_string()), Value::Str(src.to_string())],
        )
    }

    fn doc_result_json(&self, name: &str, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DOC_RESULT_JSON,
            &[Value::Str(name.to_string()), Value::Str(src.to_string())],
        )
    }
}

pub(super) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_footprint_len", host_compiler_footprint_len)?;
    linker.func_wrap("witchy", "compiler_diff_len", host_compiler_diff_len)?;
    linker.func_wrap("witchy", "compiler_footprint_into", host_compiler_footprint_into)?;
    linker.func_wrap("witchy", "compiler_diff_into", host_compiler_diff_into)?;
    linker.func_wrap("witchy", "compiler_doc_len", host_compiler_doc_len)?;
    linker.func_wrap(
        "witchy",
        "compiler_doc_result_json_len",
        host_compiler_doc_result_json_len,
    )?;
    Ok(())
}

/// `compiler_footprint_len(src_ptr) -> Int`: compute the capability-footprint
/// JSON of the guest's source string through the injected compiler services,
/// stage it for `fill_pending`, and report its byte length.
fn host_compiler_footprint_len(mut caller: Caller<'_, VmState>, src_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let json = caller.data().compiler_services.clone().footprint(&src)?;
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_diff_len(old_ptr, new_ptr) -> Int`: compute the footprint-diff
/// JSON of two guest source strings, stage it for `fill_pending`, and report
/// its byte length.
fn host_compiler_diff_len(
    mut caller: Caller<'_, VmState>,
    old_ptr: i32,
    new_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let old_src = read_wstr(mem.data(&caller), old_ptr)?;
    let new_src = read_wstr(mem.data(&caller), new_ptr)?;
    let json = caller.data().compiler_services.clone().footprint_diff(&old_src, &new_src)?;
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_footprint_into(src_ptr, src_len, out_ptr, out_capacity)`: compute
/// the footprint JSON from an explicitly bounded UTF-8 source slice. The
/// complete output range is checked before invoking the service; a short
/// output buffer receives no prefix and the required byte length is returned.
fn host_compiler_footprint_into(
    mut caller: Caller<'_, VmState>,
    src_ptr: i32,
    src_len: i32,
    out_ptr: i32,
    out_capacity: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let src = read_unsigned_slice_str(data, src_ptr, src_len)?;
    checked_guest_range(data.len(), out_ptr, out_capacity, "compiler output")?;
    let json = caller.data().compiler_services.clone().footprint(&src)?;
    let bytes = json.into_bytes();
    write_result_bytes(&mut caller, out_ptr, out_capacity, &bytes)
}

/// `compiler_diff_into(old_ptr, old_len, new_ptr, new_len, out_ptr,
/// out_capacity)`: the bounded, non-staged counterpart to `compiler_diff_len`.
fn host_compiler_diff_into(
    mut caller: Caller<'_, VmState>,
    old_ptr: i32,
    old_len: i32,
    new_ptr: i32,
    new_len: i32,
    out_ptr: i32,
    out_capacity: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let old_src = read_unsigned_slice_str(data, old_ptr, old_len)?;
    let new_src = read_unsigned_slice_str(data, new_ptr, new_len)?;
    checked_guest_range(data.len(), out_ptr, out_capacity, "compiler output")?;
    let json = caller
        .data()
        .compiler_services
        .clone()
        .footprint_diff(&old_src, &new_src)?;
    let bytes = json.into_bytes();
    write_result_bytes(&mut caller, out_ptr, out_capacity, &bytes)
}

fn checked_guest_range(data_len: usize, ptr: i32, len: i32, what: &str) -> Result<(usize, usize)> {
    let start = u64::from(u32::from_ne_bytes(ptr.to_ne_bytes()));
    let length = u64::from(u32::from_ne_bytes(len.to_ne_bytes()));
    let end = start + length;
    if end > data_len as u64 {
        return Err(Error::msg(format!("{what} ({start}..{end}) lies outside guest memory")));
    }
    Ok((start as usize, length as usize))
}

fn read_unsigned_slice_str(data: &[u8], ptr: i32, len: i32) -> Result<String> {
    let (start, length) = checked_guest_range(data.len(), ptr, len, "host input")?;
    std::str::from_utf8(&data[start..start + length])
        .map(str::to_owned)
        .map_err(|e| Error::msg(format!("host slice is not valid UTF-8: {e}")))
}

fn write_result_bytes(caller: &mut Caller<'_, VmState>, out_ptr: i32, out_capacity: i32, bytes: &[u8]) -> Result<i32> {
    let mem = memory_of(caller)?;
    write_result_bytes_to_memory(mem.data_mut(caller), out_ptr, out_capacity, bytes)
}

fn write_result_bytes_to_memory(data: &mut [u8], out_ptr: i32, out_capacity: i32, bytes: &[u8]) -> Result<i32> {
    let (start, capacity) = checked_guest_range(data.len(), out_ptr, out_capacity, "compiler output")?;
    let required = required_result_size(bytes.len())?;
    if bytes.len() <= capacity {
        data[start..start + bytes.len()].copy_from_slice(bytes);
    }
    Ok(required)
}

fn required_result_size(byte_len: usize) -> Result<i32> {
    let required = u32::try_from(byte_len)
        .map_err(|_| Error::msg("compiler service result exceeds wasm32 byte length"))?;
    Ok(required as i32)
}

/// `compiler_doc_len(name_ptr, src_ptr) -> Int`: render the guest's source string to
/// Markdown API docs (the `compiler.doc` native — `witchy doc` output) under heading
/// `name`, stage it for `fill_pending`, and report its byte length.
fn host_compiler_doc_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let md = caller.data().compiler_services.clone().doc(&name, &src)?;
    let len = md.len() as i32;
    caller.data_mut().pending = Some(md.into_bytes());
    Ok(len)
}

/// `compiler_doc_result_json_len(name_ptr, src_ptr) -> Int`: render the
/// inspectable `compiler.try_doc` JSON result, stage it for `fill_pending`, and
/// report its byte length.
fn host_compiler_doc_result_json_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let json = caller.data().compiler_services.clone().doc_result_json(&name, &src)?;
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::{checked_guest_range, read_slice_str, required_result_size, write_result_bytes_to_memory};
    use crate::runtime::{Capabilities, Runtime};
    use std::sync::Arc;
    use wasmtime::Error;

    struct FixtureCompilerServices;

    impl super::CompilerServices for FixtureCompilerServices {
        fn footprint(&self, src: &str) -> wasmtime::Result<String> {
            Ok(format!(r#"{{"footprint":{}}}"#, src.len()))
        }

        fn footprint_diff(&self, old_src: &str, new_src: &str) -> wasmtime::Result<String> {
            Ok(format!(r#"{{"old":{},"new":{}}}"#, old_src.len(), new_src.len()))
        }

        fn doc(&self, _name: &str, _src: &str) -> wasmtime::Result<String> {
            Err(Error::msg("fixture does not provide compiler docs"))
        }

        fn doc_result_json(&self, _name: &str, _src: &str) -> wasmtime::Result<String> {
            Err(Error::msg("fixture does not provide compiler doc results"))
        }
    }

    const FOOTPRINT_FIXTURE: &str = r#"
        (module
          (import "witchy" "compiler_footprint_into"
            (func $footprint (param i32 i32 i32 i32) (result i32)))
          (import "witchy" "compiler_diff_into"
            (func $diff (param i32 i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "footprint")
            (param $src i32) (param $src_len i32)
            (param $out i32) (param $out_capacity i32) (result i32)
            local.get $src
            local.get $src_len
            local.get $out
            local.get $out_capacity
            call $footprint)
          (func (export "diff")
            (param $old i32) (param $old_len i32)
            (param $new i32) (param $new_len i32)
            (param $out i32) (param $out_capacity i32) (result i32)
            local.get $old
            local.get $old_len
            local.get $new
            local.get $new_len
            local.get $out
            local.get $out_capacity
            call $diff))
    "#;

    const SOURCE: &str = "pub fn answer() -> Int:\n    1\n";
    const UPDATED_SOURCE: &str = "pub fn answer() -> Int:\n    2\n";
    const SOURCE_PTR: i32 = 0;
    const UPDATED_SOURCE_PTR: i32 = 256;
    const OUTPUT_PTR: i32 = 1024;

    fn fixture_vm() -> (Runtime, crate::runtime::Vm) {
        let mut runtime = Runtime::new().expect("runtime");
        runtime.compiler_services = Arc::new(FixtureCompilerServices);
        let vm = runtime
            .spawn(FOOTPRINT_FIXTURE, Capabilities::none(), 1)
            .expect("the runtime linker must register the bounded compiler imports");
        (runtime, vm)
    }

    fn write_fixture_inputs(vm: &mut crate::runtime::Vm) {
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
        memory
            .write(&mut vm.store, SOURCE_PTR as usize, SOURCE.as_bytes())
            .unwrap();
        memory
            .write(
                &mut vm.store,
                UPDATED_SOURCE_PTR as usize,
                UPDATED_SOURCE.as_bytes(),
            )
            .unwrap();
    }

    #[test]
    fn bounded_compiler_result_requires_full_capacity_and_valid_utf8_input() {
        let mut memory = vec![0xa5; 32];
        memory[2..5].copy_from_slice("old".as_bytes());
        assert_eq!(read_slice_str(&memory, 2, 3).unwrap(), "old");

        let required = write_result_bytes_to_memory(&mut memory, 8, 3, b"abc").unwrap();
        assert_eq!(required, 3);
        assert_eq!(&memory[8..11], b"abc");

        memory[12..16].fill(0xa5);
        let required = write_result_bytes_to_memory(&mut memory, 12, 2, b"abcd").unwrap();
        assert_eq!(required, 4);
        assert_eq!(&memory[12..16], &[0xa5; 4]);

        assert!(read_slice_str(&memory, 2, 4).is_err());
        assert!(write_result_bytes_to_memory(&mut memory, 31, 2, b"x").is_err());
    }

    #[test]
    fn bounded_compiler_lanes_preserve_unsigned_wasm32_bits() {
        let wasm32_max = u32::MAX as usize;
        assert_eq!(checked_guest_range(wasm32_max, 0, -1, "input").unwrap(), (0, wasm32_max));
        assert_eq!(checked_guest_range(wasm32_max, -1, 0, "input").unwrap(), (wasm32_max, 0));
        assert_eq!(required_result_size(0x8000_0000usize).unwrap(), i32::MIN);
        assert!(required_result_size((u32::MAX as usize) + 1).is_err());
    }

    #[test]
    fn bounded_compiler_imports_execute_utf8_bounds_and_no_partial_write() {
        let (_runtime, mut vm) = fixture_vm();
        write_fixture_inputs(&mut vm);
        let footprint = vm
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut vm.store, "footprint")
            .unwrap();
        let diff = vm
            .instance
            .get_typed_func::<(i32, i32, i32, i32, i32, i32), i32>(&mut vm.store, "diff")
            .unwrap();
        let expected_footprint = format!(r#"{{"footprint":{}}}"#, SOURCE.len()).into_bytes();
        let expected_diff = format!(
            r#"{{"old":{},"new":{}}}"#,
            SOURCE.len(),
            UPDATED_SOURCE.len()
        )
        .into_bytes();
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();

        memory
            .write(&mut vm.store, OUTPUT_PTR as usize, &[0xa5; 4096])
            .unwrap();
        let footprint_len = footprint
            .call(
                &mut vm.store,
                (SOURCE_PTR, SOURCE.len() as i32, OUTPUT_PTR, expected_footprint.len() as i32),
            )
            .unwrap();
        assert_eq!(footprint_len as usize, expected_footprint.len());
        assert_eq!(
            &memory.data(&vm.store)[OUTPUT_PTR as usize..][..expected_footprint.len()],
            expected_footprint
        );

        memory
            .write(&mut vm.store, OUTPUT_PTR as usize, &[0xa5; 4096])
            .unwrap();
        let short_len = footprint
            .call(
                &mut vm.store,
                (
                    SOURCE_PTR,
                    SOURCE.len() as i32,
                    OUTPUT_PTR,
                    (expected_footprint.len() - 1) as i32,
                ),
            )
            .unwrap();
        assert_eq!(short_len as usize, expected_footprint.len());
        assert_eq!(&memory.data(&vm.store)[OUTPUT_PTR as usize..][..4096], &[0xa5; 4096]);

        let diff_len = diff
            .call(
                &mut vm.store,
                (
                    SOURCE_PTR,
                    SOURCE.len() as i32,
                    UPDATED_SOURCE_PTR,
                    UPDATED_SOURCE.len() as i32,
                    OUTPUT_PTR,
                    expected_diff.len() as i32,
                ),
            )
            .unwrap();
        assert_eq!(diff_len as usize, expected_diff.len());
        assert_eq!(
            &memory.data(&vm.store)[OUTPUT_PTR as usize..][..expected_diff.len()],
            expected_diff
        );

        let (_runtime, mut invalid_vm) = fixture_vm();
        let invalid_memory = invalid_vm
            .instance
            .get_memory(&mut invalid_vm.store, "memory")
            .unwrap();
        invalid_memory
            .write(&mut invalid_vm.store, SOURCE_PTR as usize, &[0xff])
            .unwrap();
        let invalid_footprint = invalid_vm
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut invalid_vm.store, "footprint")
            .unwrap();
        assert!(invalid_footprint
            .call(&mut invalid_vm.store, (SOURCE_PTR, 1, OUTPUT_PTR, 16))
            .is_err());

        let (_runtime, mut bounds_vm) = fixture_vm();
        write_fixture_inputs(&mut bounds_vm);
        let bounds_footprint = bounds_vm
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut bounds_vm.store, "footprint")
            .unwrap();
        assert!(bounds_footprint
            .call(&mut bounds_vm.store, (SOURCE_PTR, SOURCE.len() as i32, 65_535, 2))
            .is_err());
        assert!(bounds_footprint
            .call(&mut bounds_vm.store, (65_535, 2, OUTPUT_PTR, 16))
            .is_err());
    }
}
