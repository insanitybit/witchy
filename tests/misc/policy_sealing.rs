fn check(source: &str) -> Result<(), String> {
    let linked = witchy::resolve_std_only(source)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

#[test]
fn raw_policy_values_cannot_be_forged_outside_std_policy() {
    for (name, raw) in [("NetPolicy", "example.com:443"), ("DirPolicy", "")]
    {
        let source = format!(
            "fn main(console: Console):\n    let _policy = {name}(\"{raw}\")\n    console.print(\"forged\")\n"
        );
        let error = check(&source).expect_err("policy representations must be sealed");
        assert!(
            error.contains("sealed type") && error.contains(name),
            "raw {name} should fail at the constructor boundary: {error}",
        );
    }

    for name in ["NetPolicy", "DirPolicy"] {
        let source = format!(
            "type {name}:\n    {name}(String)\n\nfn main(console: Console):\n    console.print(\"shadowed\")\n"
        );
        let error = check(&source).expect_err("an ambient policy lookalike must not shadow std");
        assert!(
            error.contains("shadows the ambient built-in name") && error.contains(name),
            "local {name} lookalike should be rejected: {error}",
        );
    }
}

#[test]
fn local_policy_module_cannot_impersonate_bundled_policy_owner() {
    let policy = witchy::parser::parse_module(
        "sealed type NetPolicy:\n    pattern: String\n\npub fn raw(value: String) -> NetPolicy:\n    NetPolicy(value)\n",
    )
    .expect("local policy parses");
    let main = witchy::parser::parse_module(
        "import policy\n\nfn main(console: Console):\n    let _ = policy.raw(\"*:*\")\n    console.print(\"forged\")\n",
    )
    .expect("main parses");
    let user_modules = std::collections::HashSet::from(["policy".to_string(), "main".to_string()]);
    let error = witchy::pipeline::link_with_user_modules(
        vec![("policy".to_string(), policy), ("main".to_string(), main)],
        "main",
        &user_modules,
    )
    .expect_err("a local module cannot own a reserved std name")
    .message;
    assert!(
        error.contains("module `policy` uses a reserved standard-library name")
            && error.contains("one canonical owner"),
        "{error}",
    );
}

#[test]
fn checked_policy_builders_remain_the_public_minting_surface() {
    let source = r#"
import policy

fn narrow(net: Net, dir: Dir):
    let endpoint = net.only(Net.tcp("example.com", 443))
    let logs = dir.only(Dir.ext(".log"))
    let files = logs.only(Dir.files())
    let _ = endpoint
    let _ = files

fn main(console: Console):
    console.print("ok")
"#;
    check(source).expect("checked Net and Dir policy constructors should remain usable");
}
