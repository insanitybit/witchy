//! Source/admission evidence; isolated native execution is paired with the host track.
const SOURCE: &str = r#"
import vm
import bytes
fn accumulate(state: Bytes, request: Bytes) -> Bytes:
    state.concat(request)
fn main(console: Console):
    let states = vm.serve(bytes.from_string(""), [bytes.from_string("a"), bytes.from_string("b")], accumulate)
    for state in states:
        console.print(bytes.to_string(state))
"#;

#[test]
fn vm_service_source_sites_reach_sealed_artifact() {
    let checked = witchy::resolve_std_only_checked(SOURCE).unwrap();
    let sites = witchy_types::canonical_worker::service_sites(checked.canonical_source().unwrap()).unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].id(), 0);
    assert_eq!(sites[0].occurrence(), 0);
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new()).unwrap(), ["a", "ab"]);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("isolated service core admission");
    assert_eq!(artifact.vm_services().len(), 1);
    assert_eq!(artifact.vm_services()[0].id(), sites[0].id());
    assert!(artifact.vm_services()[0].callback_export().starts_with('f'));
    let imports = wasmparser::Parser::new(0).parse_all(artifact.wasm()).filter_map(|p| match p.unwrap() {
        wasmparser::Payload::ImportSection(reader) => Some(reader.into_imports().map(|i| i.unwrap().name.to_owned()).collect::<Vec<_>>()),
        _ => None,
    }).flatten().collect::<Vec<_>>();
    for name in witchy_wir::canonical_safewir::VM_SERVICE_IMPORT_NAMES { assert!(imports.iter().any(|i| i == name)); }
    assert!(!imports.iter().any(|i| i == "vm_serve_run"));
}

#[test]
fn vm_service_local_callback_and_captures_are_source_errors() {
    for callback in ["fn(state, request): state.concat(request)", "local"] {
        let source = SOURCE.replace("    let states", "    let local = accumulate\n    let states")
            .replace(", accumulate)", &format!(", {callback})"));
        assert!(witchy::resolve_std_only_checked(&source).is_err(), "{callback}");
    }
}

#[test]
fn vm_service_own_callback_reaches_admission() {
    let source = SOURCE.replace("fn accumulate(state: Bytes, request: Bytes)", "fn accumulate(own state: Bytes, own request: Bytes)");
    let checked = witchy::resolve_std_only_checked(&source).expect("own callback source");
    witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("own service callback ABI");
}
