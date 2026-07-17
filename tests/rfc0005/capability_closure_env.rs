//! RFC-0005 capability-safe closure environments and function-valued GC
//! aggregates. The same source runs through the interpreter and Wasm backend.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, opt, parser, pipeline, typeck};

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn capability_captures_survive_aliases_aggregates_and_indirect_calls() {
    let root = std::env::temp_dir().join(format!(
        "witchy_capability_closure_env_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "value").expect("seed file");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
type Reader:
    callback: fn(String) -> String

type Vault:
    dir: Dir[Read]
    label: String

fn invoke(reader: Reader, name: String) -> String:
    match reader:
        Reader(callback) -> callback(name)

fn callback_count(callbacks: List(fn(String) -> String)) -> Int:
    list.length(callbacks)

fn invoke_pair(pair: (Dir[Read], fn(String) -> String), name: String) -> String:
    match pair:
        (dir, callback) -> dir.read(name) + "|" + callback(name)

fn main(console: Console, root: Dir[Read]):
    let prefix = "P:"
    let read = fn(name: String) -> String: prefix + root.read(name)
    let alias = read
    let reader = Reader(alias)
    console.print(invoke(reader, "value.txt"))

    let chained = fn(name: String) -> String: alias(name) + "!"
    console.print(chained("value.txt"))

    let pair = (root, alias)
    let from_pair = fn(name: String) -> String:
        invoke_pair(pair, name)
    console.print(from_pair("value.txt"))

    let vault = Vault(root, "vault:")
    let from_vault = fn(name: String) -> String:
        vault.label + vault.dir.read(name)
    console.print(from_vault("value.txt"))

    let original = [alias]
    var extended = original
    list.push(extended, chained)
    console.print("${list.length(original)}:${list.length(extended)}")
    for callback in extended:
        console.print(callback("value.txt"))

    let concatenated = list.concat(original, [chained])
    var replaced = concatenated
    list.set_at(replaced, 0, chained)
    console.print("${list.length(original)}:${list.length(concatenated)}:${callback_count(replaced)}")
    let first = list.at(replaced, 0)
    console.print(first("value.txt"))
    for callback in replaced:
        console.print(callback("value.txt"))
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("capability closure environments typecheck");

    let expected = vec![
        "P:value".to_string(),
        "P:value!".to_string(),
        "value|P:value".to_string(),
        "vault:value".to_string(),
        "1:2".to_string(),
        "P:value".to_string(),
        "P:value!".to_string(),
        "1:2:2".to_string(),
        "P:value!".to_string(),
        "P:value!".to_string(),
        "P:value!".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter capability closures",
    );

    let _reset = OptReset;
    let configurations = [
        ("all", opt::OptSet::all()),
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
        let wasm = codegen::compile_module_binary(&linked)
            .expect_lowered(&format!("{configuration}: capability closures lower to WIR"));
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &wasm,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("compiled execution");
        assert_eq!(
            actor.output(),
            expected,
            "{configuration}: compiled capability closures",
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
