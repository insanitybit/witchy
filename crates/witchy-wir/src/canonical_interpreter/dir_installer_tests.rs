//! Independent fixture-state and checked-buffer probes; no native provider calls.
use super::*;

fn invoke(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>,
    name: &str, first: &[u8], second: &[u8]) -> Result<(), InterpretError>
{
    let first = buffer(engine, layout, values, first, false);
    let mut arguments = vec![HostCallArgument::Capability(CAP), first];
    if name != "dir_set_executable_at" { arguments.push(buffer(engine, layout, values, second, false)); }
    engine.dir_storage_host_call(name, &[], &arguments, values)
}


pub fn dir_installer_oracle_virtual_links_resolve_without_backing_and_stay_confined() {
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


