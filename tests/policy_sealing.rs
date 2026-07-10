fn check(source: &str) -> Result<(), String> {
    let linked = witchy::resolve_std_only(source)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

#[test]
fn raw_policy_values_cannot_be_forged_outside_std_policy() {
    for (name, raw) in [("NetPolicy", "example.com:443"), ("DirPolicy", "")]
    {
        let source = format!(
            "import policy\nfn main(console: Console):\n    let _policy = policy.{name}(\"{raw}\")\n    console.print(\"forged\")\n"
        );
        let error = check(&source).expect_err("policy representations must be sealed");
        assert!(
            error.contains("sealed type") && error.contains(name),
            "raw {name} should fail at the constructor boundary: {error}",
        );
    }
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
