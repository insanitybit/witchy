use witchy::{interpreter, parser, pipeline, typeck};

#[test]
fn public_state_record_sum_and_generic_witnesses_typecheck_and_run() {
    let source = r#"
import public_state

type Account derive(PublicState):
    name: String
    balance: Int

type Event(a) derive(PublicState):
    Created(a)
    Empty

fn main(console: Console):
    public_state.check(Account("Ada", 7))
    let created: Event(String) = Created("created")
    let empty: Event(Int) = Empty
    public_state.check(created)
    public_state.check(empty)
    console.print("ok")
"#;
    let parsed = parser::parse_module(source).expect("parse PublicState witness");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main").expect("link PublicState witness");
    typeck::check(&linked).expect("typecheck PublicState witness");
    assert_eq!(interpreter::run_module(linked, ".", Vec::new()).expect("interpret"), ["ok"]);
}

#[test]
fn public_state_rejects_function_fields() {
    let source = r#"
import public_state

type Bad derive(PublicState):
    callback: fn(Int) -> Int
"#;
    let parsed = parser::parse_module(source).expect("parse negative PublicState witness");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main").expect("link negative witness");
    let error = typeck::check(&linked).expect_err("function fields must not derive PublicState");
    assert!(error.to_string().contains("PublicState"), "{error}");
}

#[test]
fn derived_order_is_lexicographic_and_preserves_float_none() {
    let source = r#"
import cmp

type Version derive(PartialEq, Eq, PartialOrd, Ord):
    major: Int
    minor: Int

type Measured derive(PartialEq, PartialOrd):
    value: Float
    rank: Int

fn describe(order: Option(Ordering)) -> String:
    match order:
        Some(Less) -> "less"
        Some(Greater) -> "greater"
        Some(Equal) -> "equal"
        None -> "none"

fn main(console: Console):
    console.print(describe(Version(1, 9).partial_compare(Version(1, 12))))
    console.print(describe(Version(1, 12).partial_compare(Version(1, 9))))
    console.print(describe(Version(1, 12).partial_compare(Version(1, 12))))
    let nan = 0.0 / 0.0
    console.print(describe(Measured(nan, 1).partial_compare(Measured(0.0, 2))))
    console.print(describe(Measured(1.0, 1).partial_compare(Measured(1.0, 2))))
"#;
    let parsed = parser::parse_module(source).expect("parse ordering witness");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link ordering witness");
    typeck::check(&linked).expect("typecheck ordering witness");
    assert_eq!(
        interpreter::run_module(linked, ".", Vec::new()).expect("interpret ordering witness"),
        ["less", "greater", "equal", "none", "less"]
    );
}

#[test]
fn derived_ord_generic_bound_accepts_int_and_rejects_float() {
    let accepted = r#"
import cmp

type Boxed(a) derive(PartialEq, Eq, PartialOrd, Ord):
    value: a

fn main(console: Console):
    console.print("${Boxed(1).compare(Boxed(2))}")
"#;
    let parsed = parser::parse_module(accepted).expect("parse generic Ord witness");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link generic Ord witness");
    typeck::check(&linked).expect("Int satisfies derived Ord bound");

    let rejected = r#"
import cmp

type Boxed(a) derive(PartialEq, Eq, PartialOrd, Ord):
    value: a

fn main(console: Console):
    let _ = Boxed(1.0).compare(Boxed(2.0))
"#;
    let parsed = parser::parse_module(rejected).expect("parse rejected generic Ord witness");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link rejected generic Ord witness");
    let error = typeck::check(&linked).expect_err("Float must not satisfy derived Ord bound");
    assert!(error.to_string().contains("Ord"), "{error}");
}
