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

capability ConfigDir from Dir[Read]

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

fn propagate_option(value: Option(fn(Int) -> Int)) -> Option(fn(Int) -> Int):
    let callback = value?
    Some(callback)

fn propagate_option_with_progress(
    value: Option(fn(Int) -> Int),
    var progress: Int,
) -> Option(fn(Int) -> Int):
    progress = progress + 1
    let callback = value?
    Some(callback)

fn propagate_result(
    value: Result(fn(Int) -> Int, String),
) -> Result(fn(Int) -> Int, String):
    let callback = value?
    Ok(callback)

fn propagate_file(value: Option(File[Read])) -> Option(File[Read]):
    let file = value?
    Some(file)

fn propagate_file_result(
    value: Result(File[Read], String),
) -> Result(File[Read], String):
    let file = value?
    Ok(file)

fn option_file_to_callback(
    value: Option(File[Read]),
) -> Option(fn(Int) -> Int):
    let _ = value?
    None

fn option_int_to_callback(value: Option(Int)) -> Option(fn(Int) -> Int):
    let _ = value?
    None

fn option_callback_to_int(value: Option(fn(Int) -> Int)) -> Option(Int):
    let _ = value?
    None

fn result_file_to_callback(
    value: Result(File[Read], String),
) -> Result(fn(Int) -> Int, String):
    let _ = value?
    Err("unreachable")

fn unwrap_config_dir(value: ConfigDir) -> Dir[Read]:
    let ConfigDir(dir) = value
    dir

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
    console.print(unwrap_config_dir(ConfigDir(root)).read("value.txt"))

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
    var pop_files: unique List(File[Read]) = [root.read_file("value.txt")]
    match pop_files.pop():
        Some(file) -> console.print(file.read())
        None -> console.print("missing pop file")
    match pop_files.pop():
        Some(file) -> console.print(file.read())
        None -> console.print("empty file list")
    match propagate_file(Some(root.read_file("value.txt"))):
        Some(file) -> console.print(file.read())
        None -> console.print("no file")
    let no_file: Option(File[Read]) = None
    let fallback_file = no_file ?? root.read_file("value.txt")
    console.print(fallback_file.read())
    match propagate_file_result(Ok(root.read_file("value.txt"))):
        Ok(file) -> console.print(file.read())
        Err(message) -> console.print(message)
    match propagate_file_result(Err("file stopped")):
        Ok(file) -> console.print(file.read())
        Err(message) -> console.print(message)
    match option_file_to_callback(None):
        Some(callback) -> console.print("${callback(1)}")
        None -> console.print("file-to-callback none")
    match option_int_to_callback(None):
        Some(callback) -> console.print("${callback(1)}")
        None -> console.print("int-to-callback none")
    match option_callback_to_int(None):
        Some(value) -> console.print("${value}")
        None -> console.print("callback-to-int none")
    match result_file_to_callback(Err("cross-layout error")):
        Ok(callback) -> console.print("${callback(1)}")
        Err(message) -> console.print(message)
    let lambda_cross =
        fn(value: Option(File[Read])) -> Option(fn(Int) -> Int):
            let _ = value?
            None
    match lambda_cross(None):
        Some(callback) -> console.print("${callback(1)}")
        None -> console.print("lambda cross-layout none")

    match box_file(root.read_file("value.txt")):
        GenericBox(file) -> console.print(file.read())

    let boxes: List(GenericBox(fn(Int) -> Int)) = [box_fn(add1)]
    match list.at(boxes, 0):
        GenericBox(callback) -> console.print("${callback(6)}")
    match boxes:
        [GenericBox(callback)] -> console.print("${callback(18)}")
        _ -> console.print("bad box list")
    match [add1, add1]:
        [callback, ..remaining] ->
            console.print("${callback(19)}:${list.length(remaining)}")
        _ -> console.print("bad callback list")

    match maybe_fn(add1):
        Some(callback) -> console.print("${callback(7)}")
        None -> console.print("none")

    match result_fn(add1):
        Ok(callback) -> console.print("${callback(8)}")
        Err(message) -> console.print(message)

    let from_some = maybe_fn(add1) ?? add1
    console.print("${from_some(10)}")
    let absent: Option(fn(Int) -> Int) = None
    let from_none = absent ?? add1
    console.print("${from_none(11)}")
    let from_ok: Result(fn(Int) -> Int, String) = Ok(add1)
    let coalesced_ok = from_ok ?? add1
    console.print("${coalesced_ok(12)}")
    let from_err: Result(fn(Int) -> Int, String) = Err("fallback")
    let coalesced_err = from_err ?? add1
    console.print("${coalesced_err(13)}")

    match propagate_option(Some(add1)):
        Some(callback) -> console.print("${callback(14)}")
        None -> console.print("none")
    match propagate_option(None):
        Some(callback) -> console.print("${callback(15)}")
        None -> console.print("none")
    var progress = 0
    match propagate_option_with_progress(None, progress):
        Some(callback) -> console.print("${callback(15)}")
        None -> console.print("none with progress")
    console.print("${progress}")
    match propagate_result(Ok(add1)):
        Ok(callback) -> console.print("${callback(16)}")
        Err(message) -> console.print(message)
    match propagate_result(Err("stopped")):
        Ok(callback) -> console.print("${callback(17)}")
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
        "value".to_string(),
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
        "empty file list".to_string(),
        "value".to_string(),
        "value".to_string(),
        "value".to_string(),
        "file stopped".to_string(),
        "file-to-callback none".to_string(),
        "int-to-callback none".to_string(),
        "callback-to-int none".to_string(),
        "cross-layout error".to_string(),
        "lambda cross-layout none".to_string(),
        "value".to_string(),
        "7".to_string(),
        "19".to_string(),
        "20:1".to_string(),
        "8".to_string(),
        "9".to_string(),
        "11".to_string(),
        "12".to_string(),
        "13".to_string(),
        "14".to_string(),
        "15".to_string(),
        "none".to_string(),
        "none with progress".to_string(),
        "1".to_string(),
        "17".to_string(),
        "stopped".to_string(),
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

#[test]
fn reference_list_indices_are_checked_before_i32_narrowing() {
    for index in ["0 - 1", "4294967296"] {
        for body in [
            format!(
                "    let callbacks: List(fn(Int) -> Int) = [id]\n    let callback = list.at(callbacks, {index})\n    callback(1)\n"
            ),
            format!(
                "    var callbacks: List(fn(Int) -> Int) = [id]\n    list.set_at(callbacks, {index}, id)\n"
            ),
        ] {
            let source = format!(
                "fn id(value: Int) -> Int:\n    value\n\nfn main():\n{body}"
            );
            let module = parser::parse_module(&source).expect("parse");
            let linked =
                pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("reference-list bounds program typechecks");
            let interpreted = interpreter::run_module(
                linked.clone(),
                ".",
                Vec::new(),
            )
            .expect_err("the interpreter rejects an out-of-bounds reference-list index");
            assert!(
                interpreted.message.contains("out of bounds"),
                "interpreter diagnostic: {}",
                interpreted.message,
            );

            let wasm = codegen::compile_module_binary(&linked)
                .expect_lowered("reference-list bounds program lowers");
            let mut runtime = Runtime::batch().expect("runtime");
            let mut actor = runtime
                .spawn(&wasm, Capabilities::default(), 64)
                .expect("spawn");
            actor
                .run()
                .expect_err("compiled reference-list access must trap");
        }
    }
}

#[test]
fn empty_function_aggregate_paths_still_declare_an_indirect_call_table() {
    let source = r#"
fn option_call(value: Option(fn(Int) -> Int)) -> Int:
    match value:
        Some(callback) -> callback(1)
        None -> 0

fn result_call(value: Result(fn(Int) -> Int, String)) -> Int:
    match value:
        Ok(callback) -> callback(2)
        Err(_) -> 0

fn main(console: Console):
    console.print("${option_call(None)}")
    console.print("${result_call(Err("no callback"))}")
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("empty function aggregate paths typecheck");
    let expected = vec!["0".to_string(), "0".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );
    let wasm = codegen::compile_module_binary(&linked)
        .expect_lowered("indirect-call sites require an empty table");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            64,
        )
        .expect("spawn");
    actor.run().expect("compiled execution");
    assert_eq!(actor.output(), expected);
}

#[test]
fn public_pop_extracts_reference_elements_and_writes_back_the_list() {
    let source = r#"
import list

fn id(value: Int) -> Int:
    value + 1

fn main(console: Console):
    var callbacks: unique List(fn(Int) -> Int) = [id]
    match callbacks.pop():
        Some(callback) -> console.print("${callback(8)}")
        None -> console.print("none")
    console.print("${callbacks.length()}")
    match callbacks.pop():
        Some(callback) -> console.print("${callback(9)}")
        None -> console.print("empty")
"#;
    let linked = witchy::resolve_std_only(source).expect("link bundled list");
    typeck::check(&linked).expect("reference-list pop typechecks");
    let expected = vec!["9".to_string(), "0".to_string(), "empty".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );
    let wasm = codegen::compile_module_binary(&linked)
        .expect_lowered("reference-list pop lowers");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            64,
        )
        .expect("spawn");
    actor.run().expect("compiled execution");
    assert_eq!(actor.output(), expected);
}
