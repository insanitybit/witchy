//! RFC-0005 typed closure ABI coverage. Reference-valued parameters and results
//! stay typed at indirect-call boundaries; closure environments remain scalar.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, opt, parser, pipeline, typeck};

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn function_values_preserve_externref_and_gc_tuple_signatures() {
    let root = std::env::temp_dir().join(format!(
        "witchy_typed_closure_abi_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "typed-closure").expect("seed file");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn keep_dir(dir: Dir[Read], n: Int) -> Dir[Read]:
    if n == 0:
        dir
    else:
        keep_dir(dir, n - 1)

fn apply_dir(f: fn(Dir[Read], Int) -> Dir[Read], dir: Dir[Read], n: Int) -> Dir[Read]:
    f(dir, n)

fn keep_pair(pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    if n == 0:
        pair
    else:
        keep_pair(pair, n - 1)

fn apply_pair(f: fn((Dir[Read], String), Int) -> (Dir[Read], String), pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    f(pair, n)

fn replace_dir(var current: Dir[Read], next: Dir[Read]) -> Dir[Read]:
    current = next
    current

fn replace_pair(var current: (Dir[Read], String), next: (Dir[Read], String)) -> (Dir[Read], String):
    current = next
    current

fn replace_dir_and_name(var current: Dir[Read], next: Dir[Read], name: String) -> String:
    current = next
    name

fn main(console: Console, root: Dir[Read]):
    let dir_fn = keep_dir
    let direct = apply_dir(dir_fn, root, 100001)
    console.print(read_named(direct, "value.txt"))

    let marker = 7
    let scalar_capture = fn(dir: Dir[Read], n: Int) -> Dir[Read]:
        if n == marker:
            return dir
        else:
            dir
    let captured = apply_dir(scalar_capture, root, marker)
    console.print(read_named(captured, "value.txt"))

    let pair_fn = keep_pair
    let pair = apply_pair(pair_fn, (root, "value.txt"), 100001)
    console.print(read_named(pair.0, pair.1))

    let inferred_fn = fn(x): x
    let inferred = inferred_fn(root)
    console.print(read_named(inferred, "value.txt"))

    let immediate = (fn(x): x)(root)
    console.print(read_named(immediate, "value.txt"))

    let replace_pair_fn = replace_pair
    var held_pair = (root, "value.txt")
    let replaced_pair = replace_pair_fn(held_pair, (root, "value.txt"))
    console.print(read_named(held_pair.0, held_pair.1))
    console.print(read_named(replaced_pair.0, replaced_pair.1))

    let replace = replace_dir
    var held = root
    let replaced = replace(held, root)
    console.print(read_named(held, "value.txt"))
    console.print(read_named(replaced, "value.txt"))

    let replace_named = replace_dir_and_name
    var named = root
    let name = replace_named(named, root, "value.txt")
    console.print(name)
    console.print(read_named(named, "value.txt"))
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typed reference closure signatures typecheck");

    let mut expected = vec!["typed-closure".to_string(); 9];
    expected.push("value.txt".to_string());
    expected.push("typed-closure".to_string());
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter function values",
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
            .expect_lowered(&format!("{configuration}: typed signatures lower to WIR"));
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
            "{configuration}: compiled typed closure calls",
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
