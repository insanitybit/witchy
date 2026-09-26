//! Public list mutators must retain the value semantics of their append result.

#[test]
fn list_push_value_rejects_later_argument_replacing_reserved_receiver_root() {
    let source = r#"
type Buf:
    items: List(String)

fn replace(var buf: Buf) -> String:
    buf = Buf(["changed"])
    "tail"

fn main(console: Console):
    var b = Buf(["old"])
    b.items.push(replace(b))
    console.print(b.items.join("|"))
"#;
    for source in [source.to_owned(), format!("mode opt\n{source}")] {
        let message = super::source::checked(&source)
            .expect_err("later argument must not replace the reserved receiver root");
        assert!(
            message.contains("reserves `var` place rooted in `b`"),
            "{message}"
        );
        assert!(
            message.contains("later argument 2 writes back to an overlapping place"),
            "{message}"
        );
    }
}
