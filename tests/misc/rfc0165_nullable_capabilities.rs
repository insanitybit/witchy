//! Capability options preserve absence without storing authority in memory.

#[test]
fn nullable_capability_source_refuses_forgery_and_unchecked_use() {
    assert!(witchy::resolve_std_only_checked(
        "capability Output from Console\nfn main(console: Console):\n    let forged = Output(1)\n"
    ).is_err());
    assert!(witchy::resolve_std_only_checked(
        "fn bad(console: Option(Console)):\n    console.print(\"unchecked\")\nfn main(console: Console):\n    bad(None)\n"
    ).is_err());
}
