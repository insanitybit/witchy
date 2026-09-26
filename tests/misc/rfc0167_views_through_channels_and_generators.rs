//! RFC-0167 row K channel boundary. A live `must` child handle cannot enter
//! the replayable Task continuation installed by `await`.

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

#[test]
fn channel_handle_refusal_and_owned_gc_result() {
    let refusal = match witchy::resolve_std_only_checked(CHANNELS) {
        Err(error) => error,
        Ok(_) => panic!("live child handle must not enter a replayable continuation"),
    };
    assert!(refusal.to_string().contains("closure") && refusal.to_string().contains("capture"),
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
}
