use super::*;
use witchy_wir::canonical_safewir::{CandidateVmService, FunctionId};

fn checked() -> CheckedModule {
    let main = r#"
import vm
fn handler(state: Bytes, request: Bytes) -> Bytes:
    request
fn other(state: Bytes, request: Bytes) -> Bytes:
    state
fn run(init: Bytes, requests: List(Bytes)) -> List(Bytes):
    vm.serve(init, requests, handler)
fn main() -> Int:
    0
"#;
    use std::collections::{HashSet, VecDeque};
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    let mut sources = vec![("main".to_owned(), main.as_bytes().to_vec())];
    let mut loaded = HashSet::from(["main".to_owned()]);
    let mut pending = VecDeque::from([witchy_syntax::parser::parse_module(main).unwrap()]);
    while let Some(module) = pending.pop_front() {
        for name in module.imports {
            if !loaded.insert(name.clone()) { continue; }
            let bytes = witchy_syntax::linker::bundled_source(&name).expect("actual bundled std module");
            pending.push_back(witchy_syntax::parser::parse_module(bytes).unwrap());
            sources.push((name, bytes.as_bytes().to_vec()));
        }
    }
    let application = PackageCoordinate::new(PackageSource::Workspace, "witchy/service-test", "0.1.0").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "0.1.0").unwrap();
    let mut assignments = vec![("main".to_owned(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name|
        ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap())));
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources, "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        &HashSet::from(["main".to_owned()]), AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap(),
    ).expect("checked service source")
}

#[test]
fn vm_service_exact_bindings_validate_and_bad_descriptors_refuse() {
    let checked = checked();
    let source = checked.canonical_source().unwrap();
    let candidate = || lower_typed_module(source, derive_layout_bundle(source).unwrap()).unwrap()
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    let original = candidate();
    let descriptor = original.vm_services()[0].clone();
    let initial = check_initial_with_witness(original, source).expect("exact source site validates");
    validate_final(initial, source).expect("final service validation");
    let malformed = [vec![], vec![descriptor.clone(), descriptor.clone()],
        vec![CandidateVmService::new(descriptor.id() + 1, descriptor.caller(), descriptor.callback())],
        vec![CandidateVmService::new(descriptor.id(), descriptor.callback(), descriptor.callback())],
        vec![CandidateVmService::new(descriptor.id(), descriptor.caller(), FunctionId::new(u32::MAX))]];
    for descriptors in malformed {
        assert!(check_initial_with_witness(candidate().with_vm_services(descriptors), source).is_err());
    }
}
