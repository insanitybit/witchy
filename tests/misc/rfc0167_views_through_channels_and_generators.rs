//! RFC-0167 row K channel boundary. A live `must` child handle cannot enter
//! the replayable Task continuation installed by `await`.

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { witchy::opt::set_for_tests(None); }
}

const CHANNELS: &str = r#"
from chan import Sender

async fn producer(tx: Sender(&Int), v: &Int):
    chan.send(tx, v).await

async fn main(console: Console):
    var n = 21
    let (tx, rx) = chan.channel(1).await
    let child = chan.spawn(producer(tx, &n)).await
    n = 5
    let received: Option(&Int) = chan.recv(rx).await
    match received:
        Some(value) -> console.print("${*value} ${n}")
        None -> console.print("unexpected closed")
    chan.join(child).await
"#;

const COPYABLE_FIELD_IN_AFFINE_SHELL: &str = r#"
must type Ticket:
    Ticket(Int)

must type Packet:
    Packet(&String, Ticket)

fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n

fn consume(own packet: Packet) -> Int:
    match packet:
        Packet(view, ticket) ->
            finish(move ticket) + (*view).length()

fn main() -> Int:
    let text = "abc"
    let view = &text
    let packet = Packet(view, Ticket(7))
    consume(packet)
"#;

const OWNED_GC_RESULT: &str = r#"
must type BoxView:
    BoxView(&Int)

fn make() -> BoxView:
    let n = 3
    BoxView(&n)

fn consume(own box: BoxView) -> Int:
    match box:
        BoxView(view) -> *view

fn main() -> Int:
    consume(make())
"#;

const OWNED_GC_AFFINE_FIELD: &str = r#"
must type Token:
    Token(Int)

must type Cell:
    Cell(&Int, Token)

fn make() -> Cell:
    let n = 3
    Cell(&n, Token(7))

fn consume(own cell: Cell) -> Int:
    match cell:
        Cell(view, token) ->
            match move token:
                Token(n) -> n + *view

fn consume_local() -> Int:
    let cell = make()
    match move cell:
        Cell(view, token) ->
            match move token:
                Token(n) -> n + *view

fn main() -> Int:
    consume(make()) + consume_local()
"#;

const OWNED_GC_BRANCH_JOIN: &str = r#"
must type BoxView:
    BoxView(&Int)

fn make(n: Int) -> BoxView:
    let value = n
    BoxView(&value)

fn choose(flag: Bool) -> BoxView:
    if flag:
        make(3)
    else:
        make(4)

fn consume(own box: BoxView) -> Int:
    match box:
        BoxView(view) -> *view

fn main() -> Int:
    let box = if true:
        make(3)
    else:
        make(4)
    let matched = match false:
        false -> make(5)
        _ -> make(6)
    consume(box) + consume(matched) + consume(choose(false))
"#;

const OWNED_GC_BINDING_JOIN: &str = r#"
must type BoxView:
    BoxView(&Int)

fn make(n: Int) -> BoxView:
    let value = n
    BoxView(&value)

fn consume(own box: BoxView) -> Int:
    match box:
        BoxView(view) -> *view

fn main() -> Int:
    var box = make(1)
    if true:
        let previous = consume(move box)
        box = make(previous + 2)
    else:
        let previous = consume(move box)
        box = make(previous + 3)
    let after_if = consume(move box)
    box = make(after_if)
    match false:
        false ->
            let previous = consume(move box)
            box = make(previous + 4)
        _ ->
            let previous = consume(move box)
            box = make(previous + 5)
    let after_match = consume(move box)
    box = make(after_match)
    for i in 0..2:
        let previous = consume(move box)
        box = make(previous + i)
    var step = 0
    while step < 2:
        let previous = consume(move box)
        box = make(previous + step)
        step = step + 1
    consume(move box)
"#;

#[test]
fn channel_handle_refusal_and_owned_gc_result() {
    let refusal = match witchy::resolve_std_only_checked(CHANNELS) {
        Err(error) => error,
        Ok(_) => panic!("live child handle must not enter a replayable continuation"),
    };
    // Source checking rejects the missing cancellation disposition before
    // lowering can put the live child into a generated continuation capture.
    assert!(refusal.to_string().contains("must-consume value `child`")
        && refusal.to_string().contains("cancellation"),
        "unexpected refusal: {refusal}");

    // A copyable view child gets its own retained edge when the aggregate
    // shell moves; the extracted affine child crosses an `own` call.
    let checked = witchy::resolve_std_only_checked(COPYABLE_FIELD_IN_AFFINE_SHELL)
        .expect("copyable and affine fields share one checked aggregate move plan");
    let _ = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("copyable view and affine field have distinct storage transfers");

    let checked = witchy::resolve_std_only_checked(OWNED_GC_RESULT)
        .expect("must GC aggregate has a checked owned result");
    let _ = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("owned GC result transfers a constructed owner");

    let checked = witchy::resolve_std_only_checked(OWNED_GC_AFFINE_FIELD)
        .expect("owned GC shell may transfer a nested affine field");
    let _ = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("consuming GC match extracts the affine field");

    let checked = witchy::resolve_std_only_checked(OWNED_GC_BRANCH_JOIN)
        .expect("owned GC result joins are checked by source type");
    let _ = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("owned GC branch join preserves its unique owner");

    let checked = witchy::resolve_std_only_checked(OWNED_GC_BINDING_JOIN)
        .expect("affine rebinding consumes each old owner");
    let _ = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("affine GC bindings cross conditional and loop joins");
}

#[test]
fn mixed_view_and_affine_packet_runs_without_losing_either_child() {
    use witchy::runtime::{Capabilities, Runtime};
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let body = r#"
must type Ticket:
    Ticket(Int)

must type Packet:
    Packet(&String, Ticket)

fn consume(own packet: Packet) -> Int:
    match packet:
        Packet(view, ticket) ->
            match move ticket:
                Ticket(n) -> n + (*view).length()

async fn main(console: Console):
    let text = "abc"
    let packet = Packet(&text, Ticket(7))
    console.print("${consume(packet)}")
"#;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{body}");
        let checked = witchy::resolve_std_only_checked(&source)
            .expect("mixed packet type-checks");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted unoptimized mixed packet Wasm");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted mixed packet launch");
        vm.run().expect("mixed packet execution");
        assert_eq!(vm.output(), vec!["10"], "mode {mode:?}");
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0), "mode {mode:?}");
    }
}

#[test]
fn normal_caller_transfers_opt_defined_mixed_packet_without_losing_children() {
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let worker = r#"mode opt
must type Ticket:
    Ticket(Int)

must type Packet:
    Packet(&String, Ticket)

must type Envelope:
    Envelope(Packet)

pub fn make(view: &String, n: Int) -> Packet:
    Packet(view, Ticket(n))

pub fn wrap(own packet: Packet) -> Envelope:
    Envelope(packet)

pub fn consume(own packet: Packet) -> Int:
    match packet:
        Packet(view, ticket) ->
            match move ticket:
                Ticket(n) -> n + (*view).length()

pub fn consume_wrapped(own envelope: Envelope) -> Int:
    match envelope:
        Envelope(packet) -> consume(packet)
"#;
    let main = r#"import worker

async fn main(console: Console):
    let text = "abc"
    let packet = worker.make(&text, 7)
    console.print("${worker.consume(packet)}")
    let nested = worker.wrap(worker.make(&text, 9))
    console.print("${worker.consume_wrapped(nested)}")
"#;
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "example/mixed-packet", "0.1.0")
        .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "0.1.0")
        .expect("toolchain coordinate");
    let mut assignments = ["main", "worker"].map(|name| (
        name.to_owned(), ModuleLoadIdentity::new(workspace.clone(), [name]).expect("user owner"),
    )).to_vec();
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).expect("std owner"),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners");
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("worker".into(), worker.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), owners,
    ).expect("normal caller and opt-defined packet type-check");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted unoptimized cross-mode mixed packet Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted mixed packet launch");
    vm.run().expect("cross-mode mixed packet execution");
    assert_eq!(vm.output(), vec!["10", "12"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
