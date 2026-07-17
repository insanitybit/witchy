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

type GenericBox(a):
    GenericBox(a)

type Chain(a):
    ChainEnd
    ChainLink(a, Chain(a))

fn invoke(reader: Reader, name: String) -> String:
    match reader:
        Reader(callback) -> callback(name)

fn callback_count(callbacks: List(fn(String) -> String)) -> Int:
    list.length(callbacks)

fn invoke_pair(pair: (Dir[Read], fn(String) -> String), name: String) -> String:
    match pair:
        (dir, callback) -> dir.read(name) + "|" + callback(name)

fn replace(var callback: fn(String) -> String, next: fn(String) -> String):
    callback = next

fn add1(value: Int) -> Int:
    value + 1

fn box_fn(value: fn(Int) -> Int) -> GenericBox(fn(Int) -> Int):
    GenericBox(value)

fn box_file(value: File[Read]) -> GenericBox(File[Read]):
    GenericBox(value)

fn maybe_fn(value: fn(Int) -> Int) -> Option(fn(Int) -> Int):
    Some(value)

fn result_fn(value: fn(Int) -> Int) -> Result(fn(Int) -> Int, String):
    Ok(value)

fn chain_fn(value: fn(Int) -> Int) -> Chain(fn(Int) -> Int):
    ChainLink(value, ChainEnd)

fn append_owned(
    own values: List(GenericBox(fn(Int) -> Int)),
    value: GenericBox(fn(Int) -> Int),
) -> List(GenericBox(fn(Int) -> Int)):
    list.concat(values, [value])

fn main(console: Console, root: Dir[Read]):
    let prefix = "P:"
    let read_value = fn(name: String) -> String: prefix + root.read(name)
    let alias = read_value
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

    var reassigned = [alias]
    reassigned = [chained]
    let reassigned_first = list.at(reassigned, 0)
    console.print(reassigned_first("value.txt"))

    var indexed = [alias]
    replace(indexed[0], chained)
    let indexed_first = list.at(indexed, 0)
    console.print(indexed_first("value.txt"))

    let files: List(File[Read]) = [root.read_file("value.txt")]
    let first_file: File[Read] = list.at(files, 0)
    console.print(first_file.read())

    match box_file(root.read_file("value.txt")):
        GenericBox(file) -> console.print(file.read())

    let boxes: List(GenericBox(fn(Int) -> Int)) = [box_fn(add1)]
    match list.at(boxes, 0):
        GenericBox(callback) -> console.print("${callback(6)}")

    match maybe_fn(add1):
        Some(callback) -> console.print("${callback(7)}")
        None -> console.print("none")

    match result_fn(add1):
        Ok(callback) -> console.print("${callback(8)}")
        Err(message) -> console.print(message)

    match chain_fn(add1):
        ChainLink(callback, _) -> console.print("${callback(9)}")
        ChainEnd -> console.print("end")

    let run_chain = fn(chain: Chain(fn(Int) -> Int)) -> Int:
        match chain:
            ChainLink(callback, _) -> callback(10)
            ChainEnd -> 0
    console.print("${run_chain(chain_fn(add1))}")

    let grow = fn(values: List(GenericBox(fn(Int) -> Int))) -> Int:
        let grown = append_owned(move values, box_fn(add1))
        list.length(grown)
    console.print("${grow([box_fn(add1)])}")
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
        "P:value!".to_string(),
        "P:value!".to_string(),
        "value".to_string(),
        "value".to_string(),
        "7".to_string(),
        "8".to_string(),
        "9".to_string(),
        "10".to_string(),
        "11".to_string(),
        "2".to_string(),
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
