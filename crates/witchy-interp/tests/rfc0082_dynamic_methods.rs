//! RFC-0082 closed-world dynamic method discovery and invocation.

#[path = "../../../tests/support/authenticated.rs"]
mod authenticated;
use authenticated::checked_result;

fn run(source: &str) -> Result<Vec<String>, String> {
    let checked = checked_result(source).map_err(|error| error.to_string())?;
    witchy_interp::interpreter::run_checked_module(&checked, ".", Vec::new())
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
        let error = checked_result(source).expect_err(name).to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
}

#[test]
fn capability_methods_require_and_retain_an_explicit_static_bundle() {
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
    let method = list.at(dynamic.methods(dynamic.runtime_type(Widget)), 0)
    console.print(list.at(dynamic.method_capabilities(method), 0))
    match dynamic.call(dynamic.dynamic(Widget(1)), "announce", [dynamic.dynamic("missing")]):
        Err(dynamic.CapabilityDenied(name)) -> console.print("denied-${name}")
        _ -> console.print("unexpected-implicit")
    match dynamic.call_with(dynamic.dynamic(Widget(1)), "announce", [dynamic.dynamic("wrong")], "not-authority"):
        Err(dynamic.CapabilityDenied(name)) -> console.print("wrong-${name}")
        _ -> console.print("unexpected-wrong")
    match dynamic.call_with(dynamic.dynamic(Widget(1)), "announce", [dynamic.dynamic("ok")], console):
        Ok(_) -> console.print("called")
        Err(_) -> console.print("call-failed")
"#;

    assert_eq!(
        run(source).expect("run capability-aware dynamic method fixture"),
        ["Console", "denied-announce", "wrong-announce", "cap-ok", "called"],
    );
}

#[test]
fn multiple_capabilities_use_an_exact_ordered_tuple() {
    let source = r#"
import dynamic
import reflect

type Widget:
    value: Int

impl Reflect for Widget:
    fn reflect(self) -> reflect.Mirror:
        reflect.MNil

@dynamic
pub fn report(self: Widget, console: Console, clock: Clock, amount: Int) -> Widget:
    console.print("report-${amount}")
    self

fn main(console: Console, clock: Clock):
    let method = list.at(dynamic.methods(dynamic.runtime_type(Widget)), 0)
    console.print(list.join(dynamic.method_capabilities(method), ","))
    match dynamic.call_with(dynamic.dynamic(Widget(1)), "report", [dynamic.dynamic(7)], (console, clock)):
        Ok(_) -> console.print("tuple-called")
        Err(_) -> console.print("tuple-failed")
"#;

    assert_eq!(
        run(source).expect("run multi-capability dynamic method fixture"),
        ["Console,Clock", "report-7", "tuple-called"],
    );
}

#[test]
fn trait_queries_use_authenticated_closed_impl_membership() {
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
    let packed = dynamic.dynamic(Widget(7))
    console.print("label-${dynamic.implements(packed, dynamic.runtime_type(dyn Label))}")
    console.print("missing-${dynamic.implements(packed, dynamic.runtime_type(dyn Missing))}")
    console.print("not-trait-${dynamic.implements(packed, dynamic.runtime_type(Int))}")
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

    assert_eq!(
        run(source).expect("run authenticated dynamic trait fixture"),
        ["label-true", "missing-false", "not-trait-false", "view-7", "mismatch-dyn Missing"],
    );
}
