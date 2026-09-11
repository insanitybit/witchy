use super::*;

#[test]
fn response_kind_bounds_and_immutable_short_write() -> Result<()> {
    let mut runtime = crate::runtime::Runtime::batch()?;
    let mut vm = runtime.spawn(br#"(module
        (import "witchy" "vm_service_response_size" (func $size (param externref) (result i32)))
        (import "witchy" "vm_service_response_into" (func $into (param externref i32 i32) (result i32)))
        (import "witchy" "vm_service_close" (func $close (param externref)))
        (import "witchy" "vm_service_step" (func $step (param externref i32 i32) (result externref)))
        (memory (export "memory") 1)
        (func (export "size") (param externref) (result i32) local.get 0 call $size)
        (func (export "into") (param externref i32 i32) (result i32) local.get 0 local.get 1 local.get 2 call $into)
        (func (export "close") (param externref) local.get 0 call $close)
        (func (export "step") (param externref i32 i32) (result externref) local.get 0 local.get 1 local.get 2 call $step))"#,
        Capabilities::default(), 1)?;
    let size = vm.instance.get_typed_func::<Option<Rooted<ExternRef>>, i32>(&mut vm.store, "size")?;
    let into = vm.instance.get_typed_func::<(Option<Rooted<ExternRef>>, i32, i32), i32>(&mut vm.store, "into")?;
    let close = vm.instance.get_typed_func::<Option<Rooted<ExternRef>>, ()>(&mut vm.store, "close")?;
    let step = vm.instance.get_typed_func::<(Option<Rooted<ExternRef>>, i32, i32), Option<Rooted<ExternRef>>>(&mut vm.store, "step")?;
    let forged = ExternRef::new(&mut vm.store, 7i64)?;
    let response = ExternRef::new(&mut vm.store, Response(Arc::from([0, 255, 128])))?;
    assert!(size.call(&mut vm.store, None).is_err());
    assert!(size.call(&mut vm.store, Some(forged)).is_err());
    assert!(close.call(&mut vm.store, Some(response)).is_err());
    assert!(step.call(&mut vm.store, (Some(response), 0, 0)).is_err());
    assert_eq!(size.call(&mut vm.store, Some(response))?, 3);
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 16, &[42; 4])?;
    assert_eq!(into.call(&mut vm.store, (Some(response), 16, 2))?, 3);
    assert_eq!(&memory.data(&vm.store)[16..20], &[42; 4]);
    assert!(into.call(&mut vm.store, (Some(response), -1, 3)).is_err());
    assert!(into.call(&mut vm.store, (Some(response), 16, -1)).is_err());
    assert_eq!(into.call(&mut vm.store, (Some(response), 16, 4))?, 3);
    assert_eq!(&memory.data(&vm.store)[16..20], &[0, 255, 128, 42]);
    assert_eq!(into.call(&mut vm.store, (Some(response), 24, 3))?, 3);
    assert_eq!(&memory.data(&vm.store)[24..27], &[0, 255, 128]);
    let service = Service { state: Arc::new(Mutex::new(ServiceState::Failed)), metrics: vm.store.data().vm_services.clone() };
    let token = ExternRef::new(&mut vm.store, service)?;
    assert!(size.call(&mut vm.store, Some(token)).is_err());
    assert!(step.call(&mut vm.store, (Some(token), 0, 0)).is_err());
    close.call(&mut vm.store, Some(token))?;
    assert!(close.call(&mut vm.store, Some(token)).is_err());
    assert!(step.call(&mut vm.store, (Some(token), 0, 0)).is_err());
    assert_eq!(size.call(&mut vm.store, Some(response))?, 3);
    assert_eq!(into.call(&mut vm.store, (Some(response), 32, 3))?, 3);
    assert_eq!(&memory.data(&vm.store)[32..35], &[0, 255, 128]);
    assert_eq!(vm.vm_service_metrics().closed, 1);
    Ok(())
}

#[test]
fn service_start_refuses_raw_exports_and_invalid_site_numbers() -> Result<()> {
    let mut runtime = crate::runtime::Runtime::batch()?;
    let mut vm = runtime.spawn(br#"(module
        (import "witchy" "vm_service_start" (func $start (param i64 i32 i32) (result externref)))
        (memory (export "memory") 1)
        (func (export "f0") (param i32 i32) (result i32) local.get 0)
        (func (export "start") (param i64) (result externref) local.get 0 i32.const 0 i32.const 0 call $start))"#,
        Capabilities::default(), 1)?;
    let start = vm.instance.get_typed_func::<i64, Option<Rooted<ExternRef>>>(&mut vm.store, "start")?;
    for id in [-1, 0, i64::from(u32::MAX) + 1] {
        assert!(start.call(&mut vm.store, id).is_err());
    }
    assert_eq!(vm.vm_service_metrics().started, 0);
    Ok(())
}

#[test]
fn service_copy_distinguishes_static_data_from_owned_binary_extent() -> Result<()> {
    let mut runtime = crate::runtime::Runtime::batch()?;
    let mut vm = runtime.spawn(br#"(module
        (memory (export "memory") 1)
        (global (export "__witchy_heap_base") i32 (i32.const 32))
        (global (export "__witchy_heap_bump") (mut i32) (i32.const 32))
        (data (i32.const 8) "\03\00\00\00\00\ff\80"))"#,
        Capabilities::default(), 1)?;
    assert_eq!(vm.copy_owned_bytes(8)?, (vec![0, 255, 128], false));
    let owned = vm.materialize_owned_bytes(&[255, 0, 128])?;
    assert_eq!(vm.copy_owned_bytes(owned)?, (vec![255, 0, 128], true));
    assert!(vm.copy_owned_bytes(0).is_err());
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, owned as usize, &99u32.to_le_bytes())?;
    assert!(vm.copy_owned_bytes(owned).is_err());
    memory.write(&mut vm.store, 8, &99u32.to_le_bytes())?;
    assert!(vm.copy_owned_bytes(8).is_err());
    Ok(())
}
