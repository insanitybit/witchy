//! RFC-0082 closed-world dynamic method discovery and invocation.

use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn authenticated(source: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let module = witchy_syntax::parser::parse_module(source).map_err(|error| error.to_string())?;
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/dynamic-method-test",
        "0.1.0",
    )
    .map_err(|error| error.to_string())?;
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .map_err(|error| error.to_string())?;
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).map_err(|error| error.to_string())?,
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|std_module| {
        (
            (*std_module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                .expect("valid std module owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .map_err(|error| error.to_string())?;
    witchy_interp::pipeline::link_checked_authenticated(
        vec![("main".to_string(), module)],
        "main",
        owners,
    )
    .map_err(|error| error.to_string())
}

fn run(source: &str) -> Result<Vec<String>, String> {
    let checked = authenticated(source)?;
    witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .map_err(|error| error.to_string())
}

#[test]
fn opted_in_methods_are_enumerated_and_invoked_with_exact_descriptors() {
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
    let found = dynamic.methods(dynamic.runtime_type(Widget))
    console.print(dynamic.method_name(list.at(found, 0)))
    console.print("${list.length(dynamic.method_args(list.at(found, 0)))}")
    console.print(dynamic.type_name(dynamic.method_result(list.at(found, 0))))
    match dynamic.call(dynamic.dynamic(Widget(7)), "bump", [dynamic.dynamic(5)]):
        Ok(packed) ->
            let decoded: Option(Widget) = dynamic.try_decode(packed)
            match decoded:
                Some(widget) -> console.print("value-${widget.value}")
                None -> console.print("decode-failed")
        Err(_) -> console.print("call-failed")
"#;

    assert_eq!(
        run(source).expect("run dynamic method fixture"),
        ["bump", "1", "main.Widget", "value-12"],
    );
}

#[test]
fn dynamic_call_reports_closed_dispatch_failures_without_string_lookup() {
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

    assert_eq!(
        run(source).expect("run dynamic failure fixture"),
        ["missing-missing", "arity-bump-1-0", "argument-0-Int-String"],
    );
}

#[test]
fn invalid_dynamic_declarations_fail_closed() {
    let cases = [
        (
            "private",
            "@dynamic\nfn hidden(self: Int) -> Int:\n    self\n\nfn main():\n    ()\n",
            "must be public",
        ),
        (
            "generic",
            "@dynamic\npub fn identity(self: a) -> a:\n    self\n\nfn main():\n    ()\n",
            "closed non-generic signature",
        ),
        (
            "default",
            "@dynamic\npub fn add(self: Int, amount: Int = 1) -> Int:\n    self + amount\n\nfn main():\n    ()\n",
            "runtime arity is exact",
        ),
    ];

    for (name, source, expected) in cases {
        let error = authenticated(source).expect_err(name);
        assert!(error.contains(expected), "{name}: {error}");
    }
}

#[test]
fn capability_bearing_dynamic_signatures_are_rejected_before_dispatch_generation() {
    let source = "@dynamic\npub fn expose(self: Int, console: Console) -> Int:\n    console.print(\"no\")\n    self\n\nfn main():\n    ()\n";
    let checked = authenticated(source).expect("declaration checking succeeds before lowering");
    let error = witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .expect_err("capability-bearing dynamic method must not enter the runtime table")
        .to_string();
    assert!(error.contains("unsupported capability-bearing signature"), "{error}");
}
