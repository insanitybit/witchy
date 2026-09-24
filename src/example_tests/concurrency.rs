use super::*;

fn assert_backends_agree(src: &str, expected: &[&str], context: &str) {
    if !is_nightly() {
        return;
    }
    let interp_out = link_run(src);
    let wasm_out = wasm_run(src);
    assert_eq!(interp_out, wasm_out, "{context}");
    assert_eq!(interp_out, expected.iter().map(|line| (*line).to_string()).collect::<Vec<_>>());
}

    // Async functions lower to cooperative channel tasks; async `main` is the
    // executor entry. Both backends must agree.
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
        // pipeline(3): a=6, b=12, a+b=18.  double(10)=20.
        assert_backends_agree(src, &["18", "20"], "async lowering diverged across backends");
    }

    // (RFC-0055 acceptance) One task pulls `Job`s and pushes `Answer`s, proving
    // erased channels support two message types in one task.
    #[test]
    fn rfc0055_one_task_two_message_types_job_answer() {
        let src = r#"
import chan
from chan import Receiver, Sender

type Job:
    Job(Int)

type Answer:
    Answer(Int)

async fn worker(jobs: Receiver(Job), out: Sender(Answer)) -> ():
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
        assert_backends_agree(src, &["answer 9", "answer 25"], "job/answer schedule diverged across backends");
    }

    // `chan.select` chooses the first ready receiver on ties and yields `Closed`
    // once neither can deliver.
    #[test]
    fn chan_select_backends_agree() {
        let src = r#"
import chan
from chan import Receiver, Sender

async fn pa(tx: Sender(Int)) -> ():
    chan.send(tx, 1).await
    chan.send(tx, 2).await

async fn pb(tx: Sender(Int)) -> ():
    chan.send(tx, 9).await

async fn collector(console: Console, a: Receiver(Int), b: Receiver(Int)) -> ():
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
        assert_backends_agree(src, &["a 1", "a 2", "b 9", "done"], "select schedule diverged across backends");
    }

    // `await` is a parse error outside an `async fn`.
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

    // `future.join_all` uses the pure-language future substrate to interleave
    // cooperative tasks at deterministic yield points.
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
        assert_backends_agree(
            src,
            &["A 2", "B 2", "A 1", "B 1", "done [0, 0]"],
            "executor schedule diverged across backends",
        );
    }
