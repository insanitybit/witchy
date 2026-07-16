//! RFC-0090 reference-kind coverage: proper tail staging must preserve RFC-0005
//! externrefs and GC aggregate references without routing either through i64.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn proper_tail_calls_preserve_externref_and_gc_tuple_parameters() {
    let root = std::env::temp_dir().join(format!(
        "witchy_reference_tail_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "reference-tail").expect("seed file");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn descend_dir(dir: Dir[Read], name: String, n: Int) -> String:
    if n == 0:
        read_named(dir, name)
    else:
        descend_dir(dir, name, n - 1)

fn descend_pair(pair: (Dir[Read], String), n: Int) -> String:
    if n == 0:
        read_named(pair.0, pair.1)
    else:
        descend_pair(pair, n - 1)

fn left(pair: (Dir[Read], String), n: Int) -> String:
    if n == 0:
        read_named(pair.0, pair.1)
    else:
        right(pair, n - 1)

fn right(pair: (Dir[Read], String), n: Int) -> String:
    if n == 0:
        read_named(pair.0, pair.1)
    else:
        left(pair, n - 1)

fn main(console: Console, root: Dir[Read]):
    console.print(descend_dir(root, "value.txt", 300001))
    console.print(descend_pair((root, "value.txt"), 300001))
    console.print(left((root, "value.txt"), 300001))
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("reference-bearing tail calls typecheck");

    let expected = vec!["reference-tail".to_string(); 3];
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter callable loop",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("reference-bearing tail calls lower to WIR");
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
