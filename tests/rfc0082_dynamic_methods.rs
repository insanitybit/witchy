//! RFC-0082 dynamic method parity through the authenticated production pipeline.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    let module = parser::parse_module(source).expect("parse dynamic method parity fixture");
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/dynamic-method-parity",
        "0.1.0",
    )
    .expect("workspace package coordinate");
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .expect("toolchain package coordinate");
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).expect("main module owner"),
    )];
    assignments.extend(witchy::linker::STD_MODULES.iter().map(|std_module| {
        (
            (*std_module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                .expect("std module owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated module owners");
    pipeline::link_checked_authenticated(vec![("main".to_string(), module)], "main", owners)
        .expect("authenticated checked link")
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
    let checked = checked(source);
    let interpreted = interpreter::run_checked_module(checked.clone(), ".", Vec::new())
        .expect("interpret authenticated dynamic method fixture");
    let expected = [
        "value-12",
        "missing-missing",
        "arity-bump-1-0",
        "argument-0-Int-String",
    ];
    assert_eq!(interpreted, expected);

    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile authenticated dynamic method fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
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
    let checked = checked(source);
    let interpreted = interpreter::run_checked_module(checked.clone(), ".", Vec::new())
        .expect("interpret explicit capability fixture");
    let expected = ["denied-announce", "cap-ok", "called"];
    assert_eq!(interpreted, expected);

    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile explicit capability fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
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

#[test]
fn dynamic_trait_queries_match_between_interpreter_and_wasm() {
    let source = r#"
import dynamic
import reflect

trait Label:
    fn label(self) -> String

trait Missing:
    fn missing(self) -> String

type Widget:
    value: Int

impl Reflect for Widget:
    fn reflect(self) -> reflect.Mirror:
        reflect.MNil

impl Label for Widget:
    fn label(self) -> String:
        "widget-${self.value}"

fn main(console: Console):
    let packed = dynamic.dynamic(Widget(9))
    console.print("label-${dynamic.implements(packed, dynamic.runtime_type(dyn Label))}")
    console.print("missing-${dynamic.implements(packed, dynamic.runtime_type(dyn Missing))}")
    match dynamic.as_trait(packed, dynamic.runtime_type(dyn Label)):
        Ok(view) ->
            let decoded: Option(Widget) = dynamic.try_decode(view)
            match decoded:
                Some(widget) -> console.print("view-${widget.value}")
                None -> console.print("decode-failed")
        Err(_) -> console.print("unexpected-view-error")
    match dynamic.as_trait(packed, dynamic.runtime_type(dyn Missing)):
        Err(dynamic.TraitMismatch(trait_type)) ->
            console.print("mismatch-${dynamic.type_name(trait_type)}")
        _ -> console.print("unexpected-missing-view")
"#;
    let checked = checked(source);
    let interpreted = interpreter::run_checked_module(checked.clone(), ".", Vec::new())
        .expect("interpret authenticated dynamic trait fixture");
    let expected = ["label-true", "missing-false", "view-9", "mismatch-dyn Missing"];
    assert_eq!(interpreted, expected);

    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile authenticated dynamic trait fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled dynamic trait fixture");
    actor.run().expect("run compiled dynamic trait fixture");
    assert_eq!(actor.output(), expected);
}
