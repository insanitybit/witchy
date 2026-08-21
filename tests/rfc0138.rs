//! RFC-0138 end-to-end callable contracts. These tests deliberately cross
//! aliases, generic containers, branches, must wrappers, interpreter execution,
//! and several compiled closure layouts rather than treating type-checking as
//! sufficient evidence.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, opt};

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

const HIGHER_ORDER_TRANSPORT: &str = r#"
type Parcel(a):
    Parcel(a)

type OnceBox:
    OnceBox(once fn(Int) -> Int)

must sealed type Completion:
    Completion(once fn(String) -> Nil)

must sealed type Transaction:
    Transaction(Int)

must sealed type TransactionCompletion:
    TransactionCompletion(Transaction, once fn(own Transaction) -> Nil)

pure fn apply_pure(callback: pure fn(Int) -> Int, value: Int) -> Int:
    callback(value)

fn log_twice(own callback: fn(String) -> Nil):
    callback("first")
    callback("second")

fn call_once(own callback: once fn(Int) -> Int, value: Int) -> Int:
    callback(value)

fn call_pure_once(own callback: pure once fn(Int) -> Int, value: Int) -> Int:
    callback(value)

fn pack_once(own callback: once fn(Int) -> Int) -> OnceBox:
    OnceBox(move callback)

fn run_box(own boxed: OnceBox, value: Int) -> Int:
    match boxed:
        OnceBox(callback) -> callback(value)

fn parcel(own value: a) -> Parcel(a):
    Parcel(move value)

fn unpack(own wrapped: Parcel(a)) -> a:
    match wrapped:
        Parcel(value) -> value

fn select_once(
    choose_left: Bool,
    own left: once fn(Int) -> Int,
    own right: once fn(Int) -> Int,
) -> once fn(Int) -> Int:
    if choose_left:
        move left
    else:
        move right

fn finish(own completion: Completion, message: String):
    match completion:
        Completion(callback) -> callback(message)

fn cancel(own completion: Completion):
    match completion:
        Completion(_) -> ()

fn finish_transaction(own completion: TransactionCompletion):
    match completion:
        TransactionCompletion(transaction, callback) -> callback(transaction)

fn cancel_transaction(own completion: TransactionCompletion, console: Console):
    match completion:
        TransactionCompletion(transaction, _) ->
            match transaction:
                Transaction(identifier) -> console.print("transaction-cancelled-${identifier}")

fn main(console: Console):
    let offset = 2
    let plugin: pure fn(Int) -> Int = pure fn(value: Int): value * 2 + offset
    console.print("pure-${apply_pure(plugin, 20)}")

    let logger = fn(message: String): console.print("delegated-${message}")
    log_twice(logger)

    let direct: once fn(Int) -> Int = once fn(value: Int): value * 2
    console.print("once-${call_once(direct, 5)}")

    let boxed = pack_once(once fn(value: Int): value + 4)
    console.print("boxed-${run_box(boxed, 6)}")

    let transported = parcel(once fn(value: Int): value + 7)
    let unpacked = unpack(transported)
    console.print("generic-${unpacked(4)}")

    let selected = select_once(
        false,
        once fn(value: Int): value + 100,
        once fn(value: Int): value + 8,
    )
    console.print("branch-${selected(5)}")

    let pure_single: pure once fn(Int) -> Int =
        pure once fn(value: Int): value * 3
    console.print("pure-once-${call_pure_once(pure_single, 4)}")

    let completion = Completion(
        once fn(message: String): console.print("must-${message}")
    )
    finish(completion, "done")

    let abandoned = Completion(
        once fn(message: String): console.print("unexpected-${message}")
    )
    cancel(abandoned)

    let transaction_completion = TransactionCompletion(
        Transaction(21),
        once fn(own transaction: Transaction):
            match transaction:
                Transaction(identifier) -> console.print("transaction-finished-${identifier}")
    )
    finish_transaction(transaction_completion)

    let transaction_cancellation = TransactionCompletion(
        Transaction(22),
        once fn(own transaction: Transaction):
            match transaction:
                Transaction(identifier) -> console.print("unexpected-transaction-${identifier}")
    )
    cancel_transaction(transaction_cancellation, console)
"#;

#[test]
fn higher_order_transport_has_interpreter_wasm_and_optimization_parity() {
    let checked = witchy::resolve_std_only_checked(HIGHER_ORDER_TRANSPORT)
        .expect("RFC-0138 higher-order transport type-checks");
    let expected = vec![
        "pure-42".to_string(),
        "delegated-first".to_string(),
        "delegated-second".to_string(),
        "once-10".to_string(),
        "boxed-10".to_string(),
        "generic-11".to_string(),
        "branch-13".to_string(),
        "pure-once-12".to_string(),
        "must-done".to_string(),
        "transaction-finished-21".to_string(),
        "transaction-cancelled-22".to_string(),
    ];

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret RFC-0138 higher-order transport");
    assert_eq!(
        interpreted, expected,
        "interpreter result uses an independent oracle",
    );

    let _reset = OptReset;
    let configurations = [
        ("all", opt::OptSet::all()),
        (
            "forced-copy",
            opt::OptSet::all().without(opt::Opt::InPlace),
        ),
        (
            "boxed-devirtualized",
            opt::OptSet::all().without(opt::Opt::ClosureElide),
        ),
        (
            "boxed-indirect",
            opt::OptSet::all()
                .without(opt::Opt::ClosureElide)
                .without(opt::Opt::DirectCall),
        ),
        ("none", opt::OptSet::none()),
    ];
    for (configuration, options) in configurations {
        opt::set_for_tests(Some(options));
        let wasm = codegen::compile_checked_module_binary(&checked)
            .expect_lowered(&format!("{configuration}: RFC-0138 source lowers"));
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &wasm,
                Capabilities { print: true, quiet: true, ..Default::default() },
                128,
            )
            .expect("spawn compiled RFC-0138 fixture");
        actor
            .run()
            .unwrap_or_else(|error| panic!("{configuration}: compiled execution: {error}"));
        assert_eq!(
            actor.output(),
            expected,
            "{configuration}: compiled result uses the same independent oracle",
        );
    }
}

struct Rejection {
    name: &'static str,
    source: &'static str,
    needles: &'static [&'static str],
}

#[test]
fn security_sensitive_callable_rejections_are_shared_frontend_evidence() {
    let cases = [
        Rejection {
            name: "pure capability operation",
            source: r#"
pure fn announce(console: Console):
    console.print("effect")

fn main():
    let _ = 0
"#,
            needles: &["pure", "console"],
        },
        Rejection {
            name: "pure opaque callback",
            source: r#"
pure fn invoke(callback: fn(Int) -> Int) -> Int:
    callback(1)

fn main():
    let _ = 0
"#,
            needles: &["pure", "callback"],
        },
        Rejection {
            name: "pure capability capture",
            source: r#"
fn main(console: Console):
    let invalid: pure fn(String) -> Nil =
        pure fn(message: String): console.print(message)
    invalid("effect")
"#,
            needles: &["pure", "console"],
        },
        Rejection {
            name: "pure parameter writeback",
            source: r#"
pure fn replace(var value: Int):
    value = 2

fn main():
    var value = 1
    replace(value)
"#,
            needles: &["pure", "var"],
        },
        Rejection {
            name: "ordinary callable cannot narrow to pure",
            source: r#"
fn ordinary(value: Int) -> Int:
    value

fn main():
    let invalid: pure fn(Int) -> Int = ordinary
"#,
            needles: &["pure fn"],
        },
        Rejection {
            name: "once callable cannot be invoked twice",
            source: r#"
fn main():
    let callback: once fn(Int) -> Int = once fn(value: Int): value
    let _ = callback(1)
    callback(2)
"#,
            needles: &["once-callable", "consumed"],
        },
        Rejection {
            name: "once callable cannot be copied through an alias",
            source: r#"
fn main():
    let callback: once fn(Int) -> Int = once fn(value: Int): value
    let copied = callback
    copied(1)
"#,
            needles: &["once-callable", "copied"],
        },
        Rejection {
            name: "let-borrowed once callable cannot be invoked",
            source: r#"
fn invalid(let callback: once fn(Int) -> Int) -> Int:
    callback(1)

fn main():
    let _ = 0
"#,
            needles: &["borrowed once-callable", "consumes"],
        },
        Rejection {
            name: "reusable callable cannot narrow to once",
            source: r#"
fn reusable(value: Int) -> Int:
    value

fn main():
    let invalid: once fn(Int) -> Int = reusable
"#,
            needles: &["once fn"],
        },
        Rejection {
            name: "must wrapper cannot be dropped",
            source: r#"
must sealed type Completion:
    Completion(once fn(String) -> Nil)

fn main():
    let completion = Completion(once fn(message: String): ())
"#,
            needles: &["must-consume", "completion"],
        },
        Rejection {
            name: "opaque callable cannot hide a must obligation",
            source: r#"
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket):
    match ticket:
        Ticket(_) -> ()

fn main():
    let ticket = Ticket(1)
    let hidden = fn(): finish(ticket)
    hidden()
"#,
            needles: &["closure environment carries must-consume", "ticket"],
        },
        Rejection {
            name: "Dynamic rejects callable payloads",
            source: r#"
import dynamic

fn main():
    let callback: pure once fn(Int) -> Int = pure once fn(value: Int): value
    let hidden = dynamic.dynamic(callback)
"#,
            needles: &["dynamic", "function"],
        },
    ];

    for case in cases {
        let error = witchy::resolve_std_only_checked(case.source)
            .expect_err(case.name);
        let diagnostic = error.to_string().to_lowercase();
        for needle in case.needles {
            assert!(
                diagnostic.contains(&needle.to_lowercase()),
                "{} diagnostic must contain `{needle}`: {error}",
                case.name,
            );
        }
    }
}
