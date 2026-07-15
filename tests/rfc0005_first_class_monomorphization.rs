//! RFC-0005 first-class monomorphization coverage. A named polymorphic function
//! value is specialized from its resolved concrete function type before WASM
//! closure lowering.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, opt, parser, pipeline, typeck};

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn named_polymorphic_function_values_specialize_across_value_flows() {
    let root = std::env::temp_dir().join(format!(
        "witchy_first_class_monomorphization_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "monomorphized").expect("seed file");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
import iter

type Holder:
    Holder(Dir[Read], String)

trait Numbered:
    fn number(self) -> Int

type NumberBox:
    NumberBox(Int)

impl Numbered for NumberBox:
    fn number(self) -> Int:
        match self:
            NumberBox(value) -> value

fn id(x: a) -> a:
    x

fn replace(var current: a, next: a) -> a:
    current = next
    current

fn read_number(value: a) -> Int where a: Numbered:
    value.number()

fn reader() -> fn(a) -> Int:
    read_number

fn second(ignore, x: a) -> a:
    x

fn f1() -> fn(a) -> a:
    id

fn f2() -> fn(a) -> a:
    f1()

fn f3() -> fn(a) -> a:
    f2()

fn f4() -> fn(a) -> a:
    f3()

fn f5() -> fn(a) -> a:
    f4()

fn f6() -> fn(a) -> a:
    f5()

fn apply_dir(f: fn(Dir[Read]) -> Dir[Read], dir: Dir[Read]) -> Dir[Read]:
    f(dir)

fn use_shadow(id: fn(Int) -> Int) -> Int:
    (id)(5000000004)

fn read_value(dir: Dir[Read]) -> String:
    dir.read("value.txt")

fn main(console: Console, root: Dir[Read]):
    let dir_id = id
    console.print(read_value(dir_id(root)))

    console.print(read_value((id)(root)))
    console.print(read_value(apply_dir(id, root)))

    let option_id = id
    match option_id(Some(root)):
        Some(dir) -> console.print(read_value(dir))
        None -> console.print("missing")

    let holder_id = id
    let Holder(dir, name) = holder_id(Holder(root, "value.txt"))
    console.print(dir.read(name))

    let int_id = id
    console.print(if int_id(5000000000) == 5000000000: "big-0" else: "truncated")

    var assigned = fn(x: Int) -> Int: x
    assigned = id
    console.print(if assigned(5000000001) == 5000000001: "big-1" else: "truncated")

    let joined = if true: id else: id
    console.print(if joined(5000000002) == 5000000002: "big-2" else: "truncated")

    let (pattern_id,) = (id,)
    console.print(if pattern_id(5000000003) == 5000000003: "big-3" else: "truncated")

    console.print(if use_shadow(fn(x: Int): x + 1) == 5000000005: "shadow" else: "rewritten")

    let bounded = read_number
    console.print(if bounded(NumberBox(17)) == 17: "bounded" else: "wrong-bound")

    let returned_bounded = reader()
    console.print(if returned_bounded(NumberBox(18)) == 18: "returned-bound" else: "wrong-bound")

    let second_value = second
    console.print(if second_value(true, 5000000006) == 5000000006: "untyped-param" else: "truncated")

    let deep = f6()
    console.print(if deep(5000000007) == 5000000007: "fixpoint" else: "truncated")

    let collect_ints: fn(iter.Iter(Int)) -> List(Int) = iter.collect
    let collected = collect_ints(iter.from_list([5000000008]))
    console.print(if list.at(collected, 0) == 5000000008: "result-bound" else: "truncated")

    let replace_dir = replace
    var current = root
    let returned = replace_dir(current, root)
    console.print(read_value(current))
    console.print(read_value(returned))

    let replace_int = replace
    var current_int = 0
    let returned_int = replace_int(current_int, 5000000009)
    console.print(if current_int == 5000000009 && returned_int == 5000000009: "scalar-var" else: "truncated")
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("polymorphic function values typecheck");

    let expected = vec![
        "monomorphized",
        "monomorphized",
        "monomorphized",
        "monomorphized",
        "monomorphized",
        "big-0",
        "big-1",
        "big-2",
        "big-3",
        "shadow",
        "bounded",
        "returned-bound",
        "untyped-param",
        "fixpoint",
        "result-bound",
        "monomorphized",
        "monomorphized",
        "scalar-var",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter polymorphic function values",
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
            .expect_lowered(&format!("{configuration}: generic function values lower to WIR"));
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
            "{configuration}: compiled polymorphic function values",
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
