use super::*;

    /// A large `Int` carried as an `Option`/`Result` success payload must keep its
    /// 64 bits on WASM, through both `?` and a `match`. The payload field is a type
    /// variable (generic i32 ABI), so codegen would truncate; it now tracks the
    /// declared scalar payload type and recovers `Some`/`Ok` values (and `?`
    /// results) at i64. (Regression for the big-Int-through-Option/Result gap.)
    #[test]
    fn wasm_big_int_through_result_payload_and_try() {
        let src = "fn fetch() -> Result(Int, String):\n    Ok(5000000000)\n\nfn chain() -> Result(Int, String):\n    let x = (fetch())?\n    Ok((x + 1))\n\nfn main(console: Console):\n    match chain():\n        Ok(v) -> console.print(\"${v}\")\n        Err(e) -> console.print(e)\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    #[test]
    fn big_int_arithmetic_backends_agree() {
        // Compiled Int is now i64, so arithmetic beyond the old 32-bit range
        // produces the independently calculated values without wrapping.
        let client = r#"
fn main(console: Console):
    let a = 3000000000
    let b = 4000000000
    console.print("${a + b}")
    console.print("${a * 3}")
    let big = 9000000000000
    console.print("${big}")
    console.print("${big / 1000}")
    console.print("${0 - big}")
"#;
        let sources = [("main", client)];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            compiled,
            vec![
                "7000000000",
                "9000000000",
                "9000000000000",
                "9000000000",
                "-9000000000000",
            ]
        );
    }
