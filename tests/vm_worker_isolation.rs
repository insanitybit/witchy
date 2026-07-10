fn check(source: &str) -> Result<(), String> {
    let linked = witchy::resolve_std_only(source)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
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
        assert!(
            error.contains(api)
                && error.contains("bare top-level function")
                && error.contains("isolated worker-VM boundary"),
            "unexpected diagnostic for {api}: {error}"
        );
    }
}

#[test]
fn isolated_worker_apis_accept_bare_top_level_callbacks() {
    let source = r#"
import vm

fn read_worker(_dir: Dir, input: Bytes) -> Bytes:
    input

fn service_worker(state: Bytes, _request: Bytes) -> Bytes:
    state

fn invoke(dir: Dir, input: Bytes, requests: List(Bytes)) -> List(Bytes):
    let first = vm.with_dir(dir, read_worker, input)
    vm.serve(first, requests, service_worker)

fn main(console: Console):
    console.print("ok")
"#;

    check(source).expect("bare top-level callbacks preserve the worker boundary");
}

#[test]
fn par_map_keeps_its_explicit_sequential_fallback() {
    let source = r#"
import vm

fn double(value: Int) -> Int:
    value * 2

fn map_indirect(values: List(Int)) -> List(Int):
    let callback = double
    let doubled = vm.par_map(values, callback)
    vm.par_map(doubled, fn(value: Int): value + 1)

fn main(console: Console):
    console.print("ok")
"#;

    check(source).expect("par_map may run unsupported parallel shapes sequentially");
}
