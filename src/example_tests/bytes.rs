use super::*;
use crate::{ast, codegen, interpreter, typeck};

    /// (RFC-0050) Bytes has a real inherent-method surface like the other
    /// standard value types; module functions remain callable for explicit
    /// module use and as first-class values.
    #[test]
    fn bytes_methods_cover_primary_surface_on_both_backends() {
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
        assert_eq!(link_run(src), expected, "interp: bytes methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: bytes methods",
        );
    }

    /// (parity, SEC-040) `bytes.slice` is BYTE-indexed and `bytes.to_string` is
    /// LOSSY on BOTH backends, while `bytes.decode_utf8` is strict. The compiled
    /// `bytes.slice` used to route through the CHAR-indexed `$str_substring` (so
    /// slicing a multibyte payload returned the wrong byte count — a
    /// binary-corruption primitive) and `bytes.to_string` was a raw identity (so
    /// invalid UTF-8 came back verbatim instead of the U+FFFD the interpreter's
    /// `from_utf8_lossy` produces). Both now match the byte-exact interpreter
    /// oracle. Same family as SEC-038 (the `bytes.at` OOB read).
    #[test]
    fn bytes_slice_is_byte_indexed_and_to_string_is_lossy() {
        // `é` is 2 UTF-8 bytes (0xC3 0xA9). Byte-slicing [0,1) yields ONE byte
        // (the interpreter's answer); the old char-indexed slice returned 2.
        let slice_src = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"héllo\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 1))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 1, 3))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 100))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 3, 1))}\")\n";
        // "héllo" = h(1) é(2) l(1) l(1) o(1) = 6 bytes. slice(0,1)=1, slice(1,3)=2
        // (the two bytes of é), slice(0,100) clamps to 6, slice(3,1) empty -> 0.
        let want_slice = ["1", "2", "6", "0"];
        assert_eq!(link_run(slice_src), want_slice, "interp bytes.slice is byte-indexed");
        assert_eq!(
            run_linked_on_wasm(&[("main", slice_src)], "main"),
            want_slice,
            "compiled bytes.slice must be byte-indexed too"
        );

        // Slicing `é` at [0,1) leaves a lone 0xC3 — invalid UTF-8. `to_string` must
        // lossily decode it to U+FFFD (3 bytes) on both backends, not return the
        // raw invalid byte.
        let lossy_src = "import bytes\n\nfn main(console: Console):\n    let half = bytes.slice(bytes.from_string(\"é\"), 0, 1)\n    let s = bytes.to_string_lossy(half)\n    console.print(\"${s.length()}\")\n    console.print(\"${bytes.length(bytes.from_string(s))}\")\n    match bytes.decode_utf8(half):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match bytes.decode_utf8(bytes.from_string(\"ok\")):\n        Ok(text) -> console.print(text)\n        Err(e) -> console.print(\"bad\")\n";
        // The lossy decode replaces the lone invalid byte with U+FFFD, which is 3
        // UTF-8 bytes (`string.length` is a BYTE count). The old buggy compiled
        // identity returned the single raw byte, so both readings would be "1".
        let want_lossy = ["3", "3", "bytes.decode_utf8: invalid UTF-8", "ok"];
        assert_eq!(link_run(lossy_src), want_lossy, "interp bytes.to_string is lossy");
        assert_eq!(
            run_linked_on_wasm(&[("main", lossy_src)], "main"),
            want_lossy,
            "compiled bytes.to_string must lossily decode to U+FFFD too"
        );
    }

    /// (BUG-392, parity) `bytes.slice` bounds are clamped in i64 on BOTH backends.
    /// The compiled `$bytes_slice` used to narrow `start`/`end` to i32 BEFORE
    /// clamping, so a large positive bound wrapped negative: `slice(b, 0, 2^31)`
    /// returned the FULL buffer on the interpreter (its `Int` clamp saw 2^31 > len)
    /// but an EMPTY slice compiled (2^31 truncated to a negative i32 clamped up to
    /// `lo`). Now both clamp the full `Int` first (like `$bytes_at`/`$list_at`).
    #[test]
    fn bytes_slice_clamps_bounds_in_i64_on_both_backends() {
        // "hello" = 5 bytes. Large positive `end` clamps to len (full buffer);
        // an out-of-i32-range `[start, end)` yields empty without wrapping into an
        // in-bounds slice; a large-magnitude negative `start` clamps up to 0.
        let src = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hello\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 2147483648))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 2147483648, 2147483649))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 0 - 2147483648, 2))}\")\n    console.print(bytes.to_string(bytes.slice(b, 1, 3)))\n";
        let expected = ["5", "0", "2", "el"];
        assert_eq!(link_run(src), expected, "interp clamps bytes.slice bounds in i64");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled bytes.slice must clamp bounds in i64 like the interpreter",
        );
    }

    /// (BUG-539) `Bytes` is ordinary core data, so it must participate in the
    /// public display and reflection protocols instead of being printable only by
    /// the interpreter's private `Value::Display` path. `Show` stays concise and
    /// non-lossy; reflection exposes raw byte values for debug/JSON consumers.
    #[test]
    fn bytes_are_showable_reflectable_and_renderable_on_both_backends() {
        let src = "import bytes\nimport show\nimport reflect\nimport json\n\ntype Packet derive(Reflect):\n    payload: Bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    show.say(console, b)\n    console.print(show.render([b]))\n    console.print(\"${b}\")\n    console.print(reflect.debug(b))\n    console.print(json.stringify(Packet(b)))\n";
        let expected = [
            "Bytes(len=2)",
            "[Bytes(len=2)]",
            "Bytes(len=2)",
            "[104, 105]",
            "{\"payload\":[104,105]}",
        ];
        assert_eq!(link_run(src), expected, "interp: Bytes protocols");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Bytes protocols");

        let raw = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${b}\")\n    console.print(\"${bytes.concat(b, b)}\")\n";
        let raw_expected = ["Bytes(len=2)", "Bytes(len=4)"];
        assert_eq!(link_run(raw), raw_expected, "interp: raw Bytes rendering");
        assert_eq!(run_linked_on_wasm(&[("main", raw)], "main"), raw_expected, "compiled: raw Bytes rendering");
    }

    /// (SEC-038) `bytes.at` out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$bytes_at` bounds-checks and traps
    /// (like `$list_at`), matching the interpreter's "bytes index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// parity divergence: the old lowering was an unchecked `load8_u`.)
    #[test]
    fn bytes_index_out_of_bounds_errors_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let oob = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 5)}\")\n";
        let (lmod, wasm) = compile(oob);
        assert!(
            interpreter::run_module(lmod, ".", Vec::new()).is_err(),
            "interpreter must error on OOB bytes index"
        );
        assert!(crate::run_wasm_bytes(&wasm).is_err(), "WASM must trap on OOB bytes index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 0 - 1)}\")\n";
        let (nmod, nwasm) = compile(neg);
        assert!(
            interpreter::run_module(nmod, ".", Vec::new()).is_err(),
            "interpreter must error on negative bytes index"
        );
        assert!(crate::run_wasm_bytes(&nwasm).is_err(), "WASM must trap on negative bytes index");
        // In-bounds indexing still agrees.
        let ok = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 2)}\")\n";
        let expected = ["33"];
        assert_eq!(link_run(ok), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "wasm");
    }

    /// (BUG-456) Encoding's canonical binary path is `Bytes`, not lossy `String`
    /// plumbing or hex detours. The payload includes `0xff`, so any accidental
    /// UTF-8 normalization changes the rendered byte list.
    #[test]
    fn encoding_bytes_codecs_round_trip_binary_on_both_backends() {
        let src = "import bytes\nimport encoding\nimport result\n\n\
                   fn main(console: Console):\n\
                   \x20   let raw = result.unwrap_or(encoding.hex_decode_bytes(\"4100ff2f\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(encoding.hex_encode_bytes(raw))\n\
                   \x20   console.print(encoding.base64_encode_bytes(raw))\n\
                   \x20   console.print(encoding.base64url_encode_bytes(raw))\n\
                   \x20   let from_b64 = result.unwrap_or(encoding.base64_decode_bytes(\"QQD/Lw==\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(\"${bytes.to_list(from_b64)}\")\n\
                   \x20   let from_url = result.unwrap_or(encoding.base64url_decode_bytes(\"QQD_Lw\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(\"${bytes.to_list(from_url)}\")\n\
                   \x20   match encoding.base64url_decode_bytes(\"QQD/Lw==\"):\n\
                   \x20       Ok(_) -> console.print(\"bad\")\n\
                   \x20       Err(_) -> console.print(\"err\")\n";
        let want = ["4100ff2f", "QQD/Lw==", "QQD_Lw", "[65, 0, 255, 47]", "[65, 0, 255, 47]", "err"];
        assert_eq!(link_run(src), want, "interpreter byte codecs must preserve binary");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            want,
            "WASM byte codecs must preserve binary"
        );
    }

    /// An empty needle is treated as ABSENT by `bytes.index_of`/`contains`,
    /// matching `string.index_of`'s module-wide empty-pattern rule so a
    /// String→Bytes port of the same search behaves identically. (Audit
    /// 2026-08-04: bytes used to return `Some(0)`/`true` for the empty needle.)
    #[test]
    fn bytes_empty_needle_is_absent_on_both_backends() {
        let src = "import bytes\n\n\
                   fn main(console: Console):\n\
                   \x20   let b = bytes.from_string(\"abc\")\n\
                   \x20   let e = bytes.from_string(\"\")\n\
                   \x20   console.print(\"${b.index_of(e)}\")\n\
                   \x20   console.print(\"${b.contains(e)}\")\n\
                   \x20   console.print(\"${b.index_of(bytes.from_string(\"bc\"))}\")\n";
        let want = ["None", "false", "Some(1)"];
        assert_eq!(link_run(src), want, "interpreter: empty needle is absent");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            want,
            "WASM: empty needle is absent",
        );
    }

    /// (RFC-0151) Bytes mutators (`push`, `append`, `reserve`, `set_at`, `swap`),
    /// constructors (`with_capacity`, `repeat`), aliasing, and self-append parity.
    #[test]
    fn bytes_mutators_and_constructors_parity_on_both_backends() {
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
        assert_eq!(link_run(src), expected, "interpreter: Bytes mutators");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "WASM: Bytes mutators",
        );
    }

    /// (RFC-0151) Negative capacity, invalid byte values, negative count/additional,
    /// and out-of-bounds indices abort on both backends.
    #[test]
    fn bytes_validation_and_bounds_abort_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };

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
            let (lmod, wasm) = compile(prog);
            assert!(
                interpreter::run_module(lmod, ".", Vec::new()).is_err(),
                "program {idx} must error on interpreter",
            );
            assert!(
                crate::run_wasm_bytes(&wasm).is_err(),
                "program {idx} must trap on WASM",
            );
        }
    }

    /// (RFC-0151) Positive WAT test proving direct byte operations (`i32.load8_u`, `i32.store8`)
    /// are emitted without helper calls to `$bytes_at`, `$bytes_set_at`, or `$bytes_swap`.
    #[test]
    fn bytes_direct_operations_emit_direct_wat_instructions() {
        let src = "import bytes\n\
                   fn main(console: Console):\n\
                   \x20   var b = bytes.from_string(\"ABC\")\n\
                   \x20   let x = b.at(1)\n\
                   \x20   b.set_at(0, 90)\n\
                   \x20   b.swap(1, 2)\n\
                   \x20   console.print(\"${x}\")\n\
                   \x20   console.print(b.to_string())\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let module = codegen::assemble_wir_module(&linked).expect_lowered("assemble wir");
        let wat = witchy_wir::wir::to_wat(&module);

        assert!(wat.contains("i32.load8_u"), "must contain direct i32.load8_u: {wat}");
        assert!(wat.contains("i32.store8"), "must contain direct i32.store8: {wat}");
        assert!(!wat.contains("call $bytes_at"), "must not call $bytes_at helper: {wat}");
        assert!(!wat.contains("call $bytes_set_at"), "must not call $bytes_set_at helper: {wat}");
        assert!(!wat.contains("call $bytes_swap"), "must not call $bytes_swap helper: {wat}");

        let expected = ["66", "ZCB"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

