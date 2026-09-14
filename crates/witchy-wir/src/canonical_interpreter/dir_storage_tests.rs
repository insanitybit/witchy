//! Frozen storage probes using the oracle's existing semantic heap harness.
//! Filesystem operations below only set up/tear down explicit fallback fixtures.
use super::*;
use crate::canonical_safewir::{DIR_BYTES_RESPONSE_KIND, DIR_STORAGE_IMPORT_NAMES};

const RESPONSE: ValueId = ValueId::new(9002);

fn write(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>,
    name: &str, path: &[u8], data: &[u8]) -> Result<(), InterpretError>
{
    let path = buffer(engine, layout, values, path, false);
    let data = buffer(engine, layout, values, data, false);
    let destinations = if name == "dir_create_new_at" { vec![RESULT] } else { vec![] };
    engine.dir_storage_host_call(name, &destinations, &[HostCallArgument::Capability(CAP), path, data], values)
}

fn snapshot(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>, path: &[u8]) -> InterpreterValue {
    let path = buffer(engine, layout, values, path, false);
    engine.dir_storage_host_call("dir_read_bytes_at", &[RESULT], &[HostCallArgument::Capability(CAP), path], values).unwrap();
    values[&RESULT].clone()
}

#[test]
fn dir_storage_oracle_frozen_abi_and_response_issuer_refusals() {
    assert_eq!(DIR_BYTES_RESPONSE_KIND.get(), 22);
    assert_eq!(DIR_STORAGE_IMPORT_NAMES, ["dir_create_new_at", "dir_replace_at", "dir_read_bytes_at",
        "dir_bytes_response_size", "dir_bytes_response_into", "dir_write_bytes_at",
        "dir_append_bytes_at", "dir_set_executable_at", "dir_rename_at"]);
    let owner = State::new();
    let other = State::new();
    let token = owner.mint_dir_bytes(vec![0, 255]);
    assert_eq!(&*owner.dir_bytes(&token.clone()).unwrap(), &[0, 255]);
    assert!(other.dir_bytes(&token).is_err());
    assert!(owner.dir_bytes(&CapabilityToken::root(22)).is_err());
    let mut wrong = token.clone(); wrong.kind = DIR_KIND;
    assert!(owner.dir_bytes(&wrong).is_err());
    let mut wrong = token.clone(); wrong.scope = "elsewhere".into();
    assert!(owner.dir_bytes(&wrong).is_err());
    assert!(owner.storage_scope(&token).is_err());
    let (module, _) = module(vec![]);
    let engine = Engine::new(module.module(), config());
    for request in [InterpreterValue::capability(22), InterpreterValue::Capability(token.clone()),
        InterpreterValue::NullableCapability(Some(token))]
    {
        assert!(engine.bind_registry_entry(vec![request]).is_err(), "responses never grant entry authority");
    }
    engine.bind_registry_entry(vec![InterpreterValue::capability(DIR_KIND)]).unwrap();
}

#[test]
fn dir_storage_oracle_create_new_and_atomic_replace() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.directories.insert("folder".into());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    write(&mut engine, layout, &mut values, "dir_create_new_at", b"./folder/./note", b"first").unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Bool(true));
    let before = engine.config.fixture.clone();
    write(&mut engine, layout, &mut values, "dir_create_new_at", b"folder/note", b"loser").unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Bool(false));
    assert_eq!(engine.config.fixture, before);
    write(&mut engine, layout, &mut values, "dir_create_new_at", b"folder", b"directory collision").unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Bool(false));
    assert_eq!(engine.config.fixture, before, "a named existing directory loses exclusive creation without becoming a file");
    write(&mut engine, layout, &mut values, "dir_replace_at", b"folder/note", b"complete replacement").unwrap();
    assert_eq!(engine.config.fixture.files["folder/note"], b"complete replacement");
    write(&mut engine, layout, &mut values, "dir_replace_at", b"folder/absent", b"created").unwrap();
    assert_eq!(engine.config.fixture.files["folder/absent"], b"created");
    assert_eq!(engine.config.fixture.files.len(), 2, "no pending file or temporary sibling is published");
    assert_eq!(engine.config.fixture.directories, before.directories);
}

#[test]
fn dir_storage_oracle_snapshots_survive_writes_and_short_copies() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    let original = [0, 255, 128, b'A'];
    write(&mut engine, layout, &mut values, "dir_write_bytes_at", b"file", &original).unwrap();
    let first = snapshot(&mut engine, layout, &mut values, b"./file");
    write(&mut engine, layout, &mut values, "dir_write_bytes_at", b"file", &[1, 2]).unwrap();
    let second = snapshot(&mut engine, layout, &mut values, b"file");
    engine.config.fixture.files.remove("file");
    for (response, expected) in [(first, original.to_vec()), (second, vec![1, 2])] {
        values.insert(RESPONSE, response);
        for capacity in [0, 1, 2, 3, 4, 5] {
            engine.dir_storage_host_call("dir_bytes_response_size", &[RESULT], &[HostCallArgument::Capability(RESPONSE)], &mut values).unwrap();
            assert_eq!(values[&RESULT], InterpreterValue::Size(expected.len() as u64));
            let output = buffer(&mut engine, layout, &mut values, &vec![b'!'; capacity], true);
            engine.dir_storage_host_call("dir_bytes_response_into", &[RESULT], &[HostCallArgument::Capability(RESPONSE), output.clone()], &mut values).unwrap();
            assert_eq!(values[&RESULT], InterpreterValue::Size(expected.len() as u64));
            let mut wanted = vec![b'!'; capacity];
            if capacity >= expected.len() { wanted[..expected.len()].copy_from_slice(&expected); }
            assert_eq!(bytes(&engine, &output, &values), wanted);
        }
    }
    assert!(engine.config.fixture.files.is_empty(), "response reads do not recreate or reread a file");
}

#[test]
fn dir_storage_oracle_response_length_signed_abi_boundary_without_allocation() {
    use crate::canonical_interpreter::dir_storage::response_length;
    for length in [0, 1, i32::MAX as usize - 1, i32::MAX as usize] {
        assert_eq!(response_length(length), Ok(length as u64));
    }
    for length in [i32::MAX as usize + 1, u32::MAX as usize, usize::MAX] {
        assert_eq!(response_length(length),
            Err(InterpretError::RegistryFixture("Dir byte response exceeds the guest ABI size limit".into())));
    }
}

#[test]
fn dir_storage_oracle_empty_response_has_typed_zero() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    write(&mut engine, layout, &mut values, "dir_write_bytes_at", b"empty", b"").unwrap();
    let response = snapshot(&mut engine, layout, &mut values, b"empty");
    values.insert(RESPONSE, response);
    for capacity in [0, 1] {
        let output = buffer(&mut engine, layout, &mut values, &vec![b'?'; capacity], true);
        for name in ["dir_bytes_response_size", "dir_bytes_response_into"] {
            let mut args = vec![HostCallArgument::Capability(RESPONSE)];
            if name.ends_with("into") { args.push(output.clone()); }
            engine.dir_storage_host_call(name, &[RESULT], &args, &mut values).unwrap();
            assert_eq!(values[&RESULT], InterpreterValue::Size(0));
        }
        assert_eq!(bytes(&engine, &output, &values), vec![b'?'; capacity]);
    }
}

#[test]
fn dir_storage_oracle_text_and_full_input_preflight_before_existing_false() {
    let (module, layout) = module(vec![]);
    for name in ["dir_create_new_at", "dir_replace_at", "dir_write_bytes_at"] {
        for failure in ["path-utf8", "missing-cell", "non-byte", "past-end", "write-input", "arity", "forged-dir"] {
            let mut engine = Engine::new(module.module(), config());
            engine.config.fixture.files.insert("file".into(), b"unchanged".to_vec());
            let before = engine.config.fixture.clone();
            let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND)), (RESULT, InterpreterValue::Int(777))]);
            let path_bytes: &[u8] = if failure == "path-utf8" { &[255] } else { b"file" };
            let path = buffer(&mut engine, layout, &mut values, path_bytes, false);
            let data = buffer(&mut engine, layout, &mut values, b"data", failure == "write-input");
            let (object, _, _) = engine.slice_place(place(&data), &values).unwrap();
            match failure {
                "missing-cell" => { engine.objects.get_mut(&object).unwrap().cells.remove(&CellKey::Element(3)); }
                "non-byte" => { engine.objects.get_mut(&object).unwrap().cells.insert(CellKey::Element(3), InterpreterValue::Size(256)); }
                "past-end" => {
                    let Projection::Slice { length, .. } = place(&data).projection() else { unreachable!() };
                    values.insert(*length, InterpreterValue::Size(5));
                }
                "forged-dir" => { values.insert(CAP, InterpreterValue::capability(DIR_KIND)); }
                _ => {},
            }
            let results = if name == "dir_create_new_at" { vec![RESULT] } else { vec![] };
            let results = if failure == "arity" { vec![RESULT, RESULT] } else { results };
            assert!(engine.dir_storage_host_call(name, &results, &[HostCallArgument::Capability(CAP), path, data], &mut values).is_err(), "{name}/{failure}");
            assert_eq!(engine.config.fixture, before);
            assert_eq!(values[&RESULT], InterpreterValue::Int(777));
        }
    }
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.files.insert("file".into(), b"old".to_vec());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    for name in ["dir_create_new_at", "dir_replace_at"] {
        assert_eq!(write(&mut engine, layout, &mut values, name, b"file", &[255]),
            Err(InterpretError::TypeMismatch("host string is not UTF-8")));
        assert_eq!(engine.config.fixture.files["file"], b"old");
    }
    write(&mut engine, layout, &mut values, "dir_replace_at", b"file", b"\xc3\xa9").unwrap();
    assert_eq!(engine.config.fixture.files["file"], b"\xc3\xa9");
}

#[test]
fn dir_storage_oracle_response_output_preflight_is_atomic() {
    let (module, layout) = module(vec![]);
    for capacity in [0, 1, 4, 8] {
        for failure in ["readonly", "static", "read-place", "past-end", "overflow", "foreign", "root", "wrong-kind"] {
            let mut engine = Engine::new(module.module(), config());
            let response = engine.registry.mint_dir_bytes(vec![0, 255, 2, 3]);
            let mut values = BTreeMap::from([(RESPONSE, InterpreterValue::Capability(response)), (RESULT, InterpreterValue::Int(777))]);
            let output = buffer(&mut engine, layout, &mut values, &vec![b'?'; capacity], failure != "read-place");
            let (object, _, _) = engine.slice_place(place(&output), &values).unwrap();
            let before = engine.objects[&object].cells.clone();
            match failure {
                "readonly" => engine.objects.get_mut(&object).unwrap().immutable = true,
                "static" => engine.objects.get_mut(&object).unwrap().storage = RuntimeStorage::StaticData,
                "past-end" | "overflow" => {
                    let Projection::Slice { start, length } = place(&output).projection() else { unreachable!() };
                    values.insert(*length, InterpreterValue::Size(capacity as u64 + 1));
                    if failure == "overflow" { values.insert(*start, InterpreterValue::Size(u64::MAX)); }
                }
                "foreign" => { values.insert(RESPONSE, InterpreterValue::Capability(State::new().mint_dir_bytes(vec![9]))); }
                "root" => { values.insert(RESPONSE, InterpreterValue::capability(22)); }
                "wrong-kind" => { values.insert(RESPONSE, bound(&engine, DIR_KIND)); }
                _ => {},
            }
            assert!(engine.dir_storage_host_call("dir_bytes_response_into", &[RESULT],
                &[HostCallArgument::Capability(RESPONSE), output], &mut values).is_err(), "{failure}/{capacity}");
            assert_eq!(engine.objects[&object].cells, before);
            assert_eq!(values[&RESULT], InterpreterValue::Int(777));
        }
    }
}

#[test]
fn dir_storage_oracle_scoped_paths_missing_parents_and_directory_collisions() {
    let (module, layout) = module(vec![]);
    for name in ["dir_create_new_at", "dir_replace_at", "dir_write_bytes_at", "dir_read_bytes_at"] {
        for path in ["", ".", "./", "../escape", "/absolute", "a/../escape", "bad\0name", "missing/file", "file/child"] {
            let mut engine = Engine::new(module.module(), config());
            engine.config.fixture.directories.insert("scope".into());
            engine.config.fixture.files.insert("scope/file".into(), b"kept".to_vec());
            let before = engine.config.fixture.clone();
            let InterpreterValue::Capability(root) = bound(&engine, DIR_KIND) else { unreachable!() };
            let child = engine.registry.narrow_dir(&root, "scope".into()).unwrap();
            let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(child))]);
            let result = if name == "dir_read_bytes_at" {
                let path = buffer(&mut engine, layout, &mut values, path.as_bytes(), false);
                engine.dir_storage_host_call(name, &[RESULT], &[HostCallArgument::Capability(CAP), path], &mut values)
            } else { write(&mut engine, layout, &mut values, name, path.as_bytes(), b"data") };
            assert!(result.is_err(), "{name}/{path:?}");
            assert_eq!(engine.config.fixture, before);
        }
    }
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.directories.insert("directory".into());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    for name in ["dir_replace_at", "dir_write_bytes_at"] {
        assert!(write(&mut engine, layout, &mut values, name, b"directory", b"data").is_err());
        assert!(engine.config.fixture.files.is_empty());
    }
    let path = buffer(&mut engine, layout, &mut values, b"directory", false);
    assert!(engine.dir_storage_host_call("dir_read_bytes_at", &[RESULT], &[HostCallArgument::Capability(CAP), path], &mut values).is_err());
}

#[test]
fn dir_storage_oracle_scoped_curdir_write_and_read_stay_under_grant() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.directories.insert("scope/inner".into());
    engine.config.fixture.files.insert("note".into(), b"outside grant".to_vec());
    let InterpreterValue::Capability(root) = bound(&engine, DIR_KIND) else { unreachable!() };
    let child = engine.registry.narrow_dir(&root, "scope".into()).unwrap();
    let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(child))]);
    write(&mut engine, layout, &mut values, "dir_write_bytes_at", b"./inner/./note", &[255, 0]).unwrap();
    let response = snapshot(&mut engine, layout, &mut values, b"inner//note");
    values.insert(RESPONSE, response);
    let output = buffer(&mut engine, layout, &mut values, &[0; 2], true);
    engine.dir_storage_host_call("dir_bytes_response_into", &[RESULT],
        &[HostCallArgument::Capability(RESPONSE), output.clone()], &mut values).unwrap();
    assert_eq!(bytes(&engine, &output, &values), [255, 0]);
    assert_eq!(engine.config.fixture.files["scope/inner/note"], [255, 0]);
    assert_eq!(engine.config.fixture.files["note"], b"outside grant");
    assert!(!engine.config.fixture.files.contains_key("inner/note"));
    let missing = buffer(&mut engine, layout, &mut values, b"absent", false);
    let before = values[&RESULT].clone();
    assert!(engine.dir_storage_host_call("dir_read_bytes_at", &[RESULT],
        &[HostCallArgument::Capability(CAP), missing], &mut values).is_err());
    assert_eq!(values[&RESULT], before, "failed reads never publish a response");
}

#[test]
fn dir_storage_oracle_aliased_inputs_are_snapshotted() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    let input = buffer(&mut engine, layout, &mut values, b"name", false);
    engine.dir_storage_host_call("dir_create_new_at", &[RESULT],
        &[HostCallArgument::Capability(CAP), input.clone(), input.clone()], &mut values).unwrap();
    assert_eq!(engine.config.fixture.files["name"], b"name");
    assert_eq!(bytes(&engine, &input, &values), b"name");
}

#[test]
fn dir_storage_oracle_fallback_leaf_symlinks_preserve_provider_operations() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("witchy-dir-storage-{}-{}", std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(root.clone());
    std::fs::write(root.join("file"), [0, 255]).unwrap();
    std::fs::create_dir(root.join("directory")).unwrap();
    let (module, layout) = module(vec![]);
    let mut cfg = config(); cfg.fixture.root = Some(root.clone());
    let mut engine = Engine::new(module.module(), cfg);
    let mut values = BTreeMap::from([(CAP, bound(&engine, DIR_KIND))]);
    let response = snapshot(&mut engine, layout, &mut values, b"file");
    values.insert(RESPONSE, response);
    write(&mut engine, layout, &mut values, "dir_create_new_at", b"file", b"loser").unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Bool(false));
    write(&mut engine, layout, &mut values, "dir_replace_at", b"file", b"virtual").unwrap();
    write(&mut engine, layout, &mut values, "dir_write_bytes_at", b"directory/new", &[255]).unwrap();
    assert_eq!(std::fs::read(root.join("file")).unwrap(), [0, 255]);
    assert!(!root.join("directory/new").exists());
    std::fs::write(root.join("file"), b"changed fixture").unwrap();
    let output = buffer(&mut engine, layout, &mut values, &[0; 2], true);
    engine.dir_storage_host_call("dir_bytes_response_into", &[RESULT], &[HostCallArgument::Capability(RESPONSE), output.clone()], &mut values).unwrap();
    assert_eq!(bytes(&engine, &output, &values), [0, 255], "one snapshot, not a size/copy reread");
    #[cfg(unix)]
    for (name, target) in [("inside", "file"), ("outside", "/"), ("dangling", "absent"), ("parent", "directory")] {
        std::os::unix::fs::symlink(target, root.join(name)).unwrap();
        for operation in ["dir_create_new_at", "dir_replace_at", "dir_write_bytes_at", "dir_read_bytes_at"] {
            let path = format!("./{name}/./child");
            let before = engine.config.fixture.clone();
            let result = if operation == "dir_read_bytes_at" {
                let path = buffer(&mut engine, layout, &mut values, path.as_bytes(), false);
                engine.dir_storage_host_call(operation, &[RESULT], &[HostCallArgument::Capability(CAP), path], &mut values)
            } else { write(&mut engine, layout, &mut values, operation, path.as_bytes(), b"data") };
            assert!(result.is_err(), "intermediate link: {operation}/{path}");
            assert_eq!(engine.config.fixture, before);
        }
        let before = engine.config.fixture.clone();
        assert!(write(&mut engine, layout, &mut values, "dir_write_bytes_at", name.as_bytes(), b"data").is_err());
        let path = buffer(&mut engine, layout, &mut values, name.as_bytes(), false);
        assert!(engine.dir_storage_host_call("dir_read_bytes_at", &[RESULT],
            &[HostCallArgument::Capability(CAP), path], &mut values).is_err());
        assert_eq!(engine.config.fixture, before);
        for operation in ["dir_create_new_at", "dir_replace_at"] {
            assert!(write(&mut engine, layout, &mut values, operation, name.as_bytes(), &[255]).is_err());
            assert_eq!(engine.config.fixture, before, "text preflight precedes every leaf-link result");
        }
        write(&mut engine, layout, &mut values, "dir_create_new_at", name.as_bytes(), b"loser").unwrap();
        assert_eq!(values[&RESULT], InterpreterValue::Bool(false));
        assert_eq!(engine.config.fixture, before, "an existing leaf link loses exclusive creation");
        write(&mut engine, layout, &mut values, "dir_replace_at", name.as_bytes(), b"local replacement").unwrap();
        assert_eq!(engine.config.fixture.files[name], b"local replacement");
        assert!(std::fs::symlink_metadata(root.join(name)).unwrap().file_type().is_symlink(), "replacement is fixture-only");
        assert_eq!(std::fs::read_link(root.join(name)).unwrap(), std::path::PathBuf::from(target));
        assert_eq!(std::fs::read(root.join("file")).unwrap(), b"changed fixture", "the link target is never written");
        let response = snapshot(&mut engine, layout, &mut values, name.as_bytes());
        values.insert(RESPONSE, response);
        let output = buffer(&mut engine, layout, &mut values, &[0; 17], true);
        engine.dir_storage_host_call("dir_bytes_response_into", &[RESULT],
            &[HostCallArgument::Capability(RESPONSE), output.clone()], &mut values).unwrap();
        assert_eq!(bytes(&engine, &output, &values), b"local replacement");
        write(&mut engine, layout, &mut values, "dir_write_bytes_at", name.as_bytes(), &[255]).unwrap();
        assert_eq!(engine.config.fixture.files[name], [255], "after replacement the virtual entry is a regular file");
        assert!(write(&mut engine, layout, &mut values, "dir_replace_at", format!("{name}/child").as_bytes(), b"data").is_err());
    }
    engine.config.fixture.root = Some(root.join("file/child"));
    let before = engine.config.fixture.clone();
    assert!(write(&mut engine, layout, &mut values, "dir_create_new_at", b"file", b"data").is_err());
    assert_eq!(engine.config.fixture, before, "metadata errors are not an existing-file false result");
}

#[test]
fn dir_storage_oracle_isolated_and_server_channels_fail_closed() {
    let (module, _) = module(vec![]);
    for isolated in [false, true] {
        let mut cfg = config();
        if !isolated { cfg.fixture.server_pool = Some(OracleServerPoolFixture::default()); }
        let mut engine = Engine::new(module.module(), cfg);
        if !isolated { engine.bind_registry_entry(vec![InterpreterValue::capability(DIR_KIND)]).unwrap(); }
        engine.isolated = isolated;
        for name in DIR_STORAGE_IMPORT_NAMES {
            let expected = if isolated { InterpretError::UnsupportedHostImport }
                else { InterpretError::UnsupportedOperation("server pool directory storage effect ordering") };
            assert_eq!(engine.dir_storage_host_call(name, &[], &[], &mut BTreeMap::new()), Err(expected));
        }
    }
}
