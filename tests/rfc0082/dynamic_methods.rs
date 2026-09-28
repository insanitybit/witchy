//! RFC-0082 dynamic method parity through the authenticated production pipeline.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen};
use witchy_interp::{interpreter};
use witchy::opt::{self, OptSet};

use super::authenticated::checked_source_result;

fn checked_source(source: &str) -> witchy_types::pipeline::CheckedModule {
    checked_source_result(source).expect("authenticated exact-source link")
}

#[test]
fn dynamic_method_dispatch_matches_between_interpreter_and_wasm() {
    let source = r#"
import dynamic
import reflect

type Widget:
    value: Int

impl Reflect for Widget:
    fn reflect(self) -> reflect.Mirror:
        reflect.MNil

@dynamic
pub fn bump(self: Widget, amount: Int) -> Widget:
    Widget(self.value + amount)

fn main(console: Console):
    match dynamic.call(dynamic.dynamic(Widget(7)), "bump", [dynamic.dynamic(5)]):
        Ok(packed) ->
            let decoded: Option(Widget) = dynamic.try_decode(packed)
            match decoded:
                Some(widget) -> console.print("value-${widget.value}")
                None -> console.print("decode-failed")
        Err(_) -> console.print("call-failed")
    match dynamic.call(dynamic.dynamic(Widget(1)), "missing", []):
        Err(dynamic.MissingMethod(name)) -> console.print("missing-${name}")
        _ -> console.print("unexpected-missing")
    match dynamic.call(dynamic.dynamic(Widget(1)), "bump", []):
        Err(dynamic.ArityMismatch(name, expected, actual)) ->
            console.print("arity-${name}-${expected}-${actual}")
        _ -> console.print("unexpected-arity")
    match dynamic.call(dynamic.dynamic(Widget(1)), "bump", [dynamic.dynamic("wrong")]):
        Err(dynamic.ArgumentMismatch(index, expected, actual)) ->
            console.print("argument-${index}-${dynamic.type_name(expected)}-${dynamic.type_name(actual)}")
        _ -> console.print("unexpected-argument")
"#;
    let checked = checked_source(source);
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret authenticated dynamic method fixture");
    let expected = [
        "value-12",
        "missing-missing",
        "arity-bump-1-0",
        "argument-0-Int-String",
    ];
    assert_eq!(interpreted, expected);

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit authenticated dynamic method fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled dynamic method fixture");
    actor.run().expect("run compiled dynamic method fixture");
    assert_eq!(actor.output(), expected);
}

#[test]
fn explicit_dynamic_method_capabilities_match_between_interpreter_and_wasm() {
    let source = r#"
import dynamic
import reflect

type Widget:
    value: Int

impl Reflect for Widget:
    fn reflect(self) -> reflect.Mirror:
        reflect.MNil

@dynamic
pub fn announce(self: Widget, console: Console, label: String) -> Widget:
    console.print("cap-${label}")
    self

fn main(console: Console):
    match dynamic.call(dynamic.dynamic(Widget(1)), "announce", [dynamic.dynamic("missing")]):
        Err(dynamic.CapabilityDenied(name)) -> console.print("denied-${name}")
        _ -> console.print("unexpected-implicit")
    match dynamic.call_with(dynamic.dynamic(Widget(1)), "announce", [dynamic.dynamic("ok")], console):
        Ok(_) -> console.print("called")
        Err(_) -> console.print("call-failed")
"#;
    let checked = checked_source(source);
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret explicit capability fixture");
    let expected = ["denied-announce", "cap-ok", "called"];
    assert_eq!(interpreted, expected);

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit explicit capability fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled explicit capability fixture");
    actor.run().expect("run compiled explicit capability fixture");
    assert_eq!(actor.output(), expected);
}

#[test]
fn witchy_caps_reports_conservative_dynamic_method_authority() {
    let source = r#"
import reflect

type Widget:
    value: Int

impl Reflect for Widget:
    fn reflect(self) -> reflect.Mirror:
        reflect.MNil

@dynamic
pub fn fetch(self: Widget, net: Net[Connect, Tcp]) -> Widget:
    self

fn main():
    ()
"#;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "witchy-rfc0082-dynamic-caps-{}-{nonce}.witchy",
        std::process::id(),
    ));
    std::fs::write(&path, source).expect("write dynamic caps fixture");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("caps")
        .arg(&path)
        .output()
        .expect("run witchy caps");
    std::fs::remove_file(&path).expect("remove dynamic caps fixture");
    assert!(
        output.status.success(),
        "witchy caps failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fetch"), "{stdout}");
    assert!(stdout.contains("Net[Connect, Tcp]"), "{stdout}");
}
