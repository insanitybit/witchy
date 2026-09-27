use super::source;

#[test]
fn specialization_and_contextual_try_preserve_pending_obligations() {
    for (label, input) in [
        ("context", r#"must type Ticket:
    Ticket(Int)
fn maybe() -> Result(String, String):
    Err("stop")
fn choose() -> Result(Ticket, String):
    Ok(Some(Ticket(7))? "${maybe()?}")
fn main() -> Int:
    0
"#),
        ("late-generic", r#"must type Ticket:
    Ticket(Int)
fn make[a]() -> Option(a):
    None
fn take[a](own first: Option(a), x: Int, own last: a) -> Result(Int, String):
    Ok(x)
fn maybe() -> Result(Int, String):
    Err("stop")
fn choose() -> Result(Int, String):
    take(make(), maybe()?, Ticket(7))
fn main() -> Int:
    choose() ?? 0
"#),
    ] {
        let error = source::checked_bundle(vec![("main", input)], "main")
            .err().expect("specialization and call rewriting must retain the obligation");
        assert!(format!("{error:?}").contains("already evaluated must-consume operand"), "{label}: {error:?}");
    }
}
