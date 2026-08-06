//! RFC-0090 reference-result coverage: proper tail dispatchers must return
//! RFC-0005 externrefs and GC aggregate references without integer-slot erasure.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn proper_tail_calls_preserve_reference_valued_results() {
    let root = std::env::temp_dir().join(format!(
        "witchy_reference_tail_results_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "reference-result").expect("seed file");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn descend_dir(dir: Dir[Read], n: Int) -> Dir[Read]:
    if n == 0:
        dir
    else:
        descend_dir(dir, n - 1)

fn left_pair(pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    if n == 0:
        pair
    else:
        right_pair(pair, n - 1)

fn right_pair(pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    if n == 0:
        pair
    else:
        left_pair(pair, n - 1)

fn main(console: Console, root: Dir[Read]):
    let direct_dir = descend_dir(root, 100001)
    console.print(read_named(direct_dir, "value.txt"))

    let mutual_pair = left_pair((root, "value.txt"), 100001)
    console.print(read_named(mutual_pair.0, mutual_pair.1))

"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("reference-valued tail calls typecheck");

    let expected = vec!["reference-result".to_string(); 2];
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter callable trampoline",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("reference-valued tail calls lower to WIR");
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
    assert_eq!(actor.output(), expected, "compiled typed tail dispatchers");

    let _ = std::fs::remove_dir_all(&root);
}
