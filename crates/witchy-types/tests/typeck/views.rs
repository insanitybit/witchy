use witchy_types::typeck::check;

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

    let phantom = "type Phantom(a):\n    Phantom(Int)\n\nfn inspect[a](own value: Phantom(a)) -> Int:\n    let view = &value\n    1\n\nfn main() -> Int:\n    let value: Phantom(Clock) = Phantom(1)\n    inspect(value)\n";
    let module = witchy_syntax::parser::parse_module(phantom).expect("phantom source parses");
    check(&module).expect("a phantom resource argument does not enter retained storage");
}

#[test]
fn a_generic_stored_resource_cannot_become_a_retained_view() {
    let source = "must type Ticket:\n    Ticket(Int)\n\ntype Wrapper(a):\n    payload: a\n\nfn main():\n    let wrapped = Wrapper(payload: Ticket(7))\n    let view = &wrapped\n";
    let module = witchy_syntax::parser::parse_module(source).expect("source parses");
    let error = check(&module).expect_err("a stored generic field cannot hide a must obligation");
    assert!(error.message.contains("must-consume storage cannot be copied into a persistent view"), "{error:?}");
}
