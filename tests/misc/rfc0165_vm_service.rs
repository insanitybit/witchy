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
fn vm_service_local_callback_and_captures_are_source_errors() {
    for callback in ["fn(state, request): state.concat(request)", "local"] {
        let source = SOURCE.replace("    let states", "    let local = accumulate\n    let states")
            .replace(", accumulate)", &format!(", {callback})"));
        assert!(witchy::resolve_std_only_checked(&source).is_err(), "{callback}");
    }
}

