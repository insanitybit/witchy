//! Source-bound named adapters inside nested opaque closure families.

#[test]
fn server_workers_reject_raw_wasm_even_with_genuine_entry_metadata() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    let checked = witchy::resolve_std_only_checked(r#"
fn main(net: Net):
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
"#).unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("entry metadata fixture");
    assert!(artifact.owner_bridges().is_empty());
    assert_eq!(witchy_wir::canonical_entry_from_wasm(artifact.wasm()).unwrap().as_ref(), artifact.entry());
    let mut runtime = Runtime::batch().unwrap();
    let error = match runtime.spawn(artifact.wasm(), Capabilities {
        net_allow: Some(vec!["127.0.0.1:0".into()]), net_listen: true, ..Default::default()
    }, 256) {
        Ok(_) => panic!("entry bytes cannot replace sealed admission authority"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("raw Wasm cannot claim canonical admission"));
}

#[path = "rfc0165_vm_service.rs"]
mod vm_service;
