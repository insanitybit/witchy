use super::*;
use crate::{ast, codegen, interpreter, typeck};

fn run_admitted_bytes(source: &str) -> Result<Vec<String>, String> {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { crate::opt::set_for_tests(None); }
    }
    crate::opt::set_for_tests(Some(crate::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let checked = witchy::resolve_std_only_checked(source).expect("check normal Bytes source");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit normal Bytes source");
    let mut runtime = crate::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        crate::runtime::Capabilities {
            print: true,
            quiet: true,
            ..crate::runtime::Capabilities::none()
        },
        128,
    ).expect("spawn admitted Bytes source");
    vm.run().map_err(|error| error.root_cause().to_string())?;
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    Ok(vm.output())
}

    /// (RFC-0050) Bytes has a real inherent-method surface like the other
    /// standard value types; module functions remain callable for explicit
    /// module use and as first-class values.
    #[test]
    fn bytes_methods_cover_primary_surface_on_admitted_wasm() {
        let src = "import bytes\nimport result\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${b.length()}\")\n    console.print(\"${b.at(0)}\")\n    console.print(\"${b.get(1).unwrap_or(0)}\")\n    console.print(\"${b.get(99).unwrap_or(0 - 1)}\")\n    console.print(b.to_string())\n    let c = b.concat(bytes.from_string(\"?\"))\n    console.print(c.to_string_lossy())\n    console.print(c.slice(1, 3).to_string())\n    console.print(\"${b.to_list()}\")\n    let raw = result.unwrap_or(bytes.from_list([0, 255, 65]), bytes.from_string(\"\"))\n    console.print(\"${raw.to_list()}\")\n    match raw.decode_utf8():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match raw.decode_utf8_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n    console.print(\"${b.is_empty()}\")\n    console.print(\"${c.index_of(bytes.from_string(\"i!\"))}\")\n    console.print(\"${c.index_of(bytes.from_string(\"zz\"))}\")\n    console.print(\"${c.contains(bytes.from_string(\"!?\"))}\")\n    console.print(\"${c.starts_with(b)}\")\n    console.print(\"${c.ends_with(bytes.from_string(\"!?\"))}\")\n    console.print(\"${bytes.length(b)}\")\n";
        let expected = [
            "3",
            "104",
            "105",
            "-1",
            "hi!",
            "hi!?",
            "i!",
            "[104, 105, 33]",
            "[0, 255, 65]",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.decode_utf8: invalid UTF-8",
            "false",
            "Some(1)",
            "None",
            "true",
            "true",
            "true",
            "3",
        ];
        assert_eq!(run_admitted_bytes(src).expect("run Bytes methods"), expected);
    }

    /// (SEC-038) `bytes.at` out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$bytes_at` bounds-checks and traps
    /// (like `$list_at`), matching the interpreter's "bytes index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// parity divergence: the old lowering was an unchecked `load8_u`.)
    #[test]
    fn bytes_index_out_of_bounds_errors_on_both_backends() {
        use crate::opt::{self, OptSet};
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        opt::set_for_tests(Some(OptSet::all()));
        let oob = include_str!("../../tests/fixtures/rfc0162-sec/sec038-bytes-positive-oob.witchy");
        let (lmod, wasm) = compile(oob);
        let interp_oob = interpreter::run_module(lmod, ".", Vec::new());
        let wasm_oob = crate::run_wasm_bytes(&wasm);
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = include_str!("../../tests/fixtures/rfc0162-sec/sec038-bytes-negative-oob.witchy");
        let (nmod, nwasm) = compile(neg);
        let interp_negative = interpreter::run_module(nmod, ".", Vec::new());
        let wasm_negative = crate::run_wasm_bytes(&nwasm);
        // In-bounds indexing still agrees.
        let ok = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 2)}\")\n";
        let interpreted_ok = link_run(ok);
        let wasm_ok = run_linked_on_wasm(&[("main", ok)], "main");
        opt::set_for_tests(None);

        let interp_error = interp_oob.expect_err("interpreter must error on OOB bytes index");
        assert!(
            interp_error.to_string().contains("bytes index 5 out of bounds (length 3)"),
            "interpreter violated the independent SEC-038 error oracle: {interp_error}"
        );
        let wasm_error = wasm_oob.expect_err("optimized WASM must trap on OOB bytes index");
        assert!(
            wasm_error.contains("bytes index 5 out of bounds (length 3)"),
            "optimized Wasm violated the independent SEC-038 error oracle: {wasm_error}"
        );
        let interp_error =
            interp_negative.expect_err("interpreter must error on negative bytes index");
        assert!(
            interp_error.to_string().contains("bytes index -1 out of bounds (length 3)"),
            "interpreter violated the independent SEC-038 negative error oracle: {interp_error}"
        );
        let wasm_error =
            wasm_negative.expect_err("optimized WASM must trap on negative bytes index");
        assert!(
            wasm_error.contains("bytes index -1 out of bounds (length 3)"),
            "optimized Wasm violated the independent SEC-038 negative error oracle: {wasm_error}"
        );
        let expected = ["33"];
        assert_eq!(interpreted_ok, expected, "interp");
        assert_eq!(wasm_ok, expected, "wasm");
    }

    /// (RFC-0151) Bytes mutators (`push`, `append`, `reserve`, `set_at`, `swap`),
    /// constructors (`with_capacity`, `repeat`), aliasing, and self-append.
    #[test]
    fn bytes_mutators_and_constructors_run_on_admitted_wasm() {
        let src = "import bytes\n\
                   fn main(console: Console):\n\
                   \x20   var b = bytes.with_capacity(32)\n\
                   \x20   console.print(\"${b.length()}\")\n\
                   \x20   b.push(65)\n\
                   \x20   b.push(66)\n\
                   \x20   b.push(67)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   let alias = b\n\
                   \x20   b.push(68)\n\
                   \x20   console.print(alias.to_string())\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   b.set_at(0, 90)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   b.swap(0, 3)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   b.swap(1, 1)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   b.reserve(100)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   b.append(b)\n\
                   \x20   console.print(b.to_string())\n\
                   \x20   let rep = bytes.repeat(42, 5)\n\
                   \x20   console.print(rep.to_string())\n\
                   \x20   var b2 = bytes.with_capacity(0)\n\
                   \x20   bytes.push(b2, 49)\n\
                   \x20   bytes.append(b2, bytes.from_string(\"23\"))\n\
                   \x20   bytes.reserve(b2, 10)\n\
                   \x20   bytes.set_at(b2, 1, 57)\n\
                   \x20   bytes.swap(b2, 0, 2)\n\
                   \x20   console.print(b2.to_string())\n";
        let expected = [
            "0",
            "ABC",
            "ABC",
            "ABCD",
            "ZBCD",
            "DBCZ",
            "DBCZ",
            "DBCZ",
            "DBCZDBCZ",
            "*****",
            "391",
        ];
        assert_eq!(run_admitted_bytes(src).expect("run Bytes mutators"), expected);
    }

    /// (RFC-0151) Negative capacity, invalid byte values, negative count/additional,
    /// and out-of-bounds indices abort on admitted Wasm.
    #[test]
    fn bytes_validation_and_bounds_abort_on_admitted_wasm() {
        if !is_nightly() {
            return;
        }
        let bad_programs = [
            // negative capacity in with_capacity
            "import bytes\nfn main(console: Console):\n    let b = bytes.with_capacity(0 - 1)\n",
            // invalid byte in repeat (< 0)
            "import bytes\nfn main(console: Console):\n    let b = bytes.repeat(0 - 1, 5)\n",
            // invalid byte in repeat (> 255)
            "import bytes\nfn main(console: Console):\n    let b = bytes.repeat(256, 5)\n",
            // negative count in repeat
            "import bytes\nfn main(console: Console):\n    let b = bytes.repeat(65, 0 - 1)\n",
            // invalid byte in push (< 0)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.push(0 - 1)\n",
            // invalid byte in push (> 255)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.push(300)\n",
            // negative additional in reserve
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.reserve(0 - 5)\n",
            // invalid byte in set_at (< 0)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.set_at(0, 0 - 1)\n",
            // invalid byte in set_at (> 255)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.set_at(0, 500)\n",
            // out-of-bounds index in set_at (negative)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.set_at(0 - 1, 65)\n",
            // out-of-bounds index in set_at (>= length)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"a\")\n    b.set_at(1, 65)\n",
            // out-of-bounds first index in swap (negative)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"ab\")\n    b.swap(0 - 1, 1)\n",
            // out-of-bounds first index in swap (>= length)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"ab\")\n    b.swap(2, 1)\n",
            // out-of-bounds second index in swap (negative)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"ab\")\n    b.swap(0, 0 - 1)\n",
            // out-of-bounds second index in swap (>= length)
            "import bytes\nfn main(console: Console):\n    var b = bytes.from_string(\"ab\")\n    b.swap(0, 2)\n",
        ];

        for (idx, prog) in bad_programs.iter().enumerate() {
            let error = run_admitted_bytes(prog).expect_err("invalid Bytes operation must trap");
            assert!(!error.is_empty(), "program {idx} must report its failure");
        }
    }
