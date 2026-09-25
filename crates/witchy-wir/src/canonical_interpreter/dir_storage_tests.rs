//! Frozen storage probes using the oracle's existing semantic heap harness.
//! Filesystem operations below only set up/tear down explicit fallback fixtures.
use super::*;

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



pub fn dir_storage_oracle_scoped_curdir_write_and_read_stay_under_grant() {
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


