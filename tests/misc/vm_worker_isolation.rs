//! Worker callback isolation and explicit sequential-fallback proofs.

fn check(source: &str) -> Result<(), String> {
    witchy::resolve_std_only_checked(source)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn isolated_worker_apis_reject_indirect_callbacks() {
    let cases = [
        (
            "vm.with_dir",
            r#"
import vm

fn worker(dir: Dir, input: Bytes) -> Bytes:
    input

fn invoke(dir: Dir, input: Bytes) -> Bytes:
    let callback = worker
    vm.with_dir(dir, callback, input)

fn main(console: Console):
    console.print("ok")
"#,
        ),
        (
            "vm.with_dir",
            r#"
import vm

fn invoke(dir: Dir, input: Bytes) -> Bytes:
    vm.with_dir(dir, fn(_dir: Dir, bytes: Bytes): bytes, input)

fn main(console: Console):
    console.print("ok")
"#,
        ),
        (
            "vm.serve",
            r#"
import vm

fn worker(state: Bytes, _request: Bytes) -> Bytes:
    state

fn invoke(init: Bytes, requests: List(Bytes)) -> List(Bytes):
    let callback = worker
    vm.serve(init, requests, callback)

fn main(console: Console):
    console.print("ok")
"#,
        ),
        (
            "vm.serve",
            r#"
import vm

fn invoke(init: Bytes, requests: List(Bytes)) -> List(Bytes):
    vm.serve(init, requests, fn(state: Bytes, _request: Bytes): state)

fn main(console: Console):
    console.print("ok")
"#,
        ),
    ];

    for (api, source) in cases {
        let error = check(source).expect_err("an indirect callback must not weaken isolation");
        let rejected_at_capability_function_binding = api == "vm.with_dir"
            && error.contains("Dir")
            && error.contains("function value")
            && error.contains("typed closure/GC lowering");
        assert!(
            rejected_at_capability_function_binding
                || (error.contains(api)
                    && error.contains("bare top-level function")
                    && error.contains("isolated worker-VM boundary")),
            "unexpected diagnostic for {api}: {error}"
        );
    }
}

