use crate::{codegen, interpreter, parser, typeck};

    // Phase 2 of the concurrency redesign: an `async fn` lowers (CPS over closures,
    // `crate::async_lower`) to a cooperative `chan` task, and `await` chains
    // continuations. An async `main` is the executor entry (lowers to `task.run`).
    // The lowering is ordinary closures + calls, so both backends agree.
    #[test]
    fn async_await_lowers_and_runs_backends_agree() {
        let src = r#"
async fn double(n: Int) -> Int:
    n + n

async fn pipeline(seed: Int) -> Int:
    let a = double(seed).await
    let b = double(a).await
    a + b

async fn main(console: Console):
    let r = pipeline(3).await
    console.print("${r}")
    let d = double(10).await
    console.print("${d}")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "async lowering diverged across backends");
        // pipeline(3): a=6, b=12, a+b=18.  double(10)=20.
        assert_eq!(interp_out, vec!["18", "20"]);
    }

    // (RFC-0059 Stage-1 step 1) The state-machine lowering's expressiveness: a
    // mutable `var` local crosses an `await` (mutated on both sides), an `await`
    // appears inside a `while` loop, and a `for await` body FOLDS into an outer
    // accumulator (not just drains). All three the old CPS lowering rejected; here
    // both backends agree AND the folded result is correct.
    #[test]
    fn async_state_machine_expressiveness_backends_agree() {
        let src = r#"
import chan
from chan import Sender, Receiver

fn bump(var n: Int) -> Int:
    n = n + 1
    n

async fn counter(tx: Sender(Int), n: Int) -> Nil:
    var i = 0
    while i < n:
        chan.send(tx, i).await
        i = i + 1

async fn total(console: Console, rx: Receiver(Int)) -> Nil:
    var sum = 0
    for await v in rx:
        sum = sum + v
    console.print("sum ${sum}")

async fn var_across(console: Console) -> Nil:
    var acc = 10
    let first = bump(acc)
    chan.yield_now().await
    bump(acc)
    acc = acc + 100
    chan.yield_now().await
    console.print("acc ${acc} first ${first}")

async fn main(console: Console):
    var_across(console).await
    let (tx, rx) = chan.channel(4).await
    chan.spawn(counter(tx, 5)).await
    total(console, rx).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "state-machine async lowering diverged across backends");
        // The synchronous value-returning `var` calls update a local threaded
        // through the shipped async segment functions on both sides of `await`.
        assert_eq!(interp_out, vec!["acc 112 first 11", "sum 10"]);
    }

    // `await` inside a `for` loop — over a list (producer) and a range (consumer)
    // — lowers to a sequential `task.for_each`, so iterating with `await` needs no
    // hand-written recursion. Both backends must agree, byte-for-byte.
    #[test]
    fn for_await_loop_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn producer(tx: Sender(Int)) -> Nil:
    for x in [1, 2, 3]:
        chan.send(tx, x).await

async fn consumer(console: Console, rx: Receiver(Int)) -> Nil:
    for _i in 0..3:
        let o = chan.recv(rx).await
        match o:
            Some(v) -> console.print("got ${v}")
            None -> console.print("closed")

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "for-await schedule diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 2", "got 3"]);
    }

    // `for await x in rx:` — a receive loop over a channel whose body may itself
    // `await` (here it forwards a squared value). Lowers to chan.consume; both
    // backends agree byte-for-byte.
    #[test]
    fn for_await_over_receiver_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn producer(tx: Sender(Int)) -> Nil:
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn relay(rx: Receiver(Int), out: Sender(Int)) -> Nil:
    for await x in rx:
        chan.send(out, x * x).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let (otx, orx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    task.spawn(relay(rx, otx)).await
    chan.consume(orx, fn(v): task.done(console.print("got ${v}"))).await"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "for-await-over-receiver diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 4", "got 9"]);
    }

    // The multi-actor case: each task has its OWN inbox, so several actors with
    // separate mailboxes run together (what a single shared channel cannot do).
    // A logger (#0), a forwarder (#1) that relays to the logger, and a driver (#2)
    // that messages both — `send(target, msg)` routes by actor index. This is the
    // shape `examples/actors/src/actors.witchy` (Logger + Forwarder) needs, now in async/chan,
    // byte-identical on both backends.
    #[test]
    fn chan_multi_actor_separate_inboxes_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn logger(console: Console, rx: Receiver(Int)) -> Nil:
    chan.consume(rx, fn(a): task.done(console.print("log ${a}"))).await

async fn forwarder(rx: Receiver(Int), log_tx: Sender(Int)) -> Nil:
    chan.consume(rx, fn(m): chan.send(log_tx, m)).await

async fn driver(log_tx: Sender(Int), fwd_tx: Sender(Int)) -> Nil:
    chan.send(log_tx, 100).await
    chan.send(fwd_tx, 200).await

async fn main(console: Console):
    let (log_tx, log_rx) = chan.channel(4).await
    let (fwd_tx, fwd_rx) = chan.channel(4).await
    let lh = task.spawn(logger(console, log_rx)).await
    let fh = task.spawn(forwarder(fwd_rx, log_tx)).await
    driver(log_tx, fwd_tx).await
    task.join(fh).await
    task.join(lh).await"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "multi-actor schedule diverged across backends");
        assert_eq!(interp_out, vec!["log 100", "log 200"]);
    }

    // (RFC-0055 acceptance #1) Two INDEPENDENT modules, each with a PRIVATE channel
    // of a DIFFERENT message type — one `Int`, one record `Note` — linked into one
    // program. This is the case that was IMPOSSIBLE before erasure: the executor
    // was monomorphic over one program-wide message type, so a second channel of a
    // different type failed with "expected Int, found String"-style unification.
    // Now the executor is erased and each `Sender(m)`/`Receiver(m)` carries its own
    // `m`; a library can pipeline work through a channel privately. Byte-identical
    // on both backends.
    #[test]
    fn rfc0055_two_modules_private_channels_of_different_types() {
        // Module A: a private Int channel behind a public `run` entry.
        let counter = r#"import chan
from chan import Sender

async fn feed(tx: Sender(Int)) -> Nil:
    chan.send(tx, 7).await
    chan.send(tx, 35).await

pub async fn total(console: Console) -> Nil:
    let (tx, rx) = chan.channel(4).await
    chan.spawn(feed(tx)).await
    let a = chan.recv(rx).await
    let b = chan.recv(rx).await
    match a:
        Some(x) -> match b:
            Some(y) -> console.print("sum ${x + y}")
            None -> console.print("sum none")
        None -> console.print("sum none")"#;
        // Module B: a private RECORD channel — a different message type entirely.
        let notes = r#"import chan
from chan import Sender

type Note:
    Note(String)

async fn emit(tx: Sender(Note)) -> Nil:
    chan.send(tx, Note("hi")).await

pub async fn announce(console: Console) -> Nil:
    let (tx, rx) = chan.channel(4).await
    chan.spawn(emit(tx)).await
    let o = chan.recv(rx).await
    match o:
        Some(Note(s)) -> console.print("note ${s}")
        None -> console.print("note none")"#;
        // The entry drives both private pipelines in ONE run — one erased executor,
        // two different message types coexisting.
        let app = r#"import counter
import notes

async fn main(console: Console):
    counter.total(console).await
    notes.announce(console).await
"#;
        let want = vec!["sum 42".to_string(), "note hi".to_string()];
        let link = || {
            let app_m = parser::parse_module(app).expect("parse app");
            let counter_m = parser::parse_module(counter).expect("parse counter");
            let notes_m = parser::parse_module(notes).expect("parse notes");
            crate::pipeline::link(
                vec![
                    ("main".into(), app_m),
                    ("counter".into(), counter_m),
                    ("notes".into(), notes_m),
                ],
                "main",
            )
            .expect("link")
        };
        let linked = link();
        typeck::check(&linked).expect("typecheck");
        let interp_out = interpreter::run_module(linked, ".", Vec::new()).expect("interp");
        assert_eq!(interp_out, want, "interpreter");

        let linked = link();
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this multi-type program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "multi-type channels diverged across backends");
        assert_eq!(wasm_out, want, "compiled WASM");
    }

    // (RFC-0055 acceptance) A single TASK that pulls `Job`s and pushes `Answer`s —
    // two DIFFERENT message types touched by one task. Design (a) (per-type
    // executor islands) structurally could not express this (a `Task` belonged to
    // one instantiation); erasure makes it ordinary. Both backends agree.
    #[test]
    fn rfc0055_one_task_two_message_types_job_answer() {
        let src = r#"
import chan
from chan import Receiver, Sender

type Job:
    Job(Int)

type Answer:
    Answer(Int)

async fn worker(jobs: Receiver(Job), out: Sender(Answer)) -> Nil:
    for await j in jobs:
        match j:
            Job(n) -> chan.send(out, Answer(n * n)).await

async fn main(console: Console):
    let (jtx, jrx) = chan.channel(4).await
    let (atx, arx) = chan.channel(4).await
    chan.spawn(worker(jrx, atx)).await
    chan.send(jtx, Job(3)).await
    chan.send(jtx, Job(5)).await
    let a1 = chan.recv(arx).await
    let a2 = chan.recv(arx).await
    match a1:
        Some(Answer(v)) -> console.print("answer ${v}")
        None -> console.print("none")
    match a2:
        Some(Answer(v)) -> console.print("answer ${v}")
        None -> console.print("none")"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "job/answer schedule diverged across backends");
        assert_eq!(interp_out, vec!["answer 9", "answer 25"]);
    }

    // (RFC-0042) THE headline acceptance test: `import iter` + `import chan` in ONE
    // program. Both std modules declare a `type Step`; under the old flat type
    // namespace their variant sets merged and the program failed to compile inside
    // std internals — the two flagship modules were mutually exclusive. Module-
    // scoped types make them `iter.Step` and `chan.Step`, so the two coexist and
    // the program runs byte-identically on both backends. The iter side computes a
    // value; the chan side drives a channel round-trip — exercising both `Step`s.
    #[test]
    fn iter_and_chan_coexist_backends_agree() {
        let src = r#"
import iter
import chan
from chan import Sender, Receiver

fn doubled(xs: List(Int)) -> List(Int):
    iter.collect(iter.from_list(xs).map(fn(x): x * 2))

async fn producer(tx: Sender(Int)) -> Nil:
    chan.send(tx, 41).await

async fn main(console: Console):
    console.print("${doubled([1, 2, 3])}")
    let (tx, rx) = chan.channel(1).await
    task.spawn(producer(tx)).await
    chan.consume(rx, fn(v): task.done(console.print("recv ${v}"))).await
    console.print("done")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect("iter+chan link (RFC-0042: the Step collision must be gone)");
        typeck::check(&linked).expect("typecheck");
        let interp_out = interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "iter+chan diverged across backends");
        assert_eq!(interp_out, vec!["[2, 4, 6]", "recv 41", "done"]);
    }

    // The channel message type is GENERIC (here `String`), proving the explicit
    // type-parameter fix to the monomorphizer: a multi-param ADT whose constructor
    // omits a param (`Done(a)` for `Step(m, a)`) now keeps that param generic
    // because `type Step(m, a)` fixes the order. Byte-identical on both backends.
    #[test]
    fn chan_generic_message_type_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn producer(tx: Sender(String)) -> Nil:
    chan.send(tx, "alice").await
    chan.send(tx, "bob").await

async fn consumer(console: Console, rx: Receiver(String)) -> Nil:
    chan.consume(rx, fn(name): task.done(console.print("hello ${name}"))).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "generic-message channel diverged across backends");
        assert_eq!(interp_out, vec!["hello alice", "hello bob"]);
    }

    // `chan.select` races two receivers, taking from whichever is ready (a tie
    // favours the first) and yielding `Closed` once neither can deliver. Both
    // backends must agree on the merged order.
    #[test]
    fn chan_select_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn pa(tx: Sender(Int)) -> Nil:
    chan.send(tx, 1).await
    chan.send(tx, 2).await

async fn pb(tx: Sender(Int)) -> Nil:
    chan.send(tx, 9).await

async fn collector(console: Console, a: Receiver(Int), b: Receiver(Int)) -> Nil:
    let s = chan.select(a, b).await
    match s:
        First(x) ->
            console.print("a ${x}")
            collector(console, a, b).await
        Second(y) ->
            console.print("b ${y}")
            collector(console, a, b).await
        Closed -> console.print("done")

async fn main(console: Console):
    let (atx, arx) = chan.channel(4).await
    let (btx, brx) = chan.channel(4).await
    task.spawn(pa(atx)).await
    task.spawn(pb(btx)).await
    collector(console, arx, brx).await"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "select schedule diverged across backends");
        assert_eq!(interp_out, vec!["a 1", "a 2", "b 9", "done"]);
    }

    // Phase 5 (racing): `future.select` drives tasks concurrently and returns the
    // first to finish, dropping the losers. Among tasks of length 5/2/8, the
    // index-1 task (length 2) wins first — deterministically on both backends.
    #[test]
    fn future_select_first_wins_backends_agree() {
        let src = r#"
import future
from future import Future

fn counter(label: Int, steps: Int) -> Future(Int):
    if steps <= 0:
        future.ready(label)
    else:
        future.and_then(future.pending(0), fn(_a): counter(label, steps - 1))

fn main(console: Console):
    let (idx, val) = future.select([counter(10, 5), counter(20, 2), counter(30, 8)])
    console.print("winner ${idx} ${val}")"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "select diverged across backends");
        assert_eq!(interp_out, vec!["winner 1 20"]);
    }

    // The coloring rule: `await` is a parse error outside an `async fn`.
    #[test]
    fn await_outside_async_is_a_parse_error() {
        // `.await` is postfix and legal only inside an `async fn`.
        let src = "fn f():\n    let _x = (5).await\n";
        let err = parser::parse_module(src).expect_err("`.await` in a sync fn must not parse");
        assert!(
            format!("{err:?}").contains("async fn"),
            "error should name the async-fn rule: {err:?}"
        );
        // A leading `await` (the old prefix form) is no longer accepted at all.
        assert!(parser::parse_module("async fn main():\n    await f()\n").is_err());
    }

    // Phase 3 of the concurrency redesign: the deterministic round-robin executor
    // `future.join_all`, written in pure witchy over the `std/future` substrate.
    // Two cooperative tasks (each yielding via `future.pending`) interleave at
    // their yield points in a fixed schedule, so the interleaved output is
    // byte-identical on both backends — concurrency with parity, no scheduler
    // state in the runtime and no WASM feature.
    #[test]
    fn future_executor_interleaves_backends_agree() {
        let src = r#"
import future
from future import Future

fn ticker(console: Console, name: String, n: Int) -> Future(Int):
    if n <= 0:
        future.ready(n)
    else:
        future.and_then(future.defer(fn(): console.print(name + " " + "${n}")), fn(_a):
            future.and_then(future.pending(0), fn(_b):
                ticker(console, name, n - 1)))

fn main(console: Console):
    let results = future.join_all([ticker(console, "A", 2), ticker(console, "B", 2)])
    console.print("done ${results}")"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "executor schedule diverged across backends");
        assert_eq!(interp_out, vec!["A 2", "B 2", "A 1", "B 1", "done [0, 0]"]);
    }
