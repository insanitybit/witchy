//! Independent fixture-state and checked-buffer probes; no native provider calls.
use super::*;
use crate::canonical_interpreter::dir_storage::INSTALLER_IMPORTS;

fn invoke(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>,
    name: &str, first: &[u8], second: &[u8]) -> Result<(), InterpretError>
{
    let first = buffer(engine, layout, values, first, false);
    let mut arguments = vec![HostCallArgument::Capability(CAP), first];
    if name != "dir_set_executable_at" { arguments.push(buffer(engine, layout, values, second, false)); }
    engine.dir_storage_host_call(name, &[], &arguments, values)
}

#[test]
fn dir_installer_oracle_rejects_transport_and_forged_tokens_before_mutation() {
    let (module, layout) = module(vec![]);
    for name in INSTALLER_IMPORTS {
        for failure in ["root", "foreign", "kind", "scope", "utf8", "escape", "uninitialized", "bounds", "write-slice", "arity"] {
            let mut engine = Engine::new(module.module(), config());
            engine.config.fixture.files.insert("source".into(), b"kept".to_vec());
            engine.config.fixture.files.insert("target".into(), b"old".to_vec());
            let before = engine.config.fixture.clone();
            let mut token = bound(&engine, DIR_KIND);
            match failure {
                "root" => token = InterpreterValue::capability(DIR_KIND),
                "foreign" => token = bound(&Engine::new(module.module(), config()), DIR_KIND),
                "kind" => token = bound(&engine, CLOCK),
                "scope" => { let InterpreterValue::Capability(cap) = &mut token else { unreachable!() }; cap.scope = "other".into(); }
                _ => {},
            }
            let mut values = BTreeMap::from([(CAP, token), (RESULT, InterpreterValue::Int(777))]);
            let path: &[u8] = match failure { "utf8" => &[255], "escape" => b"../source", _ => b"source" };
            let first = buffer(&mut engine, layout, &mut values, path, false);
            let mut arguments = vec![HostCallArgument::Capability(CAP), first];
            if name != "dir_set_executable_at" { arguments.push(buffer(&mut engine, layout, &mut values, b"target", false)); }
            let last = arguments.last_mut().unwrap();
            if failure == "write-slice" {
                let original = place(last);
                *last = HostCallArgument::Buffer { place: Place::projected(original.object(), original.projection().clone(),
                    original.value_type(), AccessMode::Write), extent: HostBufferExtent::Slice };
            } else if failure == "uninitialized" {
                let (object, _, _) = engine.slice_place(place(last), &values).unwrap();
                engine.objects.get_mut(&object).unwrap().cells.remove(&CellKey::Element(0));
            } else if failure == "bounds" {
                let Projection::Slice { length, .. } = place(last).projection() else { unreachable!() };
                values.insert(*length, InterpreterValue::Size(999));
            }
            let results = if failure == "arity" { vec![RESULT] } else { vec![] };
            assert!(engine.dir_storage_host_call(name, &results, &arguments, &mut values).is_err(), "{name}/{failure}");
            assert_eq!(engine.config.fixture, before, "{name}/{failure}");
            assert_eq!(values[&RESULT], InterpreterValue::Int(777));
        }
    }
}

#[test]
fn dir_installer_oracle_isolated_and_server_effect_channels_refuse() {
    let (module, _) = module(vec![]);
    for isolated in [true, false] {
        let mut cfg = config();
        if !isolated { cfg.fixture.server_pool = Some(OracleServerPoolFixture::default()); }
        let mut engine = Engine::new(module.module(), cfg); engine.isolated = isolated;
        for name in INSTALLER_IMPORTS {
            let error = engine.dir_storage_host_call(name, &[], &[], &mut BTreeMap::new()).unwrap_err();
            assert_eq!(error, if isolated { InterpretError::UnsupportedHostImport }
                else { InterpretError::UnsupportedOperation("server pool directory storage effect ordering") });
        }
    }
}

#[test]
fn dir_installer_oracle_virtual_links_resolve_without_backing_and_stay_confined() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.root = None;
    engine.config.fixture.directories.extend(["scope/tree/dir", "outside"].map(String::from));
    engine.config.fixture.dir_mutations.links.extend([
        ("scope/tree/link".into(), std::path::PathBuf::from("dir")),
        ("scope/tree/escape".into(), std::path::PathBuf::from("../../outside")),
        ("scope/tree/absolute".into(), std::path::PathBuf::from("/outside")),
        ("scope/tree/cycle".into(), std::path::PathBuf::from("cycle")),
        ("scope/tree/missing".into(), std::path::PathBuf::from("absent")),
    ]);
    let InterpreterValue::Capability(root) = bound(&engine, DIR_KIND) else { panic!("root") };
    let narrowed = engine.registry.narrow_dir(&root, "scope".into()).unwrap();
    let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(narrowed))]);
    assert!(engine.config.fixture.metadata_is_directory("scope", "tree/link"));
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"tree", b"moved").unwrap();
    assert!(engine.config.fixture.metadata_is_directory("scope", "moved/link"));
    assert!(!engine.config.fixture.metadata_is_directory("scope", "tree/link"));
    for leaf in ["escape", "absolute", "cycle", "missing"] {
        assert!(!engine.config.fixture.metadata_is_directory("scope", &format!("moved/{leaf}")), "{leaf}");
    }
    // Chmod and append never follow even an in-grant leaf link. Rename moves
    // the link itself, so a dangling or escaping target grants no authority.
    for name in ["dir_append_bytes_at", "dir_set_executable_at"] {
        let before = engine.config.fixture.clone();
        assert!(invoke(&mut engine, layout, &mut values, name, b"moved/link", b"data").is_err());
        assert_eq!(engine.config.fixture, before);
    }
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"moved/escape", b"escape").unwrap();
    assert_eq!(engine.config.fixture.dir_mutations.links["scope/escape"], std::path::PathBuf::from("../../outside"));
    assert!(!engine.config.fixture.metadata_is_directory("scope", "escape"));
}

fn open_subtree(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>, path: &[u8]) -> InterpreterValue {
    let path = buffer(engine, layout, values, path, false);
    engine.capability_host_call("dir_subtree_at", &[RESULT], &[HostCallArgument::Capability(CAP), path], values).unwrap();
    values[&RESULT].clone()
}

fn read_open_file(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>, file: ValueId) -> Result<Vec<u8>, InterpretError> {
    let output = buffer(engine, layout, values, &[b'?'; 64], true);
    engine.capability_host_call("file_read_into", &[RESULT], &[HostCallArgument::Capability(file), output.clone()], values)?;
    let InterpreterValue::Size(length) = values[&RESULT] else { panic!("length") };
    Ok(bytes(engine, &output, values)[..length as usize].to_vec())
}

#[test]
fn dir_installer_oracle_retained_directory_and_file_parent_identity() {
    const FILE: ValueId = ValueId::new(9100);
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.files.insert("old/note".into(), b"old".to_vec());
    engine.config.fixture.files.insert("old/nested/child".into(), b"N".to_vec());
    let root = bound(&engine, DIR_KIND);
    let mut values = BTreeMap::from([(CAP, root.clone())]);
    let retained = open_subtree(&mut engine, layout, &mut values, b"old");
    let nested = open_subtree(&mut engine, layout, &mut values, b"old/nested");
    let file_path = buffer(&mut engine, layout, &mut values, b"old/note", false);
    engine.capability_host_call("dir_read_file", &[FILE], &[HostCallArgument::Capability(CAP), file_path], &mut values).unwrap();
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"old", b"new").unwrap();
    let old = buffer(&mut engine, layout, &mut values, b"old", false);
    engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), old], &mut values).unwrap();
    invoke(&mut engine, layout, &mut values, "dir_append_bytes_at", b"old/note", b"replacement").unwrap();
    let replacement = open_subtree(&mut engine, layout, &mut values, b"old");
    assert_ne!(retained, replacement, "same display path must not identify the replacement directory");

    values.insert(CAP, retained.clone());
    invoke(&mut engine, layout, &mut values, "dir_append_bytes_at", b"note", b"!").unwrap();
    let child = buffer(&mut engine, layout, &mut values, b"made", false);
    engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), child], &mut values).unwrap();
    let _ = open_subtree(&mut engine, layout, &mut values, b"nested");
    let output = buffer(&mut engine, layout, &mut values, &[0; 64], true);
    engine.capability_host_call("dir_list_into", &[RESULT], &[HostCallArgument::Capability(CAP), output.clone()], &mut values).unwrap();
    let InterpreterValue::Size(length) = values[&RESULT] else { panic!("list length") };
    assert_eq!(&bytes(&engine, &output, &values)[..length as usize], b"made\0nested\0note");
    values.insert(CAP, nested);
    invoke(&mut engine, layout, &mut values, "dir_append_bytes_at", b"child", b"+").unwrap();
    assert_eq!(engine.config.fixture.file("new/note"), Some(b"old!".to_vec()));
    assert_eq!(engine.config.fixture.file("new/nested/child"), Some(b"N+".to_vec()));
    assert_eq!(engine.config.fixture.file("old/note"), Some(b"replacement".to_vec()));
    assert!(!engine.config.fixture.exists("old/made"));
    assert_eq!(read_open_file(&mut engine, layout, &mut values, FILE).unwrap(), b"old!");

    // ConfinedFile keeps a parent and leaf, not an open file inode: changing
    // the leaf later changes its result without changing its retained parent.
    values.insert(CAP, root);
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"new/note", b"new/gone").unwrap();
    assert!(read_open_file(&mut engine, layout, &mut values, FILE).is_err());
    invoke(&mut engine, layout, &mut values, "dir_append_bytes_at", b"new/note", b"new-leaf").unwrap();
    assert_eq!(read_open_file(&mut engine, layout, &mut values, FILE).unwrap(), b"new-leaf");
    // A second relocation reaches cloned capabilities too, not their display path.
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"new", b"again").unwrap();
    values.insert(CAP, retained);
    invoke(&mut engine, layout, &mut values, "dir_append_bytes_at", b"note", b"+").unwrap();
    assert_eq!(engine.config.fixture.file("again/note"), Some(b"new-leaf+".to_vec()));
}

#[test]
fn dir_installer_oracle_displaced_directory_handles_never_redirect() {
    let (module, layout) = module(vec![]);
    let mut engine = Engine::new(module.module(), config());
    engine.config.fixture.files.insert("source/private".into(), b"source-only".to_vec());
    engine.config.fixture.directories.insert("destination".into());
    let root = bound(&engine, DIR_KIND);
    let mut values = BTreeMap::from([(CAP, root.clone())]);
    let displaced = open_subtree(&mut engine, layout, &mut values, b"destination");
    invoke(&mut engine, layout, &mut values, "dir_rename_at", b"source", b"destination").unwrap();
    let incoming = open_subtree(&mut engine, layout, &mut values, b"destination");
    assert_ne!(displaced, incoming);
    values.insert(CAP, displaced.clone());
    for (path, present) in [(b"".as_slice(), true), (b"private".as_slice(), false)] {
        for name in ["dir_exists_at", "dir_is_dir_at"] {
            let input = buffer(&mut engine, layout, &mut values, path, false);
            engine.capability_host_call(name, &[RESULT], &[HostCallArgument::Capability(CAP), input], &mut values).unwrap();
            assert_eq!(values[&RESULT], if name == "dir_exists_at" { InterpreterValue::Size(u64::from(present)) }
                else { InterpreterValue::Bool(present) });
        }
    }
    assert_eq!(open_subtree(&mut engine, layout, &mut values, b""), displaced);
    let output = buffer(&mut engine, layout, &mut values, &[b'!'; 16], true);
    engine.capability_host_call("dir_list_into", &[RESULT], &[HostCallArgument::Capability(CAP), output.clone()], &mut values).unwrap();
    assert_eq!(values[&RESULT], InterpreterValue::Size(0));
    assert_eq!(bytes(&engine, &output, &values), [b'!'; 16]);
    let before = engine.config.fixture.clone();
    for name in ["dir_append_bytes_at", "dir_set_executable_at", "dir_rename_at"] {
        assert!(invoke(&mut engine, layout, &mut values, name, b"private", b"stolen").is_err());
        assert_eq!(engine.config.fixture, before);
    }
    let child = buffer(&mut engine, layout, &mut values, b"child", false);
    assert!(engine.registry_host_call("dir_make_dir_at", &[], &[HostCallArgument::Capability(CAP), child], &mut values).is_err());
    values.insert(CAP, root);
    assert_eq!(engine.config.fixture.file("destination/private"), Some(b"source-only".to_vec()));
}

#[test]
fn dir_installer_oracle_mkdir_rejects_unbound_root_and_legacy_subtree() {
    let (module, layout) = module(vec![]);
    for narrowed in [false, true] {
        let mut engine = Engine::new(module.module(), config());
        engine.config.fixture.directories.insert("scope".into());
        let before = engine.config.fixture.clone();
        let root = CapabilityToken::root(DIR_KIND);
        let token = if narrowed { engine.registry.narrow_dir(&root, "scope".into()).unwrap() } else { root };
        let mut values = BTreeMap::from([(CAP, InterpreterValue::Capability(token))]);
        let path = buffer(&mut engine, layout, &mut values, b"brand-new", false);
        assert_eq!(engine.registry_host_call("dir_make_dir_at", &[],
            &[HostCallArgument::Capability(CAP), path.clone()], &mut values),
            Err(InterpretError::RegistryFixture("registry unauthenticated resource".into())));
        assert_eq!(engine.config.fixture, before);
        // The same auth failure precedes malformed guest transport as well.
        let (object, _, _) = engine.slice_place(place(&path), &values).unwrap();
        engine.objects.get_mut(&object).unwrap().cells.remove(&CellKey::Element(0));
        assert_eq!(engine.registry_host_call("dir_make_dir_at", &[],
            &[HostCallArgument::Capability(CAP), path], &mut values),
            Err(InterpretError::RegistryFixture("registry unauthenticated resource".into())));
        assert_eq!(engine.config.fixture, before);
    }
}
