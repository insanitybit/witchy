//! RFC-0005 regression coverage for nominal GC aggregate `let` patterns.
//! Capability fields must remain typed references rather than crossing i64 slots.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn capability_record_let_patterns_preserve_reference_fields() {
    let root = std::env::temp_dir().join(format!(
        "witchy_gc_let_pattern_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    let root_string = root.to_str().expect("UTF-8 temp path").to_string();

    let source = r#"
type DirBox:
    d: Dir
    tag: Int

fn main(console: Console, root: Dir):
    let db = DirBox(root, 41)
    let DirBox(d, tag) = db
    let rebuilt = DirBox(d, tag + 1)
    let DirBox(_, final_tag) = rebuilt
    console.print("${final_tag}")
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("capability record let pattern typechecks");

    let expected = vec!["42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        expected,
        "interpreter nominal record destructure",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("capability record let pattern lowers to WIR");
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
    assert_eq!(actor.output(), expected, "compiled nominal record destructure");

    let _ = std::fs::remove_dir_all(&root);
}
