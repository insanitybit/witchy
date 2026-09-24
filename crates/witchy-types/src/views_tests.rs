use crate::typeck::check;

#[test]
fn view_qualifiers_and_unsupported_authority_carriers_reject() {
    for source in [
        "fn invalid(value: unique &Int):\n    ()\n",
        "fn invalid(value: frozen &Int):\n    ()\n",
        "fn invalid(value: &Console):\n    ()\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(check(&module).is_err(), "{source}");
    }
    for source in [
        "fn inspect(value: &unique Int):\n    ()\n",
        "fn inspect(value: &frozen Int):\n    ()\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        check(&module).expect("payload qualifiers remain legal");
    }
}

#[test]
fn views_cannot_launder_must_or_once_payload_obligations() {
    for source in [
        "must type Ticket:\n    Ticket(Int)\n\nfn main():\n    let ticket = Ticket(1)\n    let view = &ticket\n",
        "fn main():\n    let action: once fn() -> Int = once fn() -> Int: 1\n    let view = &action\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(check(&module).is_err(), "copyable views preserve payload obligations: {source}");
    }
}

