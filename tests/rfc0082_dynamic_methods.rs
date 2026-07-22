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
