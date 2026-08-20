//! RFC-0130 acceptance row 5: lazy adapter and `FromIterator` parity.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

fn compiled_output(module: &witchy::pipeline::CheckedModule) -> Vec<String> {
    let wasm = codegen::compile_checked_module_binary(module)
        .expect_lowered("compile RFC-0130 adapter and collection fixture");
    let mut runtime = Runtime::batch().expect("create RFC-0130 adapter runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0130 adapter fixture");
    actor.run().expect("run RFC-0130 adapter fixture");
    actor.output()
}

#[test]
fn rfc0130_row_5_iterator_adapters_and_from_iterator_collections_agree_on_both_backends() {
    let source = r#"
import iter

fn main(console: Console):
    let indexed: List((Int, Int)) = iter.collect(
        iter.from_list([1, 2, 3, 4])
            .filter_map(fn(n: Int) -> Option(Int): if n % 2 == 0: Some(n * 10) else: None)
            .chain(iter.once(50))
            .enumerate()
    )
    console.print("${indexed}")

    let by_name: Dict(String, Int) = iter.collect(
        iter.from_list(["a", "b", "c"]).zip(iter.range(10, 13))
    )
    console.print("${by_name}")

    let distinct: Set(Int) = iter.collect(
        iter.from_list([1, 2, 3, 4]).map(fn(n: Int): n % 3)
    )
    console.print("${distinct}")

    let joined: String = iter.collect(
        iter.from_list(["ad", "apt", "ers"]).map(fn(piece: String): piece)
    )
    console.print(joined)

    let bounded: List(Int) = iter.collect(
        iter.count_from(0)
            .drop(3)
            .filter(fn(n: Int): n % 2 == 1)
            .map(fn(n: Int): n * n)
            .take(4)
    )
    console.print("${bounded}")
    console.print("${iter.count_from(0).find(fn(n: Int): n == 8)}")
"#;
    let module = witchy::resolve_std_only_checked(source)
        .expect("check RFC-0130 adapter and collection fixture");
    let expected = vec![
        "[(0, 20), (1, 40), (2, 50)]".to_string(),
        "{a: 10, b: 11, c: 12}".to_string(),
        "{1, 2, 0}".to_string(),
        "adapters".to_string(),
        "[9, 25, 49, 81]".to_string(),
        "Some(8)".to_string(),
    ];

    assert_eq!(
        interpreter::run_checked_module(&module, ".", Vec::new())
            .expect("interpret RFC-0130 adapter fixture"),
        expected,
        "interpreter adapter and collection output",
    );
    assert_eq!(
        compiled_output(&module),
        expected,
        "compiled-Wasm adapter and collection output",
    );
}
