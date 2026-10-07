use witchy::runtime::{Capabilities, Runtime};

#[test]
fn owner_bridge_metrics_are_opt_in_and_vm_local() {
    let source = r#"from chan import Receiver

async fn receive(console: Console, rx: Receiver(Int)):
    var sum = 0
    for await value in rx:
        sum = sum + value
    console.print("${sum}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.send(tx, 7).await
    receive(console, rx).await
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked fixture");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities { print: true, quiet: true, ..Capabilities::none() };
    let mut reference = runtime.spawn_admitted(&artifact, caps.clone(), 256).expect("reference VM");
    let mut measured = runtime.spawn_admitted(&artifact, caps.clone(), 256).expect("measured VM");
    let mut independent = runtime.spawn_admitted(&artifact, caps, 256).expect("independent VM");
    measured.enable_owner_bridge_metrics();
    independent.enable_owner_bridge_metrics();
    assert!(measured.owner_bridge_counts().unwrap().is_empty());
    reference.run().expect("reference execution");
    measured.run().expect("measured execution");
    assert_eq!(reference.output(), ["7"]);
    assert_eq!(reference.output(), measured.output());
    assert!(reference.owner_bridge_counts().is_none());
    assert!(independent.owner_bridge_counts().unwrap().is_empty());
    let expected = measured.owner_bridge_counts().unwrap().clone();
    assert!(expected.get("construct").copied().unwrap_or(0) > 0);
    assert!(expected.get("drop").copied().unwrap_or(0) > 0);
    independent.run().expect("independent execution");
    assert_eq!(independent.owner_bridge_counts().unwrap(), &expected);
    assert_eq!(measured.canonical_heap_leaks().expect("heap audit"), Some(0));
    assert_eq!(independent.canonical_heap_leaks().expect("heap audit"), Some(0));
}
