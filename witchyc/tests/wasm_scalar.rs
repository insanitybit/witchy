//! Focused static checker coverage. These tests never invoke Lean or a runtime.
use witchy_two::wasm::compile;

// The CLI checks nested programs on a compiler worker, with a stack
// sized for the language's complexity limits.
fn with_compiler_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(witchy_two::compiler::COMPILER_STACK)
        .spawn(test)
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn rejects(source: &str) {
    assert!(compile(source).is_err(), "unexpected acceptance:\n{source}");
}
fn main_with(body: &str) -> String {
    format!("fn main(console: Console<Write>):\n{body}")
}

#[test]
fn real_frontend_emits_binary_wasm() {
    let bytes = compile(include_str!("../examples/wasm-scalar.wy")).unwrap();
    assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0");
    assert!(bytes.len() < 1024);
}
#[test]
fn all_selected_functions_and_unreachable_statements_are_checked() {
    rejects("fn bad() -> U64:\n    return false\nfn main():\n    return\n");
    rejects("fn main():\n    return\n    let wrong: Bool = 1\n");
    rejects("fn main():\n    if false:\n        memory.read(0, 0)\n");
}
#[test]
fn proof_bearing_syntax_fails_closed() {
    for source in [
        "lemma false_unused():\n    ensures false\n    proof:\n        auto\nfn main():\n    return\n",
        "fn unused() -> U64:\n    requires true\n    return 1\nfn main():\n    return\n",
        "fn unused() -> U64:\n    ensures result == 1\n    return 1\nfn main():\n    return\n",
        "fn main():\n    proof:\n        auto\n",
        "fn main():\n    assert true\n",
        "fn main():\n    let value = 1 by arithmetic.checked\n",
        "fn unused():\n    return\n    proof:\n        auto\nfn main():\n    return\n",
        "fn main():\n    while false:\n        invariant true\n        return\n",
        "predicate unused(x: U64):\n    x\nfn main():\n    return\n",
    ] {
        assert!(
            witchy_two::parser::parse(source).is_ok(),
            "proof control must reach the checker"
        );
        rejects(source);
    }
}
#[test]
fn unsupported_storage_and_unsafe_features_fail_closed() {
    for body in [
        "    let x = [1, false]\n",
        "    let x = memory.alloc(1)\n",
        "    let x = &1\n",
        "    let x = &mut 1\n",
        "    let x = move 1\n",
        "    let x = *1\n",
        "    let x = unsafe_read(1)\n",
        // I64 is supported since 2026-11-15; other signed widths are not.
        "    let x: I32 = 1\n",
        "    let x: USize = 1\n",
        // Text literals are values since 2026-11-14 (docs/decisions.md).
    ] {
        rejects(&main_with(body));
    }
    // Records with several allocations are proved, not refused, since 2026-11-16.
    rejects("fn leak() -> &U64:\n    let local = 1\n    return &local\nfn main():\n    return\n");
    rejects("fn identity<T>(x: T) -> T:\n    return x + x\nfn main():\n    return\n");
}
#[test]
fn output_authority_cannot_be_forged_copied_shadowed_or_forwarded() {
    for source in [
        "fn main():\n    console.print_int(1)\n",
        "fn main(console: U64):\n    console.print_int(1)\n",
        // `Console<Read>` is a capability of `main` since 2026-11-15; other rights are not.
        "fn main(console: Console<Exec>):\n    return\n",
        "fn main(console: Console<Write>):\n    let x = console\n",
        "fn main(console: Console<Write>):\n    let console = 1\n",
        "fn main(console: Console<Write>):\n    console = 1\n",
        "fn main(console: Console<Write>):\n    console.print_int(console)\n",
        "fn main(console: Console<Write>):\n    console.open(1)\n",
        "fn main(console: Console<Write>):\n    main()\n",
    ] {
        rejects(source);
    }
}
#[test]
fn scalar_type_mismatches_are_rejected() {
    for body in [
        "    let x: Bool = 1\n",
        "    let x: U64 = false\n",
        "    console.print_int(false)\n",
        "    if 1:\n        return\n",
        "    while 1:\n        return\n",
        "    let x = 1 + false\n",
        "    let x = true < false\n",
        "    let x = true == 1\n",
        "    let x = 1 && 2\n",
        "    let x = !1\n",
        "    let x = ()\n",
        "    let x = () == ()\n",
        "    return 1\n",
        "    var x = true\n    x += false\n",
    ] {
        rejects(&main_with(body));
    }
}
#[test]
fn call_signatures_and_returns_are_checked() {
    for source in [
        "fn f(x: U64) -> U64:\n    return x\nfn main():\n    f(true)\n",
        "fn f(x: U64) -> U64:\n    return x\nfn main():\n    f()\n",
        "fn f() -> U64:\n    return\nfn main():\n    return\n",
        "fn f(x: Bool) -> U64:\n    if x:\n        return 1\nfn main():\n    return\n",
        "fn f() -> U64:\n    while true:\n        return 1\nfn main():\n    return\n",
        "fn f(x: (), y: U64):\n    return\nfn main():\n    return\n",
        "fn main() -> U64:\n    return 1\n",
        "fn f():\n    return\n",
    ] {
        rejects(source);
    }
}
#[test]
fn scopes_and_initialization_are_checked() {
    for body in [
        "    let x = x\n",
        "    let x = 1\n    x = 2\n",
        "    var x = 1\n    x = false\n",
        "    let x = 1\n    let x = 2\n",
        "    if true:\n        let x = 1\n    console.print_int(x)\n",
        "    while false:\n        let x = 1\n    console.print_int(x)\n",
        "    if true:\n        let x = 1\n    else:\n        console.print_int(x)\n",
    ] {
        rejects(&main_with(body));
    }
    rejects("fn f(x: U64, x: U64):\n    return\nfn main():\n    return\n");
    rejects("fn main():\n    return\nfn main():\n    return\n");
}
#[test]
fn forward_calls_and_branch_returns_are_supported() {
    compile("fn main(console: Console<Write>):\n    console.print_int(f(true))\nfn f(b: Bool) -> U64:\n    if b:\n        return 1\n    else:\n        return 2\n").unwrap();
}
#[test]
fn deterministic_emission_and_source_limit() {
    let source = include_str!("../examples/wasm-scalar.wy");
    assert_eq!(compile(source).unwrap(), compile(source).unwrap());
    rejects(&" ".repeat(65_537));
}

#[test]
fn narrow_scalar_ranges_and_widths_are_checked() {
    for body in [
        "    let x: U8 = 256\n",
        "    let x: U16 = 65536\n",
        "    let x: U32 = 4294967296\n",
        "    let x: U8 = 1\n    let y: U16 = x\n",
        "    let x: U8 = 1\n    console.print_int(x)\n",
        "    let x: U8 = 1\n    let y: U16 = 1\n    let z = x + y\n",
        "    let x: U8 = 1\n    console.print_int(U16.to_u64(x))\n",
        "    let x: U8 = 1\n    let y = x == 256\n",
        "    let x: U8 = 1\n    let y = 256 + x\n",
        "    let U8 = 1\n    console.print_int(U8.to_u64(1))\n",
    ] {
        rejects(&main_with(body));
    }
    compile("fn inc(n: U8) -> U8:\n    return n + 1\nfn main(console: Console<Write>):\n    let n: U8 = 41\n    console.print_int(U8.to_u64(inc(n)))\n").unwrap();
}
#[test]
fn a_chain_of_operators_is_typed_once_at_each_level() {
    with_compiler_stack(|| {
        // Each level read both operands twice: 2^n steps for n operators (24 took seconds, 40
        // would not finish). 48 additions are now a few hundred steps.
        let started = std::time::Instant::now();
        let sum = vec!["y"; 48].join(" + ");
        compile(&main_with(&format!("    let y: U64 = 1\n    let x = {sum}\n    console.print_int(x)\n"))).unwrap();
        compile(&main_with(&format!("    let y: U64 = 1\n    if {sum} < 100:\n        console.print_int(y)\n"))).unwrap();
        let nested = (0..47).fold("y".to_string(), |inner, _| format!("y + ({inner})"));
        compile(&main_with(&format!("    let y: U64 = 1\n    let x: U64 = {nested}\n    console.print_int(x)\n"))).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    });
}
#[test]
fn expression_depth_is_bounded_where_it_is_read() {
    with_compiler_stack(|| {
        // Brackets (at most 128 deep) around chains (at most 128 long) multiplied: 60 groups of
        // 100 additions made a tree 6000 deep and the checker's recursion overflowed the stack.
        let mut deep = String::from("1");
        for _ in 0..60 {
            deep = format!("({deep}{})", " + 1".repeat(100));
        }
        let error = compile(&main_with(&format!("    let x: U64 = {deep}\n"))).unwrap_err();
        assert_eq!(error.code, "W0102");
        assert!(error.message.starts_with("expression nesting limit exceeded"), "{}", error.message);
        // Wide is not deep: many shallow operands and statements stay accepted.
        let wide = (0..40).map(|_| "(1 + 1)").collect::<Vec<_>>().join(" + ");
        compile(&main_with(&format!("    let x: U64 = {wide}\n    console.print_int(x)\n"))).unwrap();
    });
}
#[test]
fn pattern_depth_is_bounded_where_it_is_read() {
    with_compiler_stack(|| {
        // A pattern inside a pattern was read by plain recursion: `Some(Some(...` 100,000 deep
        // overflowed the stack. It now counts against the syntax nesting limit, as an expression does.
        let pattern = |depth: usize| format!("{}n{}", "Some(".repeat(depth), ")".repeat(depth));
        let source = |depth: usize| format!("fn f(value: U64) -> U64:\n    match value:\n        {}:\n            return 1\n        _:\n            return 0\nfn main():\n    return\n", pattern(depth));
        for depth in [200, 100_000] {
            let error = compile(&source(depth)).unwrap_err();
            assert_eq!(error.code, "W0102", "{depth}: {}", error.message);
            assert!(error.message.starts_with("syntax nesting limit exceeded"), "{depth}: {}", error.message);
        }
        // Three deep is still read (tests/fixtures/match/nested.wy runs one).
        compile("type Option<T>:\n    None\n    Some(T)\n\nfn depth(value: Option<Option<Option<U64>>>) -> U64:\n    match value:\n        Some(Some(Some(n))):\n            return n\n        _:\n            return 0\nfn main(console: Console<Write>):\n    console.print_int(depth(Some(Some(Some(42)))))\n").unwrap();
    });
}
#[test]
fn target_nesting_is_bounded_before_emission() {
    with_compiler_stack(|| {
        let mut source = String::from("fn main():\n");
        for depth in 1..62 {
            source.push_str(&format!("{}while true:\n", "    ".repeat(depth)));
        }
        source.push_str(&format!("{}return\n", "    ".repeat(62)));
        rejects(&source);
    });
}

#[test]
fn aggregate_shapes_types_and_ownership_boundaries_are_checked() {
    for source in [
        "type R:\n    x: U64\n    x: Bool\nfn main():\n    return\n",
        "type R:\n    x: R\nfn main():\n    return\n",
        "type R:\n    x: Missing\nfn main():\n    return\n",
        "type R:\n    x: U64\n    invariant valid(x)\nfn main():\n    return\n",
        "type R:\n    x: ()\nfn main():\n    return\n",
        "type R:\n    x: U64\nfn main():\n    let r = R(true)\n",
        "type R:\n    x: U64\nfn main():\n    let r = R()\n",
        "type R:\n    x: U64\nfn main():\n    let r = R(1)\n    r.x = 2\n",
        "type R:\n    x: U64\nfn main():\n    var r = R(1)\n    r.x = false\n",
        "type R:\n    x: U64\nfn main():\n    let r = R(1)\n    r.missing\n",
        "type R:\n    x: U64\ntype S:\n    x: U64\nfn f(r: R):\n    return\nfn main():\n    f(S(1))\n",
        "fn main():\n    let x = []\n",
        "fn main():\n    let x = [1; 0]\n",
        "fn main():\n    let x = [1; 257]\n",
        "fn main():\n    let x: [U8; 2] = [1]\n",
        "fn main():\n    let x: [U8; 2] = [1, 256]\n",
        "fn main():\n    let x = [1, true]\n",
        "fn main():\n    let x = [1]\n    x[true]\n",
        "fn main():\n    let x = [1]\n    x[0] = 2\n",
        "fn main():\n    let x = [1]\n    let y = x == x\n",
    ] {
        rejects(source);
    }
    compile(include_str!("../examples/wasm-aggregates.wy")).unwrap();
}

#[test]
fn indexed_assignment_preserves_type_and_mutability_checks() {
    for body in [
        "    let values = [1]\n    values[0] = 2\n",
        "    var values = [1]\n    values[false] = 2\n",
        "    var values: [U8; 1] = [1]\n    values[0] = 256\n",
        "    var values = [1]\n    values[0] = false\n",
        "    var values = [true]\n    values[0] += false\n",
    ] {
        rejects(&main_with(body));
    }
    rejects("fn f(values: [U64; 1]):\n    values[0] = 2\nfn main():\n    return\n");
    compile(include_str!("../examples/wasm-sort.wy")).unwrap();
}

#[test]
fn scalarized_shape_and_abi_limits_are_checked_even_when_unused() {
    rejects("fn unused(x: [U64; 32], extra: U64):\n    return\nfn main():\n    return\n");
    rejects("type TooBig:\n    values: [U64; 32]\n    extra: U8\nfn main():\n    return\n");
    // Since 2026-11-15 a module holds 4096 functions (6 helpers here) and only what `main`
    // reaches counts; 64 callers of up to 63 functions each keep every body small.
    let reached = |n: usize| {
        let mut source = (0..n).map(|i| format!("fn f{i}():\n    return\n")).collect::<String>();
        for g in 0..64 {
            source.push_str(&format!("fn g{g}():\n"));
            for i in g * 63..((g + 1) * 63).min(n) {
                source.push_str(&format!("    f{i}()\n"));
            }
        }
        source.push_str("fn main():\n");
        for g in 0..64 {
            source.push_str(&format!("    g{g}()\n"));
        }
        source
    };
    compile(&reached(4025)).unwrap();
    let error = compile(&reached(4026)).unwrap_err();
    assert!(error.message.contains("more than the 4096 functions"), "{error:?}");
    // The source limit is 1 MiB since 2026-11-15.
    let mut exact = String::from("fn main():\n    return\n//");
    exact.push_str(&"x".repeat((1 << 20) - exact.len()));
    compile(&exact).unwrap();
    exact.push('x');
    rejects(&exact);
    rejects(&main_with(
        "    let small: U8 = 1\n    let large: U16 = 2\n    let values = [small, large]\n",
    ));
}

#[test]
fn generic_copyable_records_use_concrete_context_and_the_existing_abi() {
    let source = "type Box<T>:\n    value: T\ntype Pair<A, B>:\n    first: A\n    second: B\nfn make(x: U8) -> Box<U8>:\n    return Box(x)\nfn update(x: Pair<Box<U8>, [U16; 2]>) -> Pair<Box<U8>, [U16; 2]>:\n    var y = x\n    y.first.value = 8\n    y.second[1] = 300\n    return y\nfn main(console: Console<Write>):\n    let p: Pair<Box<U8>, [U16; 2]> = Pair(make(3), [10, 20])\n    let q = update(p)\n    console.print_int(U8.to_u64(q.first.value))\n    console.print_int(U16.to_u64(q.second[1]))\n    console.print_int(U8.to_u64(p.first.value))\n";
    let (bytes, receipt) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(bytes.starts_with(b"\0asm\x01\0\0\0"));
    assert!(receipt.contains("record:Box<U8>"));
    assert!(receipt.contains("record:Pair<"));
    assert_eq!(bytes, compile(source).unwrap());

    // Nested use of the same template is finite and concrete, not recursive layout.
    compile("type Box<T>:\n    value: T\nfn main():\n    let x: Box<Box<U8>> = Box(Box(7))\n    let y: [Box<U8>; 2] = [Box(1), Box(2)]\n").unwrap();
}

#[test]
fn generic_record_identity_is_nominal_and_width_sensitive() {
    let prefix = "type Box<T>:\n    value: T\ntype Other<T>:\n    value: T\n";
    for suffix in [
        "fn main():\n    let x = Box(1)\n", // unsupported unconstrained inference
        "fn main():\n    let x: Box<U8> = Box(256)\n",
        "fn main():\n    let n: U16 = 1\n    let x: Box<U8> = Box(n)\n",
        "fn main():\n    let x: Box<U8> = Other(1)\n",
        "fn main():\n    let x: Box<U8> = Box(1)\n    let y: Box<U16> = x\n",
        "fn main():\n    let x: Box = Box(1)\n",
        "fn main():\n    let x: Box<U8, U16> = Box(1)\n",
        "fn main():\n    let x: Box<()> = Box(())\n",
        "fn Box():\n    return\nfn main():\n    return\n",
        "fn id<T>(x: T) -> T:\n    return x + x\nfn main():\n    return\n",
    ] {
        rejects(&format!("{prefix}{suffix}"));
    }
    // Legal since 2026-11-15: a record may hold `&` references and capabilities.
    for source in [
        "type R:\n    x: &U64\nfn main():\n    return\n",
        "type R:\n    x: Console<Write>\nfn main():\n    return\n",
    ] {
        witchy_two::wasm::compile_report(source).unwrap();
    }
}

#[test]
fn every_unused_generic_template_is_validated_without_specializing_it() {
    for template in [
        "type Bad<T, T>:\n    value: T\n",
        "type Bad<A, B, C, D, E, F, G, H, I>:\n    value: A\n",
        "type Bad<U8>:\n    value: U8\n",
        "type Bad<T>:\n    x: T\n    x: U8\n",
        "type Bad<T>:\n    value: Missing\n",
        "type Bad<T>:\n    value: T<U8>\n",
        "type Bad<T>:\n    value: U8<T>\n",
        "type Bad<T>:\n    value: [T; 0]\n",
        "type Bad<T>:\n    value: ()\n",
        // `value: &T` (2026-11-15, a record may hold `&` references) and
        // `value: Console<Write>` (2026-11-15, a record may hold a capability) are legal.
        "type Bad<T>:\n    value: Bad<T>\n",
        "type Bad<T>:\n    value: Other<T>\ntype Other<T>:\n    value: Bad<T>\n",
        "type Bad<T>:\n    value: Other<T, T>\ntype Other<T>:\n    value: T\n",
        "type Bad<T>:\n    value: [U64; 32]\n    extra: T\n",
        "type Bad<T>:\n    value: T\n    invariant valid(value)\n",
        "type T:\n    value: U8\ntype Bad<T>:\n    value: T\n",
        "type U8<T>:\n    value: T\n",
    ] {
        let source = format!("{template}fn main():\n    return\n");
        assert!(witchy_two::parser::parse(&source).is_ok(), "control must reach checker: {source}");
        rejects(&source);
    }
    let plain = "fn main():\n    return\n";
    let unused = format!("type Box<T>:\n    value: T\n{plain}");
    let (bytes, receipt) = witchy_two::wasm::compile_report(&unused).unwrap();
    assert_eq!(bytes, compile(plain).unwrap());
    assert!(receipt.contains("\"selected_types\":[\"Box\"]"));
    assert!(receipt.contains("\"aggregate_layouts\":[]"));
}

#[test]
fn generic_specializations_are_canonical_and_bounded() {
    let source = "type Box<T>:\n    value: T\nfn f(x: Box<U8>) -> Box<U8>:\n    return x\nfn main():\n    let a: Box<U8> = Box(1)\n    let b: Box<U8> = f(Box(2))\n";
    let (_, receipt) = witchy_two::wasm::compile_report(source).unwrap();
    assert_eq!(receipt.matches("record:Box<U8>").count(), 1);

    compile("type Pair<T>:\n    x: T\n    y: T\nfn main():\n    let p: Pair<[U8; 16]> = Pair([1; 16], [2; 16])\n").unwrap();
    rejects("type Pair<T>:\n    x: T\n    y: T\nfn main():\n    let p: Pair<[U8; 17]> = Pair([1; 17], [2; 17])\n");

    // 64 distinct array arguments and their Boxes are 128 types, which was once every type a
    // program could have; one more is as ordinary as the rest now (the limit, 4096, is
    // checked where it is enforced: layout.rs).
    let mut source = String::from("type Box<T>:\n    value: T\n");
    for (width, ty) in ["U8", "U16"].iter().enumerate() {
        for length in 1..=32 {
            source.push_str(&format!("fn f{width}_{length}(x: Box<[{ty}; {length}]>):\n    return\n"));
        }
    }
    let nested = format!("{}U8{}", "Box<".repeat(17), ">".repeat(17));
    rejects(&format!("type Box<T>:\n    value: T\nfn unused(x: {nested}):\n    return\nfn main():\n    return\n"));
    let main = "fn main():\n    return\n";
    compile(&format!("{source}{main}")).unwrap();
    compile(&format!("{source}fn extra(x: Box<[U32; 1]>):\n    return\n{main}")).unwrap();
}

#[test]
fn scalar_borrows_forward_with_call_scoped_permissions() {
    let source = "fn read(x: &U64) -> U64:\n    return *x\nfn put(x: &mut U64):\n    *x += 1\nfn forward(x: &mut U64) -> U64:\n    put(x)\n    put(&mut *x)\n    return read(&*x)\nfn both(a: &U64, b: &U64) -> U64:\n    return *a + *b\nfn param(x: U64) -> U64:\n    return read(&x)\nfn main(console: Console<Write>):\n    var x = 40\n    console.print_int(forward(&mut x))\n    console.print_int(both(&x, &x))\n    console.print_int(param(x))\n";
    let (bytes, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(bytes.starts_with(b"\0asm\x01\0\0\0"));
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
    // The default stack is 16 KiB since 2026-11-18: [66560 - 16384, 66560).
    assert!(report.contains("\"stack_low\":50176,\"stack_high\":66560,\"stack_alignment\":8"));
    assert!(report.contains("\"allocator\":false,\"memory_exported\":true"));
}

#[test]
fn scalar_borrows_preserve_packed_widths_and_bool_context() {
    compile("fn u8(x: &mut U8):\n    *x += 1\nfn u16(x: &mut U16):\n    *x += 1\nfn u32(x: &mut U32):\n    *x += 1\nfn u64(x: &mut U64):\n    *x += 1\nfn flip(x: &mut Bool):\n    *x = !*x\nfn narrow(x: &U8) -> U8:\n    return *x + 1\nfn main(console: Console<Write>):\n    var a: U8 = 254\n    var b: U16 = 65534\n    var c: U32 = 4294967294\n    var d: U64 = 18446744073709551614\n    var e = true\n    u8(&mut a)\n    u16(&mut b)\n    u32(&mut c)\n    u64(&mut d)\n    flip(&mut e)\n    console.print_int(U8.to_u64(a))\n    console.print_int(U16.to_u64(b))\n    console.print_int(U32.to_u64(c))\n    console.print_int(d)\n    if !e:\n        console.print_int(1)\n    let n: U8 = 10\n    console.print_int(U8.to_u64(narrow(&n)))\n").unwrap();
}

#[test]
fn root_loans_cover_all_later_arguments_and_nested_calls() {
    let helpers = "fn two(a: &mut U64, b: &mut U64):\n    return\nfn mixed(a: &mut U64, b: &U64):\n    return\nfn value(a: &mut U64, b: U64):\n    return\nfn shared(a: &U64, b: U64):\n    return\nfn change(a: &mut U64) -> U64:\n    *a += 1\n    return *a\nfn read(a: &U64) -> U64:\n    return *a\nfn lend(a: &mut U64) -> &mut U64:\n    return a\nfn view(a: &U64) -> &U64:\n    return a\n";
    // Since 2026-11-15 a reference argument that runs nothing is taken after the other
    // arguments (docs/decisions.md); a reference a call makes is lent while they run.
    for body in [
        "    var x = 1\n    two(&mut x, &mut x)\n",
        "    var x = 1\n    mixed(&mut x, &x)\n",
        "    var x = 1\n    mixed(lend(&mut x), &x)\n",
        "    var x = 1\n    value(lend(&mut x), x)\n",
        "    var x = 1\n    value(lend(&mut x), read(&x))\n",
        "    var x = 1\n    shared(view(&x), change(&mut x))\n",
    ] {
        let source = format!("{helpers}fn main():\n{body}");
        assert!(witchy_two::parser::parse(&source).is_ok());
        rejects(&source);
    }
    for body in [
        "    two(p, p)\n",
        "    value(lend(p), read(p))\n",
        "    value(lend(p), *p)\n",
        "    shared(view(p), change(p))\n",
        "    mixed(p, &*p)\n",
    ] {
        rejects(&format!("{helpers}fn outer(p: &mut U64):\n{body}fn main():\n    return\n"));
    }
    // The first nested call has finished before the later shared loan begins.
    compile(&format!("{helpers}fn plain(a: U64, b: &U64):\n    return\nfn main():\n    var x = 1\n    plain(change(&mut x), &x)\n    shared(&x, read(&x))\n")).unwrap();
    compile(&format!("{helpers}fn main():\n    var x = 1\n    value(&mut x, x)\n    value(&mut x, read(&x))\n    shared(&x, change(&mut x))\n")).unwrap();
    compile(&format!("{helpers}fn outer(p: &mut U64):\n    value(p, read(p))\n    value(p, *p)\n    shared(p, change(p))\nfn main():\n    return\n")).unwrap();
}

#[test]
fn borrow_escape_storage_wrong_permission_and_unused_bad_code_refuse() {
    for source in [
        "fn bad(p: &U64):\n    *p = 1\nfn main():\n    return\n",
        "fn bad(p: &U64):\n    change(p)\nfn change(p: &mut U64):\n    return\nfn main():\n    return\n",
        // `&mut` references are values since 2026-11-15; a reborrow still excludes its source.
        "fn bad(p: &mut U64):\n    let q = p\n    *p = 1\n    *q = 2\nfn main():\n    return\n",
        "fn put(p: &mut U64):\n    return\nfn bad(x: U64):\n    put(&mut x)\nfn main():\n    return\n",
        // Returning a `&` parameter is allowed (2026-11-14); returning a local's address is not.
        "fn bad(p: &U64) -> &U64:\n    let x = *p\n    return &x\nfn main():\n    return\n",
        "type Box<T>:\n    value: T\nfn bad(p: Box<&U8>):\n    return\nfn main():\n    return\n",
        // Shared references are values since 2026-11-14; the referent stays frozen while used.
        "fn read(p: &U64):\n    return\nfn main():\n    var x = 1\n    let p = &x\n    x = 2\n    read(p)\n",
        "fn put(p: &mut U64):\n    return\nfn main():\n    let x = 1\n    put(&mut x)\n",
        "fn put(p: &mut U64):\n    return\nfn main():\n    var x: U8 = 1\n    put(&mut x)\n",
        "fn bad(p: &mut U64):\n    bad(&mut p)\nfn main():\n    return\n",
        "fn main():\n    memory.read(0, 1)\n",
    ] {
        assert!(witchy_two::parser::parse(source).is_ok(), "control must reach checker: {source}");
        rejects(source);
    }
    let source = "lemma okay(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn two(a: &mut U64, b: &mut U64):\n    return\nfn main():\n    var x = 1\n    two(&mut x, &mut x)\n";
    let error = compile(source).unwrap_err();
    assert!(error.message.contains("overlapping unique"), "borrow failure must precede any Lean resolution: {error:?}");
}

// Section inventory, not instruction-substring matching: private memory/SP must
// exist only in frame modules and never become exported entry parameters.
fn wasm_section_payload(bytes: &[u8], wanted: u8) -> Option<&[u8]> {
    let mut cursor = 8;
    while cursor < bytes.len() {
        let id = bytes[cursor];
        cursor += 1;
        let mut length = 0usize;
        let mut shift = 0;
        loop {
            let byte = bytes[cursor];
            cursor += 1;
            length |= ((byte & 127) as usize) << shift;
            if byte & 128 == 0 { break; }
            shift += 7;
        }
        let payload = &bytes[cursor..cursor + length];
        if id == wanted { return Some(payload); }
        cursor += length;
    }
    None
}

#[test]
fn frame_profile_is_exact_and_zero_borrow_modules_stay_memory_free() {
    let plain = "fn main():\n    return\n";
    let (bytes, report) = witchy_two::wasm::compile_report(plain).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    assert!(wasm_section_payload(&bytes, 6).is_none());
    assert!(!report.contains("runtime_profile"));
    assert!(!report.contains("frame_memory"));
    // Only functions `main` reaches are emitted (2026-11-15), so an unused borrowed function
    // adds no frame; a reached one selects the closed frame profile.
    let (bytes, _) = witchy_two::wasm::compile_report(&format!("fn unused(p: &U8):\n    return\n{plain}")).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    let (bytes, report) = witchy_two::wasm::compile_report("fn used(p: &U8):\n    return\nfn main():\n    let x: U8 = 1\n    used(&x)\n").unwrap();
    assert_eq!(wasm_section_payload(&bytes, 5).unwrap(), &[1, 1, 2, 2]);
    assert_eq!(wasm_section_payload(&bytes, 6).unwrap(), &[1, 0x7f, 1, 0x41, 0x80, 0x88, 4, 0x0b]);
    // Memory 0 is exported for bulk host I/O since 2026-11-17; the stack pointer is not.
    assert_eq!(wasm_section_payload(&bytes, 7).unwrap(), &[2, 3, b'r', b'u', b'n', 0, 0, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0]);
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
}

#[test]
fn recursive_and_oversized_frames_are_runtime_guarded_before_writes() {
    compile("fn recurse(p: &U64):\n    let next = *p + 1\n    recurse(&next)\nfn main(console: Console<Write>):\n    console.print_int(7)\n    let x = 0\n    recurse(&x)\n").unwrap();
    let mut source = String::from("fn read(p: &U64) -> U64:\n    return *p\nfn huge():\n");
    for index in 0..129 {
        source.push_str(&format!("    let x{index}: U64 = {index}\n    read(&x{index})\n"));
    }
    source.push_str("fn main():\n    huge()\n");
    // 1032-byte frame: accepted source, guarded runtime refusal rather than a
    // partial allocation/store. Engine execution and sentinel checks belong to
    // the separately coordinated host controls.
    let (_, report) = witchy_two::wasm::compile_report(&source).unwrap();
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
}

#[test]
fn generic_aggregate_references_use_common_typed_places_and_copy_abi() {
    let source = "type Inner<T>:\n    wide: U64\n    small: T\ntype Outer<T>:\n    inner: Inner<T>\n    tail: T\nfn edit(p: &mut Outer<U8>):\n    (*p).inner.small = 9\n    (*p).tail = 7\nfn forward(p: &mut Outer<U8>):\n    edit(p)\nfn own(p: Outer<U8>) -> Outer<U8>:\n    var copy = p\n    edit(&mut copy)\n    return copy\nfn set(p: &mut [U8; 3], i: U64, value: U8):\n    (*p)[i] = value\nfn get(p: &[U8; 3], i: U64) -> U8:\n    return (*p)[i]\nfn increment(p: &mut U8):\n    *p += 1\nfn main(console: Console<Write>):\n    var outer: Outer<U8> = Outer(Inner(100, 2), 3)\n    let original = outer\n    forward(&mut outer)\n    increment(&mut outer.inner.small)\n    let result = own(original)\n    console.print_int(U8.to_u64(outer.inner.small))\n    console.print_int(U8.to_u64(outer.tail))\n    console.print_int(U8.to_u64(original.inner.small))\n    console.print_int(U8.to_u64(result.inner.small))\n    var xs: [U8; 3] = [1, 2, 3]\n    set(&mut xs, 1, 9)\n    increment(&mut xs[1])\n    console.print_int(U8.to_u64(get(&xs, 1)))\n";
    let (_, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
    assert!(report.contains("record:Outer<U8>\",\"storage_bytes\":24"));
    // Former scalar-slice refusals are now supported, including unused arrays.
    compile("type R:\n    x: U8\nfn read(p: &U8):\n    return\nfn unused(p: &[U64; 2]):\n    return\nfn main():\n    let r = R(1)\n    read(&r.x)\n    var xs: [U8; 2] = [1, 2]\n    read(&xs[0])\n").unwrap();
}

#[test]
fn whole_aggregate_assignment_captures_overlapping_rhs_before_stores() {
    compile("type Pair<T>:\n    first: T\n    second: T\nfn swap(p: &mut Pair<U64>):\n    *p = Pair((*p).second, (*p).first)\nfn main(console: Console<Write>):\n    var p: Pair<U64> = Pair(1, 2)\n    swap(&mut p)\n    console.print_int(p.first)\n    console.print_int(p.second)\n").unwrap();
    // A fixed array indexed in place lives in the frame since 2026-11-18.
    let (bytes, report) = witchy_two::wasm::compile_report("fn main():\n    var values = [[1, 2], [3, 4]]\n    values[0][1] = 9\n    values[1][0] += 1\n").unwrap();
    assert_eq!(wasm_section_payload(&bytes, 5).unwrap(), &[1, 1, 2, 2]);
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
}

#[test]
fn projected_loans_keep_the_whole_canonical_root() {
    let prefix = "type Pair<T>:\n    left: T\n    right: T\nfn use(a: &mut U64, b: U64):\n    return\nfn two(a: &mut U64, b: &mut U64):\n    return\nfn read(a: &U64) -> U64:\n    return *a\nfn change(a: &mut U64) -> U64:\n    *a += 1\n    return *a\nfn share(a: &U64, b: U64):\n    return\nfn lend(a: &mut U64) -> &mut U64:\n    return a\nfn view(a: &U64) -> &U64:\n    return a\nfn whole(a: &mut Pair<U64>, b: &mut U64):\n    return\n";
    // Since 2026-11-16 a loan covers the field path (or literal array index) it was taken
    // through, so disjoint fields may be lent at once; overlapping paths still conflict.
    for source in [
        "fn main():\n    var p: Pair<U64> = Pair(1, 2)\n    use(lend(&mut p.left), p.left)\n",
        "fn main():\n    var p: Pair<U64> = Pair(1, 2)\n    two(&mut p.left, &mut p.left)\n",
        "fn main():\n    var p: Pair<U64> = Pair(1, 2)\n    whole(&mut p, &mut p.right)\n",
        "fn main():\n    var xs = [1, 2]\n    two(&mut xs[0], &mut xs[0])\n",
        "fn main():\n    var xs = [1, 2]\n    var i = 0\n    two(&mut xs[i], &mut xs[1])\n",
        "fn outer(p: &mut Pair<U64>):\n    use(lend(&mut (*p).left), (*p).left)\nfn main():\n    return\n",
        "fn outer(p: &mut Pair<U64>):\n    use(lend(&mut (*p).left), read(&(*p).left))\nfn main():\n    return\n",
        "fn outer(p: &mut Pair<U64>):\n    share(view(&(*p).left), change(&mut (*p).left))\nfn main():\n    return\n",
    ] {
        let source = format!("{prefix}{source}");
        assert!(witchy_two::parser::parse(&source).is_ok());
        rejects(&source);
    }
    compile(&format!("{prefix}fn main():\n    var a: Pair<U64> = Pair(1, 2)\n    var b: Pair<U64> = Pair(3, 4)\n    two(&mut a.left, &mut b.right)\n    use(&mut a.left, b.right)\n")).unwrap();
    compile(&format!("{prefix}fn main():\n    var p: Pair<U64> = Pair(1, 2)\n    use(&mut p.left, p.right)\n    two(&mut p.left, &mut p.right)\n    var xs = [1, 2]\n    two(&mut xs[0], &mut xs[1])\n")).unwrap();
    let error = compile(&format!("lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\n{prefix}fn main():\n    var p: Pair<U64> = Pair(1, 2)\n    use(lend(&mut p.left), p.left)\n")).unwrap_err();
    assert!(error.message.contains("a reference into it is still in use"), "{error:?}");
}

#[test]
fn aggregate_borrows_reject_permission_identity_and_reference_storage_errors() {
    for source in [
        "type R:\n    x: U8\nfn put(p: &mut R):\n    return\nfn bad(p: R):\n    put(&mut p)\nfn main():\n    return\n",
        "type R:\n    x: U8\nfn bad(p: &R):\n    (*p).x = 1\nfn main():\n    return\n",
        "type Box<T>:\n    value: T\nfn put(p: &mut Box<U8>):\n    return\nfn main():\n    var p: Box<U16> = Box(1)\n    put(&mut p)\n",
        "type R:\n    x: U8\ntype S:\n    x: U8\nfn put(p: &mut R):\n    return\nfn main():\n    var p = S(1)\n    put(&mut p)\n",
        "fn put(p: &mut [U8; 2]):\n    return\nfn main():\n    var xs: [U8; 3] = [1, 2, 3]\n    put(&mut xs)\n",
        "type R:\n    x: U8\ntype Box<T>:\n    value: T\nfn bad(p: Box<&R>):\n    return\nfn main():\n    return\n",
    ] {
        assert!(witchy_two::parser::parse(source).is_ok());
        rejects(source);
    }
    // Legal since 2026-11-15: a shared reference is a value, copied, returned and held in a record.
    for source in [
        "type R:\n    x: U8\nfn bad(p: &R):\n    let q = p\nfn main():\n    return\n",
        "type R:\n    x: U8\nfn bad(p: &R) -> &R:\n    return p\nfn main():\n    return\n",
        "type R:\n    x: U8\ntype Bad:\n    p: &R\nfn main():\n    return\n",
    ] {
        witchy_two::wasm::compile_report(source).unwrap();
    }
}

#[test]
fn aggregate_indices_and_full_parameter_frames_have_runtime_controls() {
    compile("fn next(p: &mut U64) -> U64:\n    let old = *p\n    *p += 1\n    return old\nfn set(p: &mut [U8; 3], counter: &mut U64):\n    (*p)[next(counter)] = 9\nfn main(console: Console<Write>):\n    var xs: [U8; 3] = [1, 2, 3]\n    var counter = 0\n    set(&mut xs, &mut counter)\n    console.print_int(counter)\n    console.print_int(U8.to_u64(xs[0]))\n").unwrap();
    // A by-value rvalue snapshots initialized cells before its index helper
    // mutates the live original (runtime output should be 1, then 9).
    compile("fn alter(p: &mut [U8; 2]) -> U64:\n    (*p)[0] = 9\n    return 0\nfn main(console: Console<Write>):\n    var xs: [U8; 2] = [1, 2]\n    let copied = xs[alter(&mut xs)]\n    console.print_int(U8.to_u64(copied))\n    console.print_int(U8.to_u64(xs[0]))\n").unwrap();
    for index in ["18446744073709551615", "4294967296"] {
        compile(&format!("fn spin() -> U8:\n    while true:\n        return spin()\n    return 0\nfn put(p: &mut [U8; 3], i: U64):\n    (*p)[i] = spin()\nfn main(console: Console<Write>):\n    console.print_int(7)\n    var xs: [U8; 3] = [1, 2, 3]\n    put(&mut xs, {index})\n")).unwrap();
    }
    // Param32 cells + four addressed32-cell roots:1280 physical bytes. The full
    // guard must trap before the first parameter cell store (not just locals).
    let mut source = String::from("fn read(p: &[U64; 32]):\n    return\nfn huge(param: [U64; 32]):\n    read(&param)\n");
    for index in 0..4 {
        source.push_str(&format!("    let a{index}: [U64; 32] = [1; 32]\n    read(&a{index})\n"));
    }
    source.push_str("fn main():\n    huge([0; 32])\n");
    compile(&source).unwrap();
}

#[test]
fn typed_place_projections_do_not_bypass_the_function_work_budget() {
    let mut source = String::from("type Inner:\n    tail: U8\ntype Outer:\n    inner: Inner\nfn main():\n    let root = Outer(Inner(1))\n");
    source.push_str(&"    root.inner.tail\n".repeat(1200));
    let error = compile(&source).unwrap_err();
    assert!(error.message.contains("4096 operations"));
}

#[test]
fn result_constructors_private_calls_and_exhaustive_match_are_typed() {
    let source = "fn choose(ok: Bool) -> Result<U8, U64>:\n    if ok:\n        return Ok(7)\n    else:\n        return Err(300)\nfn dispatch(value: Result<U8, U64>) -> U64:\n    match value:\n        Err(payload):\n            return payload\n        Ok(payload):\n            return U8.to_u64(payload)\nfn main(console: Console<Write>):\n    var r: Result<U8, U64> = Ok(1)\n    console.print_int(dispatch(r))\n    r = Err(500)\n    console.print_int(dispatch(r))\n    console.print_int(dispatch(choose(true)))\n    console.print_int(dispatch(choose(false)))\n    console.print_int(dispatch(Ok(9)))\n";
    let (bytes, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    assert!(report.contains("\"sum_types\":[{\"id\":0,\"kind\":\"Result\""));
    // Sums may be stored since 2026-11-14 (tag and largest variant, 2026-11-15).
    assert!(report.contains("\"representation\":\"private-registers\",\"storage\":true,\"storage_bytes\":16,\"alignment\":8"));
    assert!(report.contains("\"name\":\"Ok\",\"tag\":0,\"type\":\"Unsigned(U8)\",\"carrier_start\":1,\"carrier_count\":1"));
    assert!(report.contains("\"name\":\"Err\",\"tag\":1,\"type\":\"Unsigned(U64)\",\"carrier_start\":2,\"carrier_count\":1"));
    assert_eq!(bytes, compile(source).unwrap());
    let (_, plain) = witchy_two::wasm::compile_report("fn main():\n    return\n").unwrap();
    assert!(!plain.contains("sum_types"));
}

#[test]
fn result_unit_and_bool_binders_are_active_values_without_fake_carriers() {
    compile("fn unit_ok(r: Result<(), U64>) -> U64:\n    match r:\n        Ok(unit):\n            let copied: Result<(), ()> = Ok(unit)\n            match copied:\n                Ok:\n                    return 11\n                Err(other_unit):\n                    let also: Result<(), ()> = Err(other_unit)\n                    return 12\n        Err(payload):\n            return payload\nfn unit_err(r: Result<U64, ()>) -> U64:\n    match r:\n        Ok(payload):\n            return payload\n        Err(unit):\n            let copied: Result<(), ()> = Err(unit)\n            return 13\nfn bool_value(r: Result<Bool, ()>) -> U64:\n    match r:\n        Ok(value):\n            if value:\n                return 1\n            else:\n                return 0\n        Err:\n            return 2\nfn main(console: Console<Write>):\n    console.print_int(unit_ok(Ok(())))\n    console.print_int(unit_ok(Err(300)))\n    console.print_int(unit_err(Ok(400)))\n    console.print_int(unit_err(Err(())))\n    console.print_int(bool_value(Ok(true)))\n    console.print_int(bool_value(Ok(false)))\n    console.print_int(bool_value(Err(())))\n").unwrap();
}

#[test]
fn result_active_record_array_payloads_support_ordinary_initialized_copies() {
    compile("type Box<T>:\n    value: T\nfn read(p: &Box<U8>) -> U8:\n    return (*p).value\nfn make() -> Result<Box<U8>, [U16; 2]>:\n    return Ok(Box(7))\nfn main(console: Console<Write>):\n    match make():\n        Ok(record):\n            console.print_int(U8.to_u64(read(&record)))\n            var copy = record\n            copy.value = 9\n            console.print_int(U8.to_u64(copy.value))\n            console.print_int(U8.to_u64(record.value))\n        Err(array):\n            console.print_int(U16.to_u64(array[0]))\n    let failed: Result<Box<U8>, [U16; 2]> = Err([100, 200])\n    match failed:\n        Ok(record):\n            console.print_int(U8.to_u64(record.value))\n        Err(array):\n            console.print_int(U16.to_u64(array[1]))\n").unwrap();
    compile("fn once(counter: &mut U64) -> Result<U8, U64>:\n    *counter += 1\n    return Ok(3)\nfn main(console: Console<Write>):\n    var counter = 0\n    match once(&mut counter):\n        Ok(value):\n            console.print_int(U8.to_u64(value))\n        Err(error):\n            console.print_int(error)\n    console.print_int(counter)\n").unwrap();
}

#[test]
fn result_match_checks_inventory_binders_scope_and_every_selected_arm() {
    let prefix = "fn main():\n    let r: Result<U8, U64> = Ok(1)\n";
    for body in [
        "    match r:\n        Ok(x):\n            return\n",
        "    match r:\n        Ok(x):\n            return\n        Ok(y):\n            return\n",
        "    match r:\n        Ok(x):\n            return\n        Other(y):\n            return\n",
        // `_:` is a rest arm since 2026-11-14: it must come last and stand for some variant.
        "    match r:\n        Ok(x):\n            return\n        Err(y):\n            return\n        _:\n            return\n",
        "    match r:\n        _:\n            return\n        Ok(x):\n            return\n",
        "    match r:\n        Ok:\n            return\n        Err(y):\n            return\n",
        "    match r:\n        Ok(x):\n            x = 2\n        Err(y):\n            return\n",
        "    match r:\n        Ok(x):\n            return\n        Err(y):\n            let bad: Bool = y\n",
        "    match r:\n        Ok(x):\n            return\n        Err(y):\n            let wrong: U64 = x\n",
        "    match r:\n        Ok(x):\n            return\n        Err(y):\n            return\n    let escaped = x\n",
        "    let x = 1\n    match r:\n        Ok(x):\n            return\n        Err(y):\n            return\n",
    ] { rejects(&format!("{prefix}{body}")); }
    // `Ok(_)` and `_:` may drop a copied payload (2026-11-14), never an owner.
    rejects("fn bad(r: Result<Buffer<U8>, U64>):\n    match move r:\n        Ok(_):\n            return\n        Err(y):\n            return\nfn main():\n    return\n");
    rejects("fn bad(r: Result<Buffer<U8>, U64>):\n    match move r:\n        Err(y):\n            return\n        _:\n            return\nfn main():\n    return\n");
    rejects("fn unused(r: Result<U8, U64>) -> U64:\n    match r:\n        Ok(x):\n            return U8.to_u64(x)\n        Err(y):\n            let checked = y\nfn main():\n    return\n");
    compile("fn later(r: Result<U8, U64>) -> U64:\n    match r:\n        Ok(x):\n            return U8.to_u64(x)\n        Err(y):\n            let checked = y\n    return 99\nfn main():\n    return\n").unwrap();
    let error = compile("lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn main():\n    let r: Result<U8, U64> = Ok(1)\n    match r:\n        Ok(x):\n            return\n        Err(y):\n            let bad: Bool = y\n").unwrap_err();
    assert_eq!(error.code, "W0402"); // runtime mismatch BEFORE any Lean resolution
    // The rule's words first; a remedy may follow them.
    assert!(error.message.starts_with("scalar expression type mismatch"), "{}", error.message);
}

#[test]
fn result_sealed_representation_rejects_operators_projection_and_storage() {
    for source in [
        "fn main():\n    let r = Ok(1)\n",
        "fn main():\n    let r: Result<U8, U64> = Ok()\n",
        "fn main():\n    let r: Result<U8, U64> = Err(1, 2)\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(256)\n",
        "fn main():\n    let n: U16 = 1\n    let r: Result<U8, U64> = Ok(n)\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    let same = r == r\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    let same = r != r\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    if r:\n        return\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    let tag = r.tag\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    let inactive = r.Err\n",
        "fn main():\n    let r: Result<U8, U64> = Ok(1)\n    let tried = r?\n",
        // References to a Result, nested Results, Results holding references and stored
        // Results (record fields, arrays, type arguments) are legal: a match on a shared
        // reference and stored sums (2026-11-14), shared variant bytes (2026-11-15) and
        // per-variant borrows (2026-11-17).
        "fn bad(r: Result<Buffer<U8>, U64>):\n    return\nfn main():\n    return\n",
        "type R:\n    x: U8\ntype S:\n    x: U8\nfn main():\n    let r: Result<R, ()> = Ok(S(1))\n",
    ] {
        assert!(witchy_two::parser::parse(source).is_ok(), "control must reach checker: {source}");
        rejects(source);
    }
}

#[test]
fn result_reserved_names_and_constructor_shadowing_fail_before_proof() {
    for name in ["Result", "Ok", "Err"] {
        for declaration in [
            format!("fn {name}():\n    return\n"),
            format!("type {name}:\n    value: U8\n"),
            format!("lemma {name}(x: U64):\n    ensures x == x\n    proof:\n        auto\n"),
        ] {
            let error = compile(&format!("{declaration}fn main():\n    return\n")).unwrap_err();
            assert!(error.message.contains("reserved selected declaration"));
        }
    }
    rejects("fn main():\n    let Ok = 1\n    let r: Result<U8, U64> = Ok(2)\n");
    rejects("fn main(Err: Console<Write>):\n    let r: Result<U8, U64> = Err(2)\n");
    // A capitalized name in a pattern is a variant since 2026-11-14 (nested patterns), so a
    // constructor name can no longer be a binder: `Ok(Err)` matches a U8 against `Err`.
    let error = compile("fn main():\n    let r: Result<U8, U64> = Ok(1)\n    match r:\n        Ok(Err):\n            let copied = Err\n        Err(Ok):\n            let copied = Ok\n").unwrap_err();
    assert!(error.message.contains("match requires a value of a sum type"), "{error:?}");
}

#[test]
fn result_carrier_limits_count_the_tag_and_both_arm_ranges() {
    compile("fn exact(r: Result<[U64; 15], [U64; 16]>):\n    return\nfn main():\n    return\n").unwrap();
    compile("fn exact(r: Result<[U64; 31], ()>):\n    return\nfn main():\n    return\n").unwrap();
    rejects("fn too_big(r: Result<[U64; 16], [U64; 16]>):\n    return\nfn main():\n    return\n");
    rejects("fn too_big(r: Result<[U64; 32], ()>):\n    return\nfn main():\n    return\n");
}

#[test]
fn affine_console_private_moves_shared_logging_and_loop_rebinding_are_supported() {
    let source = "fn relay(c: Console<Write>) -> Console<Write>:\n    return move c\nfn log(sink: &Console<Write>, n: U64):\n    sink.print_int(n)\nfn forward(sink: &Console<Write>, n: U64):\n    log(sink, n)\nfn main(console: Console<Write>):\n    var c = relay(move console)\n    forward(&c, 1)\n    let d = relay(move c)\n    d.print_int(2)\n    var e = relay(move d)\n    var i = 0\n    while i < 3:\n        e = relay(move e)\n        i += 1\n    e.print_int(3)\n    discard(move e)\n";
    let (bytes, receipt) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_some());
    assert!(receipt.contains("\"capability_types\":[{\"name\":\"Console<Write>\",\"ownership\":\"affine\",\"carrier\":\"i32\",\"frame_bytes\":4,\"frame_alignment\":4"));
    // Retired main-only negative now has a typed, private parameter receiver.
    compile("fn helper(authority: Console<Write>):\n    authority.print_int(1)\nfn main():\n    return\n").unwrap();
    compile("fn log(p: &Console<Write>):\n    (*p).print_int(4)\nfn main(c: Console<Write>):\n    log(&c)\n").unwrap();
    compile("fn main():\n    let x = 7\n    let y = move x\n    let z = x + y\n").unwrap();
    let (_, plain) = witchy_two::wasm::compile_report("fn main():\n    return\n").unwrap();
    assert!(!plain.contains("capability_types"));
}

#[test]
fn affine_continuing_paths_ignore_exited_branches_without_resurrecting_moves() {
    compile("fn relay(c: Console<Write>) -> Console<Write>:\n    return move c\nfn early(c: Console<Write>, flag: Bool) -> Console<Write>:\n    if flag:\n        return relay(move c)\n    return move c\nfn selected(c: Console<Write>, r: Result<U8, U64>) -> Console<Write>:\n    match r:\n        Ok(n):\n            return relay(move c)\n        Err(n):\n            let checked = n\n    return move c\nfn loop_exit(c: Console<Write>, flag: Bool) -> Console<Write>:\n    while flag:\n        return move c\n    return move c\nfn main(output: Console<Write>):\n    var c = early(move output, false)\n    c.print_int(10)\n    c = selected(move c, Err(7))\n    c.print_int(20)\n    c = loop_exit(move c, true)\n    c.print_int(30)\n").unwrap();
}

#[test]
fn affine_results_consume_only_the_active_capability_and_match_by_move() {
    let source = "fn choose(c: Console<Write>, flag: Bool) -> Result<Console<Write>, Console<Write>>:\n    if flag:\n        return Ok(move c)\n    return Err(move c)\nfn unwrap(r: Result<Console<Write>, Console<Write>>) -> Console<Write>:\n    match move r:\n        Ok(c):\n            return move c\n        Err(c):\n            return move c\nfn none() -> Result<Console<Write>, U64>:\n    return Err(7)\nfn main(console: Console<Write>):\n    var c = unwrap(choose(move console, true))\n    c.print_int(10)\n    c = unwrap(choose(move c, false))\n    c.print_int(20)\n    match none():\n        Ok(other):\n            other.print_int(0)\n        Err(n):\n            c.print_int(n)\n    c.print_int(30)\n";
    let (bytes, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    assert!(report.contains("\"ownership\":\"affine\",\"implicit_discard\":true"));
}

#[test]
fn affine_copy_double_move_scope_and_maybe_move_fail_closed() {
    let helpers = "fn relay(c: Console<Write>) -> Console<Write>:\n    return move c\nfn close(c: Console<Write>) -> U64:\n    return 1\nfn pair(p: &Console<Write>, n: U64):\n    return\nfn log(p: &Console<Write>):\n    p.print_int(1)\n";
    for body in [
        "    let alias = c\n",
        "    relay(c)\n",
        "    let alias = move c\n    c.print_int(1)\n",
        "    discard(move c)\n    discard(move c)\n",
        "    if true:\n        let alias = move c\n    c.print_int(1)\n",
        "    if flag:\n        discard(move c)\n    c.print_int(1)\n",
        "    if flag:\n        discard(move c)\n    else:\n        discard(move c)\n    c.print_int(1)\n",
        "    discard(move c)\n    log(&c)\n",
        "    pair(&c, close(move c))\n",
        "    c.print_int(close(move c))\n",
        "    let r: Result<Console<Write>, U64> = Ok(c)\n",
        "    let r: Result<Console<Write>, U64> = Ok(move c)\n    match r:\n        Ok(owner):\n            owner.print_int(1)\n        Err(n):\n            return\n",
        "    let r: Result<Console<Write>, U64> = Ok(move c)\n    if flag:\n        discard(move r)\n    match move r:\n        Ok(owner):\n            owner.print_int(1)\n        Err(n):\n            return\n",
    ] {
        rejects(&format!("{helpers}fn bad(c: Console<Write>, flag: Bool):\n{body}fn main():\n    return\n"));
    }
    rejects("fn bad(c: Console<Write>) -> Console<Write>:\n    return c\nfn main():\n    return\n");
    let error = compile("lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn main(c: Console<Write>):\n    let moved = move c\n    c.print_int(1)\n").unwrap_err();
    assert_eq!(error.code, "W0401");
    assert!(error.message.contains("use of a moved value"));
}

#[test]
fn affine_short_circuit_and_loop_conditions_preserve_all_possible_flows() {
    let helper = "fn consume(c: Console<Write>) -> Bool:\n    return false\n";
    for body in [
        "    let decision = flag && consume(move c)\n    c.print_int(1)\n",
        "    let decision = flag || consume(move c)\n    c.print_int(1)\n",
        "    while flag:\n        discard(move c)\n",
        "    while consume(move c):\n        return\n    c.print_int(1)\n",
    ] {
        rejects(&format!("{helper}fn bad(c: Console<Write>, flag: Bool):\n{body}fn main():\n    return\n"));
    }
    // No backedge: the condition still consumes c even on a false/zero-body exit.
    compile(&format!("{helper}fn main(c: Console<Write>):\n    while consume(move c):\n        return\n")).unwrap();
}

#[test]
fn affine_console_reference_reads_are_not_owned_copies_and_storage_stays_closed() {
    for source in [
        "fn steal(p: &Console<Write>) -> Console<Write>:\n    return *p\nfn main():\n    return\n",
        "fn steal(p: &Console<Write>) -> Console<Write>:\n    return move p\nfn main():\n    return\n",
        "fn steal(p: &Console<Write>) -> Console<Write>:\n    return move *p\nfn main():\n    return\n",
        "fn bad(p: &mut Console<Write>):\n    return\nfn main():\n    return\n",
        // A shared reference to a console is a value since 2026-11-14 (`&place` is an
        // expression); it still cannot be stored.
        "fn main(c: Console<Write>):\n    let values = [&c]\n",
        "fn main(c: Console<Write>):\n    let values = [move c]\n",
        // A record may hold a console since 2026-11-15 (sealed capabilities); a Buffer may not.
        "fn bad(b: Buffer<Console<Write>>):\n    return\nfn main():\n    return\n",
        "type Ignore<T>:\n    n: U8\nfn bad(x: Ignore<Console<Write>>):\n    return\nfn main():\n    return\n",
        // `Console<Read>` exists since 2026-11-15; a console of any other right does not.
        "fn bad(x: Console<U64>):\n    return\nfn main():\n    return\n",
        "fn main():\n    let forged: Console<Write> = 1\n",
        "fn main():\n    let forged: Console<Write> = Console(1)\n",
        "fn main(c: Console<Write>):\n    let n = U64.to_u64(move c)\n",
        "fn main(c: Console<Write>):\n    let tag = c.tag\n",
        "fn discard(c: Console<Write>):\n    return\nfn main():\n    return\n",
        "fn main(c: Console<Write>):\n    let discard = 1\n    discard(move c)\n",
    ] {
        assert!(witchy_two::parser::parse(source).is_ok(), "control must reach checker: {source}");
        rejects(source);
    }
}

#[test]
fn linear_buffer_selected_helpers_transfer_on_all_normal_paths_without_a_factory() {
    let source = "fn relay(b: Buffer<U8>) -> Buffer<U8>:\n    return move b\nfn rebound(b: Buffer<U8>, again: Bool) -> Buffer<U8>:\n    var slot = move b\n    while again:\n        slot = relay(move slot)\n    return move slot\nfn early(b: Buffer<U8>, flag: Bool) -> Buffer<U8>:\n    if flag:\n        return relay(move b)\n    return move b\nfn choose(b: Buffer<U8>, flag: Bool) -> Result<Buffer<U8>, Buffer<U8>>:\n    if flag:\n        return Ok(move b)\n    return Err(move b)\nfn unwrap(r: Result<Buffer<U8>, Buffer<U8>>) -> Buffer<U8>:\n    match move r:\n        Ok(b):\n            return move b\n        Err(b):\n            return move b\nfn remap(r: Result<Buffer<U8>, U64>) -> Result<Buffer<U8>, U64>:\n    match move r:\n        Ok(b):\n            return Ok(relay(move b))\n        Err(n):\n            return Err(n)\nfn main():\n    return\n";
    let (bytes, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    assert!(!report.contains("capability_types"));
    assert!(report.contains("\"buffer_types\":[{\"id\":0,\"name\":\"Buffer<U8>\",\"ownership\":\"linear\",\"carrier\":\"i64\",\"frame_storage\":true,\"element\":\"U8\",\"element_stride\":1,\"element_alignment\":1}]"));
    assert!(report.contains("\"ownership\":\"linear\",\"implicit_discard\":false"));
    // Every program also selects the call-once trait the tasks library takes work by
    // (`WitchyOnce0`, 2026-11-17); its methods have no body to check.
    assert!(report.contains("\"selected_functions\":[\"relay\",\"rebound\",\"early\",\"choose\",\"unwrap\",\"remap\",\"main\",\"WitchyOnce0.call_once\",\"WitchyOnce0.discard\",\"WitchyOnce0.start_task\"]"));
    let (_, plain) = witchy_two::wasm::compile_report("fn main():\n    return\n").unwrap();
    assert!(!plain.contains("buffer_types"));
}

#[test]
fn linear_buffer_element_metadata_comes_from_concrete_copy_layouts() {
    let source = "type Pair:\n    wide: U64\n    small: U8\ntype Duo<A, B>:\n    first: A\n    second: B\nfn pair(b: Buffer<Pair>) -> Buffer<Pair>:\n    return move b\nfn array(b: Buffer<[U8; 3]>) -> Buffer<[U8; 3]>:\n    return move b\nfn generic(b: Buffer<Duo<U8, U64>>) -> Buffer<Duo<U8, U64>>:\n    return move b\nfn main():\n    return\n";
    let (_, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(report.contains("\"name\":\"Buffer<Pair>\",\"ownership\":\"linear\",\"carrier\":\"i64\",\"frame_storage\":true,\"element\":\"Pair\",\"element_stride\":16,\"element_alignment\":8"));
    assert!(report.contains("\"name\":\"Buffer<[U8; 3]>\""));
    assert!(report.contains("\"element\":\"[U8; 3]\",\"element_stride\":3,\"element_alignment\":1"));
    assert!(report.contains("\"name\":\"Buffer<Duo<U8, U64>>\""));
}

fn linear_w0401_refuses(source: &str, reason: &str) {
    assert!(witchy_two::parser::parse(source).is_ok(), "control must reach ownership checking");
    let error = compile(source).unwrap_err();
    assert_eq!(error.code, "W0401", "expected ownership stage: {error:?}");
    assert!(error.message.contains(reason), "expected {reason}: {error:?}");
}

#[test]
fn linear_buffer_normal_and_early_scope_exits_must_transfer_every_live_owner() {
    for helper in [
        "fn leak(b: Buffer<U8>):\n    let copied = 1\n",
        "fn leak(b: Buffer<U8>):\n    return\n",
        "fn leak(b: Buffer<U8>) -> U64:\n    return 1\n",
        "fn leak(b: Buffer<U8>, flag: Bool) -> Buffer<U8>:\n    if flag:\n        let hidden = move b\n    return move b\n",
        "fn leak(b: Buffer<U8>, flag: Bool) -> Result<Buffer<U8>, U64>:\n    if flag:\n        return Err(7)\n    return Ok(move b)\n",
        "fn leak(r: Result<Buffer<U8>, U64>) -> Result<Buffer<U8>, U64>:\n    match move r:\n        Ok(b):\n            return Err(7)\n        Err(n):\n            return Err(n)\n",
    ] {
        // At a `return` with a value the refusal is at the return ("this `return` leaves while ...").
        linear_w0401_refuses(&format!("{helper}fn main():\n    return\n"), "still holds an owner");
    }
    linear_w0401_refuses("fn leak(r: Result<Buffer<U8>, U64>) -> Result<Buffer<U8>, U64>:\n    match move r:\n        Ok:\n            return Err(7)\n        Err(n):\n            return Err(n)\nfn main():\n    return\n", "an arm for a variant that holds a value names it");
    let source = "lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn leak(b: Buffer<U8>):\n    return\nfn main():\n    return\n";
    linear_w0401_refuses(source, "still holds an owner when it goes out of scope"); // BEFORE Lean
}

#[test]
fn linear_buffer_ignored_rvalues_discard_and_live_replacement_are_ownership_errors() {
    let helpers = "fn relay(b: Buffer<U8>) -> Buffer<U8>:\n    return move b\nfn wrap(b: Buffer<U8>) -> Result<Buffer<U8>, U64>:\n    return Ok(move b)\n";
    for body in ["    relay(move b)\n", "    wrap(move b)\n"] {
        linear_w0401_refuses(&format!("{helpers}fn leak(b: Buffer<U8>):\n{body}fn main():\n    return\n"), "a value that holds an owner is dropped here: bind it and hand it on, or close or discard it");
    }
    // The built-in `discard` names how an owner is closed instead (2026-11-18).
    linear_w0401_refuses(&format!("{helpers}fn leak(b: Buffer<U8>):\n    discard(move b)\nfn main():\n    return\n"), "`discard(value)` drops a value that holds no owner");
    linear_w0401_refuses(&format!("{helpers}fn leak(b: Buffer<U8>):\n    let r = wrap(move b)\nfn main():\n    return\n"), "still holds an owner when it goes out of scope");
    linear_w0401_refuses(&format!("{helpers}fn overwrite(a: Buffer<U8>, b: Buffer<U8>) -> Buffer<U8>:\n    var slot = move a\n    slot = relay(move b)\n    return move slot\nfn main():\n    return\n"), "linear replacement requires a definitely consumed old slot");
    linear_w0401_refuses("fn copied(b: Buffer<U8>) -> Buffer<U8>:\n    let alias = b\n    return move b\nfn main():\n    return\n", "require explicit move");
    linear_w0401_refuses(&format!("{helpers}fn twice(b: Buffer<U8>) -> Buffer<U8>:\n    let first = relay(move b)\n    return relay(move b)\nfn main():\n    return\n"), "use of a moved value");
}

#[test]
fn linear_buffer_identity_and_feature_exclusions_are_not_fake_ownership_evidence() {
    for (prefix, from, to) in [
        ("", "U8", "U16"),
        ("type R:\n    x: U8\ntype S:\n    x: U8\n", "R", "S"),
        ("", "[U8; 2]", "[U8; 3]"),
    ] {
        let source = format!("{prefix}fn wrong(b: Buffer<{from}>) -> Buffer<{to}>:\n    return move b\nfn main():\n    return\n");
        assert_eq!(compile(&source).unwrap_err().code, "W0402");
    }
    // Feature/type exclusions are intentionally NOT counted as flow controls.
    // A Buffer may hold one owner or a stored Result since 2026-11-15 (`Vec<Vec<Edge>>`,
    // stored sums), and nested Results are legal (2026-11-14), so those cases are gone.
    for source in [
        "fn bad(b: Buffer<Console<Write>>) -> Buffer<Console<Write>>:\n    return move b\nfn main():\n    return\n",
        "fn bad(b: Buffer<()>) -> Buffer<()>:\n    return move b\nfn main():\n    return\n",
        "fn bad(b: [Buffer<U8>; 1]) -> [Buffer<U8>; 1]:\n    return move b\nfn main():\n    return\n",
        "fn bad<T>(b: Buffer<T>) -> Buffer<T>:\n    return b\nfn main():\n    return\n",
        "fn main(b: Buffer<U8>):\n    return\n",
        "fn main():\n    let b: Buffer<U8> = Buffer(1)\n",
        "fn Buffer(n: U64) -> Buffer<U8>:\n    return Buffer(n)\nfn main():\n    return\n",
    ] { rejects(source); }
    linear_w0401_refuses("fn inactive() -> Result<Buffer<U8>, U64>:\n    return Err(7)\nfn main():\n    let r = inactive()\n", "still holds an owner when it goes out of scope");
}

// Recursive Err transfer is a real owner-preserving try_release sink, not an assumed destructor.
const DRAIN_U8: &str = "fn drain(b: Buffer<U8>) -> U8:\n    let freed = memory.try_release(move b)\n    match move freed:\n        Ok:\n            return 7\n        Err(still):\n            return drain(move still)\n";

#[test]
fn typed_allocator_factory_capacity_try_release_and_exact_indices_are_recorded() {
    let source = format!("{DRAIN_U8}fn cap(b: &Buffer<U8>) -> U64:\n    return memory.capacity(b)\nfn main(output: Console<Write>, arena: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 3)\n    match move r:\n        Ok(b):\n            output.print_int(cap(&b))\n            let consumed = drain(move b)\n        Err(code):\n            output.print_int(code)\n");
    let (bytes, report) = witchy_two::wasm::compile_report(&source).unwrap();
    // Allocator state has its own memory (2026-11-14): program memory 0, metadata memory 1.
    assert_eq!(wasm_section_payload(&bytes, 5).unwrap(), &[2, 1, 3, 3, 1, 1, 1]);
    assert!(report.contains("\"runtime_profile\":\"witchy.buffer.v2\""));
    assert!(report.contains("\"contract\":\"witchy.typed-buffer.v3\""));
    // The default stack is 16 KiB since 2026-11-18.
    assert!(report.contains("\"frame_memory\":{\"pages\":3,\"maximum_pages\":3,\"stack_low\":50176,\"stack_high\":66560,\"stack_alignment\":8,\"allocator\":true,\"memory_exported\":true}"));
    // I=4 actual imports (print, print_u8 since 2026-11-15, print_word since 2026-11-19,
    // authorize), N=3 source functions, six arithmetic helpers, then six storage helpers.
    assert!(report.contains("\"imports\":{\"print\":0,\"authorize\":3}"));
    assert!(report.contains("\"helpers\":{\"type_info\":13,\"validate\":14,\"try_alloc\":15,\"capacity\":16,\"try_release\":17,\"slot_prepare\":18}"));
    // The allocator rewrite of 2026-11-14 keeps no full-count table.
    assert!(report.contains("\"initialization_bitmap_low\":7232,\"initialization_bitmap_high\":19520,\"element_carrier_limit\":30"));
    assert!(report.contains("\"element\":\"U8\",\"stride\":1,\"alignment\":1,\"initialized_operations\":true,\"element_carriers\":1"));
}

#[test]
fn allocator_only_empty_type_table_and_nonheap_buffer_borrows_remain_distinct() {
    let (bytes, report) = witchy_two::wasm::compile_report("fn main(arena: Allocator):\n    return\n").unwrap();
    // Allocator state has its own memory (2026-11-14).
    assert_eq!(wasm_section_payload(&bytes, 5).unwrap(), &[2, 1, 3, 3, 1, 1, 1]);
    assert!(report.contains("\"imports\":{\"print\":null,\"authorize\":0}"));
    assert!(report.contains("\"helpers\":{\"type_info\":8,\"validate\":9,\"try_alloc\":10,\"capacity\":11,\"try_release\":12,\"slot_prepare\":13}"));
    assert!(report.contains("\"type_ids\":[]"));
    compile(&format!("{DRAIN_U8}fn main(arena: Allocator, out: Console<Write>):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 0)\n    match move r:\n        Ok(b):\n            out.print_int(memory.capacity(&b))\n            let consumed = drain(move b)\n        Err(code):\n            out.print_int(code)\n")).unwrap();
    // An unreached function is not emitted (2026-11-15), so it selects no frame either.
    let (bytes, report) = witchy_two::wasm::compile_report("fn unused(b: &mut Buffer<U8>):\n    return\nfn main():\n    return\n").unwrap();
    assert!(wasm_section_payload(&bytes, 5).is_none());
    assert!(!report.contains("\"allocator\":{\"contract\""));
}

#[test]
fn allocator_and_buffer_loans_cover_nested_argument_evaluation() {
    linear_w0401_refuses("fn consume(arena: Allocator) -> U64:\n    return 1\nfn main(arena: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, consume(move arena))\n", "active call-argument loan");
    // Since 2026-11-15 `&b` is taken after `drain(move b)` runs, and finds `b` consumed.
    linear_w0401_refuses(&format!("{DRAIN_U8}fn hold(p: &Buffer<U8>, n: U8):\n    return\nfn bad(b: Buffer<U8>):\n    hold(&b, drain(move b))\nfn main():\n    return\n"), "use of a moved value");
    linear_w0401_refuses("fn steal(p: &Buffer<U8>) -> Buffer<U8>:\n    return *p\nfn main():\n    return\n", "explicit move");
}

#[test]
fn mutable_allocator_reference_is_refused_during_type_checking() {
    let source = "fn bad(p: &mut Allocator):\n    return\nfn main():\n    return\n";
    let error = compile(source).unwrap_err();
    assert_eq!(error.code, "W0403");
    assert!(error.message.contains("mutable Console/Allocator/Dir/File references"));
}

#[test]
fn successful_and_error_allocation_results_still_require_owned_flow() {
    linear_w0401_refuses("fn main(arena: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 1)\n", "still holds an owner when it goes out of scope");
    linear_w0401_refuses("fn main(arena: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 1)\n    match move r:\n        Ok(b):\n            return\n        Err(code):\n            return\n", "still holds an owner when it goes out of scope");
    compile(&format!("{DRAIN_U8}fn finish(b: Buffer<U8>):\n    let capacity = memory.capacity(&b)\n    let consumed = drain(move b)\nfn main():\n    return\n")).unwrap();
    linear_w0401_refuses("fn bad(b: Buffer<U8>):\n    memory.try_release(move b)\nfn main():\n    return\n", "a value that holds an owner is dropped here: bind it and hand it on, or close or discard it");
    linear_w0401_refuses("fn bad(b: Buffer<U8>):\n    let r = memory.try_release(move b)\n    match move r:\n        Ok:\n            return\n        Err(still):\n            return\nfn main():\n    return\n", "still holds an owner when it goes out of scope");
}

#[test]
fn allocator_namespace_context_authority_and_migration_fail_closed() {
    for source in [
        "fn main():\n    memory.try_alloc(1)\n",
        "fn bad(arena: &Allocator) -> Buffer<U8>:\n    return memory.try_alloc(arena, 1)\nfn main():\n    return\n",
        "fn main(arena: Allocator):\n    let r: Result<Buffer<U8>, U8> = memory.try_alloc(&arena, 1)\n",
        "fn main(arena: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(2, 1)\n",
        "fn main(arena: Allocator):\n    let memory = 1\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 1)\n",
        "fn main():\n    let forged: Allocator = 2\n",
        "fn Allocator() -> Allocator:\n    return Allocator()\nfn main():\n    return\n",
        "fn main(first: Allocator, second: Allocator):\n    return\n",
        "fn main(b: Buffer<U8>):\n    return\n",
        "fn main():\n    memory.capacity(1)\n",
        "fn main():\n    memory.try_release(1)\n",
    ] { rejects(source); }
    linear_w0401_refuses("fn bad(b: Buffer<U8>):\n    memory.release(move b)\nfn main():\n    return\n", "memory.release was replaced by memory.try_release");
    linear_w0401_refuses("lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn consume(a: Allocator) -> U64:\n    return 1\nfn main(a: Allocator):\n    let r: Result<Buffer<U8>, U64> = memory.try_alloc(&a, consume(move a))\n", "active call-argument loan");
}

#[test]
fn typed_storage_infers_copy_results_and_preserves_nested_layout_fields() {
    let source = "type Inner:\n    wide: U64\n    small: U8\ntype Outer:\n    inner: Inner\n    flag: Bool\nfn ops(p: &mut Buffer<Outer>) -> Result<Outer, U64>:\n    let n = memory.capacity(p)\n    let initialized = memory.init(p, 0, Outer(Inner(9, 2), true))\n    let read = memory.read(p, 0)\n    let swapped = memory.swap(p, 0, Outer(Inner(4, 3), false))\n    return memory.take(p, 0)\nfn drain(b: Buffer<Outer>):\n    let freed = memory.try_release(move b)\n    match move freed:\n        Ok:\n            return\n        Err(still):\n            drain(move still)\nfn main(arena: Allocator):\n    let made: Result<Buffer<Outer>, U64> = memory.try_alloc(&arena, 1)\n    match move made:\n        Ok(b):\n            var buffer = move b\n            let taken = ops(&mut buffer)\n            drain(move buffer)\n        Err(code):\n            return\n";
    // `main` reaches `ops`: an unreached function adds no allocator type table (2026-11-15).
    let (_, report) = witchy_two::wasm::compile_report(source).unwrap();
    assert!(report.contains("\"element\":\"Outer\",\"stride\":24,\"alignment\":8,\"initialized_operations\":true,\"element_carriers\":3"));
    assert!(report.contains("\"byte_offset\":16"));
    compile("fn a(p: &mut Buffer<[U16; 3]>) -> Result<[U16; 3], U64>:\n    let r = memory.init(p, 1, [2; 3])\n    return memory.swap(p, 1, [3; 3])\nfn main():\n    return\n").unwrap();
}

#[test]
fn typed_storage_thirty_carrier_boundary_is_checked_before_payload() {
    for n in [30, 31, 32] {
        let source = format!("fn a(p: &mut Buffer<[U8; {n}]>):\n    let _ = memory.init(p, 0, [1; {n}])\nfn main():\n    return\n");
        if n == 30 { compile(&source).unwrap(); }
        else { linear_w0401_refuses(&source, "at most 30 element carriers"); }
        let (_, report) = witchy_two::wasm::compile_report(&format!("fn a(p: &mut Buffer<[U8; {n}]>):\n    return\nfn main(arena: Allocator):\n    return\n")).unwrap();
        assert!(report.contains(&format!("\"initialized_operations\":{},\"element_carriers\":{n}", n == 30)));
    }
    // Larger Copy buffers still transfer through empty allocation and owner-preserving release.
    compile("fn free(b: Buffer<[U8; 32]>) -> Result<(), Buffer<[U8; 32]>>:\n    return memory.try_release(move b)\nfn main():\n    return\n").unwrap();
}

#[test]
fn typed_storage_conditional_payload_cannot_forget_other_linear_owner() {
    for op in ["init", "swap"] {
        let source = format!("{DRAIN_U8}fn bad(target: Buffer<U8>, other: Buffer<U8>) -> Buffer<U8>:\n    var dst = move target\n    let _ = memory.{op}(&mut dst, 18446744073709551615, drain(move other))\n    return move dst\nfn main():\n    return\n");
        linear_w0401_refuses(&source, "still holds an owner when it goes out of scope");
        let positive = format!("{DRAIN_U8}fn good(target: Buffer<U8>, other: Buffer<U8>) -> Buffer<U8>:\n    var dst = move target\n    let value = drain(move other)\n    let _ = memory.{op}(&mut dst, 18446744073709551615, value)\n    return move dst\nfn main():\n    return\n");
        compile(&positive).unwrap();
    }
    let index = "fn index(b: Buffer<U8>) -> U64:\n    let consumed = drain(move b)\n    return 18446744073709551615\n";
    compile(&format!("{DRAIN_U8}{index}fn good(target: Buffer<U8>, other: Buffer<U8>) -> Buffer<U8>:\n    var dst = move target\n    let _ = memory.init(&mut dst, index(move other), 3)\n    return move dst\nfn main():\n    return\n")).unwrap();
    linear_w0401_refuses(&format!("{DRAIN_U8}{index}fn bad(target: Buffer<U8>, index_owner: Buffer<U8>, payload_owner: Buffer<U8>) -> Buffer<U8>:\n    var dst = move target\n    let _ = memory.init(&mut dst, index(move index_owner), drain(move payload_owner))\n    return move dst\nfn main():\n    return\n"), "still holds an owner when it goes out of scope");
}

#[test]
fn typed_storage_unique_loans_outer_loans_and_owner_replacement_remain_closed() {
    for body in [
        "    let _ = memory.init(&mut dst, memory.capacity(&dst), 1)\n",
        "    memory.swap(&mut dst, 0, memory.take(&mut dst, 0))\n",
    ] {
        linear_w0401_refuses(&format!("fn bad(b: Buffer<U8>) -> Buffer<U8>:\n    var dst = move b\n{body}    return move dst\nfn main():\n    return\n"), "overlapping unique call-argument loans");
    }
    // Since 2026-11-15 a plain `&dst` argument is taken last; one made by a call stays lent.
    let source = "fn view(p: &Buffer<U8>) -> &Buffer<U8>:\n    return p\nfn consume(p: &Buffer<U8>, n: Result<(), U64>):\n    return\nfn bad(b: Buffer<U8>) -> Buffer<U8>:\n    var dst = move b\n    consume(view(&dst), memory.init(&mut dst, 0, 1))\n    return move dst\nfn main():\n    return\n";
    linear_w0401_refuses(source, "a reference into it is still in use");
    linear_w0401_refuses("fn bad(p: &mut Buffer<U8>, b: Buffer<U8>):\n    *p = move b\nfn main():\n    return\n", "owned replacement requires a mutable local slot");
    linear_w0401_refuses("fn bad(b: Buffer<U8>) -> Buffer<U8>:\n    let _ = memory.init(&mut b, 0, 1)\n    return move b\nfn main():\n    return\n", "insufficient root permission");
    linear_w0401_refuses("fn bad(p: &mut Buffer<U8>) -> Buffer<U8>:\n    return *p\nfn main():\n    return\n", "explicit move");
    // Returning a `&mut` parameter is allowed since 2026-11-15 (`&mut` results).
}

#[test]
fn typed_storage_argument_stages_and_enclosing_loan_cleanup_are_discriminating() {
    for (source, reason) in [
        ("fn main():\n    let _ = memory.init(1, 0, 2)\n", "typed storage requires a Buffer reference"),
        ("fn main():\n    memory.read(1)\n", "takes Buffer borrow, index and optional value"),
        ("fn bad(p: &mut Buffer<[U8; 31]>):\n    let _ = memory.init(p, 0, [1; 31])\nfn main():\n    return\n", "at most 30 element carriers"),
    ] { linear_w0401_refuses(source, reason); }
    let error = compile("lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn bad(p: &mut Buffer<U8>):\n    let _ = memory.init(p, 0, true)\nfn main():\n    return\n").unwrap_err();
    assert_eq!(error.code, "W0402"); // invalid payload is checked before any Lean invocation
    // Since 2026-11-15 `&dst` is taken after the other arguments run, and finds `dst` consumed.
    linear_w0401_refuses(&format!("{DRAIN_U8}fn hold(p: &Buffer<U8>, r: Result<U8, U64>, n: U8):\n    return\nfn bad(dst: Buffer<U8>, other: &Buffer<U8>):\n    hold(&dst, memory.read(other, 0), drain(move dst))\nfn main():\n    return\n"), "use of a moved value");
}

const OWNER_VEC: &str = "type Vec<T>:\n    data: Buffer<T>\n    length: U64\n    capacity: U64\n";

#[test]
fn owner_record_traits_concrete_names_and_nested_frame_offsets_are_reported() {
    let source = format!("{OWNER_VEC}type Wrapped<T>:\n    prefix: U8\n    vector: Vec<T>\n    flag: Bool\ntype Row<T>:\n    item: T\n    flag: Bool\ntype Rows<T>:\n    data: Buffer<Row<T>>\n    count: U64\ntype Triples<T>:\n    data: Buffer<[T; 3]>\nfn byte(v: Vec<U8>) -> Vec<U8>:\n    return move v\nfn word(v: Vec<U64>) -> Vec<U64>:\n    return move v\nfn rows(v: Rows<U16>) -> Rows<U16>:\n    return move v\nfn triples(v: Triples<U8>) -> Triples<U8>:\n    return move v\nfn cap(p: &Wrapped<U8>) -> U64:\n    return memory.capacity(&(*p).vector.data)\nfn unwrap(v: Wrapped<U8>) -> Buffer<U8>:\n    let inner = move v.vector\n    return move inner.data\nfn main():\n    return\n");
    let (_, report) = witchy_two::wasm::compile_report(&source).unwrap();
    for name in ["Vec<U8>", "Vec<U64>", "Rows<U16>", "Triples<U8>", "Wrapped<U8>"] {
        assert!(report.contains(&format!("record:{name}")), "missing concrete {name}");
    }
    assert!(report.contains("\"name\":\"Buffer<U8>\""));
    assert!(report.contains("\"name\":\"Buffer<U64>\""));
    assert!(report.contains("\"element\":\"Row<U16>\",\"element_stride\":4,\"element_alignment\":2"));
    assert!(report.contains("\"name\":\"Buffer<[U8; 3]>\""));
    assert!(!report.contains("\"element\":\"T\""));
    assert!(report.contains("record:Wrapped<U8>\",\"storage_bytes\":40,\"alignment\":8"));
    assert!(report.contains("\"slot\":1,\"byte_offset\":8"));
    // Receipts list every owner field and say "owning" since 2026-11-15 (review F9).
    assert!(report.contains("\"ownership\":\"linear\",\"implicit_discard\":false,\"owner_fields\":[\"vector\"]"));
    assert!(report.contains("\"ownership\":\"linear\",\"implicit_discard\":false,\"owner_fields\":[\"data\"]"));
    assert!(report.contains("structural-owning-template"));
}

#[test]
fn owner_record_nominal_equal_width_elements_do_not_share_identity() {
    let source = format!("{OWNER_VEC}type Left:\n    n: U8\ntype Right:\n    n: U8\nfn a(v: Vec<Left>) -> Vec<Left>:\n    return move v\nfn b(v: Vec<Right>) -> Vec<Right>:\n    return move v\nfn main():\n    return\n");
    let (_, report) = witchy_two::wasm::compile_report(&source).unwrap();
    assert!(report.contains("\"id\":0,\"name\":\"Buffer<Left>\""));
    assert!(report.contains("\"id\":1,\"name\":\"Buffer<Right>\""));
    assert_eq!(compile(&format!("{OWNER_VEC}fn bad(v: Vec<U8>) -> Vec<U64>:\n    return move v\nfn main():\n    return\n")).unwrap_err().code, "W0402");
}

#[test]
fn all_selected_owner_templates_validate_roles_before_any_instantiation() {
    let (_, report) = witchy_two::wasm::compile_report(&format!("{OWNER_VEC}fn main():\n    return\n")).unwrap();
    assert!(report.contains("\"aggregate_layouts\":[]")); // no placeholder concrete T
    assert!(report.contains("structural-owning-template")); // renamed 2026-11-15 (review F9)
    // Now legal, so no longer listed: several allocations in one record (2026-11-16); owners
    // as Buffer and record type arguments, as in `Vec<Vec<Edge>>` (2026-11-15); stored
    // Results holding owners (2026-11-14/15); `&` fields (2026-11-15); capability fields
    // (2026-11-15).
    for bad in [
        "type ArrayOwner<T>:\n    a: [Vec<T>; 1]\n",
        "type Cycle<T>:\n    child: Cycle<T>\n",
    ] {
        let error = compile(&format!("{OWNER_VEC}{bad}fn main():\n    return\n")).unwrap_err();
        assert_eq!(error.code, "W0403", "must reach structural role refusal: {bad}: {error:?}");
    }
    // `Buffer<Vec<U8>>` and `Vec<Buffer<U8>>` are legal types since 2026-11-15.
    for signature in ["[Vec<U8>; 1]"] {
        let error = compile(&format!("{OWNER_VEC}fn bad(v: {signature}):\n    return\nfn main():\n    return\n")).unwrap_err();
        assert_eq!(error.code, "W0403");
    }
}

#[test]
fn owner_record_whole_moves_extract_rebuild_and_failed_release_transfer_duty() {
    let source = format!("{OWNER_VEC}type Wrapped<T>:\n    vector: Vec<T>\n    marker: Bool\nfn relay(v: Vec<U8>) -> Vec<U8>:\n    return move v\nfn rebuilt(v: Vec<U8>) -> Vec<U8>:\n    let length = v.length\n    let capacity = v.capacity\n    let data = move v.data\n    return Vec(move data, length, capacity)\nfn self_rebind(v: Vec<U8>) -> Vec<U8>:\n    var slot = move v\n    let length = slot.length\n    let capacity = slot.capacity\n    slot = Vec(move slot.data, length, capacity)\n    return move slot\nfn unwrap(v: Wrapped<U8>) -> Buffer<U8>:\n    let inner = move v.vector\n    return move inner.data\nfn release(v: Vec<U8>) -> Result<(), Vec<U8>>:\n    let length = v.length\n    let capacity = v.capacity\n    let result = memory.try_release(move v.data)\n    match move result:\n        Ok:\n            return Ok(())\n        Err(kept):\n            return Err(Vec(move kept, length, capacity))\nfn main():\n    return\n");
    let (_, report) = witchy_two::wasm::compile_report(&source).unwrap();
    assert!(report.contains("\"ownership\":\"linear\",\"implicit_discard\":false"));
}

#[test]
fn owner_record_normal_paths_and_extract_use_after_reach_real_flow_errors() {
    let relay = "fn relay(v: Vec<U8>) -> Vec<U8>:\n    return move v\n";
    for (helper, reason) in [
        ("fn bad(v: Vec<U8>):\n    return\n", "still holds an owner when it goes out of scope"),
        ("fn bad(v: Vec<U8>):\n    relay(move v)\n", "a value that holds an owner is dropped here: bind it and hand it on, or close or discard it"),
        ("fn bad(v: Vec<U8>) -> Buffer<U8>:\n    let data = move v.data\n    let length = v.length\n    return move data\n", "use of a moved value"),
        ("fn bad(v: Vec<U8>) -> Buffer<U8>:\n    let data = move v.data\n    return move v.data\n", "use of a moved value"),
        ("fn bad(v: Vec<U8>) -> Vec<U8>:\n    let copied = v\n    return move v\n", "explicit move"),
        ("fn bad(v: Vec<U8>, flag: Bool) -> Vec<U8>:\n    if flag:\n        let data = move v.data\n        let rebuilt: Vec<U8> = Vec(move data, 0, 1)\n        return move rebuilt\n    return move v\n", ""),
        ("fn bad(a: Vec<U8>, b: Vec<U8>) -> Vec<U8>:\n    var slot = move a\n    slot = move b\n    return move slot\n", "linear replacement requires a definitely consumed old slot"),
        ("fn bad(v: Vec<U8>) -> Vec<U8>:\n    var slot = move v\n    slot = Vec(move slot.data, slot.length, slot.capacity)\n    return move slot\n", "use of a moved value"),
    ] {
        let source = format!("{OWNER_VEC}{relay}{helper}fn main():\n    return\n");
        if reason.is_empty() { compile(&source).unwrap(); }
        else { linear_w0401_refuses(&source, reason); }
    }
    linear_w0401_refuses(&format!("{OWNER_VEC}lemma good(x: U64):\n    ensures x == x\n    proof:\n        auto\nfn bad(v: Vec<U8>):\n    return\nfn main():\n    return\n"), "still holds an owner when it goes out of scope"); // before Lean
}

#[test]
fn owner_record_extraction_scope_and_rvalue_projection_are_closed() {
    for (helper, reason) in [
        ("fn bad(v: Vec<U8>) -> U64:\n    return move v.length\n", "sole non-Copy field"),
        ("fn bad(v: &mut Vec<U8>) -> Buffer<U8>:\n    return move (*v).data\n", "direct field of a bound owned record"),
        ("fn bad(v: Wrapped<U8>) -> Buffer<U8>:\n    return move v.vector.data\n", "direct field of a bound owned record"),
        ("fn bad(v: Vec<U8>) -> U64:\n    return relay(move v).length\n", "owned record projection requires an available bound place"),
        ("fn bad(v: &Vec<U8>) -> Buffer<U8>:\n    return (*v).data\n", "explicit move"),
        ("fn bad(v: &mut Vec<U8>, b: Buffer<U8>):\n    (*v).data = move b\n", "owned replacement requires a mutable local slot"),
    ] {
        let source = format!("{OWNER_VEC}type Wrapped<T>:\n    vector: Vec<T>\nfn relay(v: Vec<U8>) -> Vec<U8>:\n    return move v\n{helper}fn main():\n    return\n");
        linear_w0401_refuses(&source, reason);
    }
}

#[test]
fn owner_record_field_borrows_use_whole_root_and_preserve_parent_availability() {
    let source = format!("{OWNER_VEC}fn mutate(p: &mut Vec<U8>):\n    let index = (*p).length\n    let _ = memory.init(&mut (*p).data, index, 3)\n    (*p).length += 1\nfn inspect(p: &Vec<U8>) -> U64:\n    return memory.capacity(&(*p).data)\nfn main():\n    return\n");
    compile(&source).unwrap();
    // Since 2026-11-16 loans are per field: `vector.data` beside `vector.length` is accepted,
    // while the same field, or the whole record beside a field, still overlaps.
    compile(&format!("{OWNER_VEC}fn hold(a: &mut Buffer<U8>, n: &U64):\n    return\nfn ok(v: Vec<U8>) -> Vec<U8>:\n    var vector = move v\n    let _ = memory.init(&mut vector.data, vector.length, 1)\n    hold(&mut vector.data, &vector.length)\n    return move vector\nfn main():\n    return\n")).unwrap();
    linear_w0401_refuses(&format!("{OWNER_VEC}fn bad(v: Vec<U8>) -> Vec<U8>:\n    var vector = move v\n    let _ = memory.init(&mut vector.data, memory.capacity(&vector.data), 1)\n    return move vector\nfn main():\n    return\n"), "overlapping unique call-argument loans");
    linear_w0401_refuses(&format!("{OWNER_VEC}fn hold(a: &mut Buffer<U8>, n: &Buffer<U8>):\n    return\nfn bad(v: Vec<U8>) -> Vec<U8>:\n    var vector = move v\n    hold(&mut vector.data, &vector.data)\n    return move vector\nfn main():\n    return\n"), "overlapping unique call-argument loans");
    linear_w0401_refuses(&format!("{OWNER_VEC}fn hold(a: &mut Buffer<U8>, n: &Vec<U8>):\n    return\nfn bad(v: Vec<U8>) -> Vec<U8>:\n    var vector = move v\n    hold(&mut vector.data, &vector)\n    return move vector\nfn main():\n    return\n"), "overlapping unique call-argument loans");
    linear_w0401_refuses(&format!("{OWNER_VEC}fn bad(v: Vec<U8>) -> Buffer<U8>:\n    var vector = move v\n    let data = move vector.data\n    vector.length = 0\n    return move data\nfn main():\n    return\n"), "use of a moved value");
}

#[test]
fn owner_record_conditional_payload_cannot_forget_another_wrapper() {
    let source = format!("{OWNER_VEC}{DRAIN_U8}fn finish(v: Vec<U8>) -> U8:\n    return drain(move v.data)\nfn bad(target: Buffer<U8>, other: Vec<U8>) -> Buffer<U8>:\n    var dst = move target\n    let _ = memory.init(&mut dst, 18446744073709551615, finish(move other))\n    return move dst\nfn main():\n    return\n");
    linear_w0401_refuses(&source, "still holds an owner when it goes out of scope");
    compile(&source.replace("    let _ = memory.init(&mut dst, 18446744073709551615, finish(move other))", "    let value = finish(move other)\n    let _ = memory.init(&mut dst, 18446744073709551615, value)")).unwrap();
    compile(&format!("{OWNER_VEC}fn now_generic<T>(v: Vec<T>) -> Vec<T>:\n    return move v\nfn main():\n    return\n")).unwrap();
}

#[test]
fn owner_copy_field_assignment_rechecks_availability_after_rhs() {
    let prefix = "type Vec<T>:\n    data: Buffer<T>\n    length: U64\n    capacity: U64\nfn dispose(value: Buffer<U8>):\n    match memory.try_release(move value):\n        Ok:\n            return\n        Err(back):\n            dispose(move back)\nfn consume(value: Vec<U8>) -> U64:\n    dispose(move value.data)\n    return 1\n";
    for operation in ["=", "+="] {
        let source = format!("{prefix}fn bad(value: Vec<U8>):\n    var slot = move value\n    slot.length {operation} consume(move slot)\nfn main():\n    return\n");
        let error = compile(&source).unwrap_err();
        assert_eq!(error.code, "W0401");
        assert!(error.message.contains("use of a moved value"));
    }
}

#[test]
fn semantic_body_counts_actual_single_entries_for_all_selected_nongeneric_forms() {
    let source = "type Pair:\n    small: U8\n    wide: U64\nfn pair(p: Pair) -> Pair:\n    return p\nfn access(p: &mut [Pair; 2], i: U64) -> U64:\n    let old = (*p)[i].wide\n    (*p)[i].wide = old + 1\n    return old\nfn unit(r: Result<(), U64>) -> U64:\n    match r:\n        Ok(done):\n            return 1\n        Err(n):\n            return n\nfn main():\n    let p = pair(Pair(1, 2))\n    var ps = [p, p]\n    let old = access(&mut ps, 0)\n    let n = unit(Ok(()))\n";
    // `main` calls each form: only reached functions are instantiated since 2026-11-15.
    let (_, report) = witchy_two::wasm::compile_report(source).unwrap();
    // Every program also selects the call-once trait the tasks library takes work by
    // (`WitchyOnce0`, 2026-11-17); its methods have no body to check.
    assert!(report.contains("\"semantic_body\":{\"contract\":\"witchy.logical-body.v2\",\"selected_definitions\":7,\"checked_bodies\":4,\"concrete_instances\":4,\"body_checks\":[{\"name\":\"pair\",\"checks\":1,\"instances\":1},{\"name\":\"access\",\"checks\":1,\"instances\":1},{\"name\":\"unit\",\"checks\":1,\"instances\":1},{\"name\":\"main\",\"checks\":1,\"instances\":1},{\"name\":\"WitchyOnce0.call_once\",\"checks\":0,\"instances\":0},{\"name\":\"WitchyOnce0.discard\",\"checks\":0,\"instances\":0},{\"name\":\"WitchyOnce0.start_task\",\"checks\":0,\"instances\":0}]}"));
    assert!(report.contains("record:Pair\",\"storage_bytes\":16,\"alignment\":8"));
    assert!(report.contains("\"runtime_profile\":\"witchy.frame.v1\""));
}

#[test]
fn semantic_logical_unit_binders_and_wide_bindings_lower_to_separate_ranges() {
    let source = "type Pair:\n    small: U8\n    wide: U64\nfn pair(p: Pair) -> Pair:\n    return p\nfn mix(r: Result<(), Pair>, p: Pair, tail: U8) -> Pair:\n    let bytes: [U8; 3] = [tail; 3]\n    match r:\n        Ok(done):\n            let saved = pair(p)\n            return Pair(bytes[2], saved.wide)\n        Err(other):\n            return Pair(other.small, p.wide)\nfn main():\n    let q = mix(Ok(()), Pair(1, 2), 3)\n";
    // `main` calls `mix`: only reached functions are instantiated since 2026-11-15.
    let (_, report) = witchy_two::wasm::compile_report(source).unwrap();
    // Every program also selects the call-once trait the tasks library takes work by
    // (`WitchyOnce0`, 2026-11-17); its methods have no body to check.
    assert!(report.contains("\"selected_functions\":[\"pair\",\"mix\",\"main\",\"WitchyOnce0.call_once\",\"WitchyOnce0.discard\",\"WitchyOnce0.start_task\"]"));
    assert!(report.contains("\"checked_bodies\":3,\"concrete_instances\":3"));
    // Every public source inspection also performs concrete lowering before proof resolution.
    assert!(witchy_two::wasm::inspect(source).unwrap().contains("locals"));
}

#[test]
fn physical_target_obligations_refuse_after_semantics_before_proof_or_emission() {
    linear_w0401_refuses("fn bad(a: [U8; 32], extra: U64):\n    return\nfn main():\n    return\n", "at most 32 scalar parameters");
    let body=(0..257).map(|i|format!("    let local{i} = 1\n")).collect::<String>();
    linear_w0401_refuses(&format!("fn bad():\n{body}fn main():\n    return\n"), "at most 256 scalar locals");
    for n in [31,32] { for op in ["read","take"] {
        linear_w0401_refuses(&format!("fn bad(p: &mut Buffer<[U8; {n}]>):\n    let _ = memory.{op}(p, 0)\nfn main():\n    return\n"), "at most 30 element carriers");
    }}
    let wide="fn bad() -> Result<[U8; 32], U64>:\n    return Err(0)\nfn main():\n    return\n";
    let e=compile(wide).unwrap_err(); assert_eq!(e.code,"W0403");assert!(e.message.contains("32 private carriers"));
    let e=witchy_two::wasm::inspect(wide).unwrap_err();assert_eq!(e.code,"W0403");
}

#[test]
fn semantic_cutover_preserves_recursion_owner_flow_and_refused_generic_contracts() {
    let source=format!("{OWNER_VEC}{DRAIN_U8}fn finish(v: Vec<U8>) -> U8:\n    return drain(move v.data)\nfn good(v: Vec<U8>) -> U8:\n    return finish(move v)\nfn main():\n    return\n");
    let (_, report)=witchy_two::wasm::compile_report(&source).unwrap();
    // Unreached bodies are checked but not instantiated (2026-11-15); the three call-once
    // trait methods (2026-11-17) are selected with no body.
    assert!(report.contains("\"selected_definitions\":7,\"checked_bodies\":4,\"concrete_instances\":1"));
    for op in ["=","+="] {
        linear_w0401_refuses(&format!("{OWNER_VEC}fn consume(v: Vec<U8>) -> U64:\n    let data = move v.data\n    let result = memory.try_release(move data)\n    match move result:\n        Ok:\n            return 1\n        Err(kept):\n            let rebuilt: Vec<U8> = Vec(move kept, 0, 1)\n            return consume(move rebuilt)\nfn bad(v: Vec<U8>):\n    var slot = move v\n    slot.length {op} consume(move slot)\nfn main():\n    return\n"), "use of a moved value");
    }
    for source in [
        "fn id<T>(p: T) -> T:\n    return p + p\nfn main():\n    return\n",
        "fn bad(p: U64) -> U64:\n    ensures result == p\n    return p\nfn main():\n    return\n",
        "type Bad:\n    length: U64\n    invariant Valid(length)\nfn main():\n    return\n",
    ] {rejects(source);}
}

#[test]
fn generic_function_bodies_check_once_and_instances_use_concrete_scoped_types() {
    let source="fn id<T>(x: T) -> T:\n    return x\nfn relay<T>(x: T) -> T:\n    return id(x)\nfn unused<T>(x: T) -> T:\n    return x\nfn main():\n    let a: U8 = id(1)\n    let word: U64 = 2\n    let b = id(word)\n    let c: U16 = relay(3)\n";
    let (_,report)=witchy_two::wasm::compile_report(source).unwrap();
    // Every program also selects the call-once trait the tasks library takes work by
    // (`WitchyOnce0`, 2026-11-17); its methods have no body to check.
    assert!(report.contains("\"selected_functions\":[\"id\",\"relay\",\"unused\",\"main\",\"WitchyOnce0.call_once\",\"WitchyOnce0.discard\",\"WitchyOnce0.start_task\"]"));
    assert!(report.contains("\"selected_definitions\":7,\"checked_bodies\":4,\"concrete_instances\":5"));
    assert!(report.contains("\"name\":\"id\",\"checks\":1,\"instances\":3"));
    assert!(report.contains("\"name\":\"unused\",\"checks\":1,\"instances\":0"));
    assert!(report.contains("\"function_instances\":[{\"definition\":\"main\",\"type_arguments\":[],\"ordinal\":0},{\"definition\":\"id\",\"type_arguments\":[\"U8\"],\"ordinal\":1},{\"definition\":\"id\",\"type_arguments\":[\"U64\"],\"ordinal\":2},{\"definition\":\"relay\",\"type_arguments\":[\"U16\"],\"ordinal\":3},{\"definition\":\"id\",\"type_arguments\":[\"U16\"],\"ordinal\":4}]"));
    assert!(!report.contains("$<"));
    compile("fn id<T>(x: T) -> T:\n    return x\nfn choose<T>(a: T, b: T) -> T:\n    return a\nfn main():\n    let typed: U8 = 2\n    let one = choose(1, typed)\n    let nested = choose(id(1), typed)\n    let two: U64 = id(id(3))\n").unwrap();
}

#[test]
fn generic_inference_and_unused_bodies_refuse_without_a_concrete_standin() {
    for (source,reason) in [
        // `id(1)` is a U64 call: a bare number falls back to U64 (2026-11-18 entry on guards).
        ("fn phantom<T>() -> U64:\n    return 1\nfn main():\n    let x = phantom()\n", "generic call has ambiguous type arguments"),
        ("fn bad<T>(x: T) -> T:\n    return x + x\nfn main():\n    return\n", "numeric operation requires unsigned scalar operands"),
        // `==` goes through `Eq.eq` since 2026-11-15.
        ("fn bad<T>(x: T) -> Bool:\n    return x == x\nfn main():\n    return\n", "no trait this type parameter is bound by has a method of this name"),
        ("fn bad<T>(x: T) -> T:\n    if true:\n        return x\n    return unknown(x)\nfn main():\n    return\n", "unsupported or unknown function"),
        ("fn main<T>():\n    return\n", "main cannot be generic"),
        ("fn bad<T, T>(x: T) -> T:\n    return x\nfn main():\n    return\n", "duplicate or conflicting function type parameter"),
        ("fn bad<U8>(x: U8) -> U8:\n    return x\nfn main():\n    return\n", "duplicate or conflicting function type parameter"),
    ]{linear_w0401_refuses(source,reason);}
    let conflict="fn same<T>(a: T, b: T) -> T:\n    return a\nfn main():\n    let a: U8 = 1\n    let b: U64 = 2\n    let n = same(a,b)\n";
    let e=compile(conflict).unwrap_err();assert_eq!(e.code,"W0402");assert!(e.message.contains("generic type argument conflict"));
    for value in ["move b","&b"] {
        let source=format!("fn id<T>(x: T) -> T:\n    return x\nfn bad(b: Buffer<U8>):\n    let result = id({value})\nfn main():\n    return\n");
        let e=compile(&source).unwrap_err();assert_eq!(e.code,"W0403");assert!(e.message.contains("non-unit Copy storage without ownership"));
    }
}

#[test]
fn generic_sccs_keep_scopes_and_propagate_real_target_constraints() {
    compile("fn left<T>(x: T, again: Bool) -> T:\n    if again:\n        return right(x, false)\n    return x\nfn right<U>(x: U, again: Bool) -> U:\n    if again:\n        return left(x, false)\n    return x\nfn main():\n    let a: U8 = left(1,true)\n    let b: U64 = left(2,false)\n").unwrap();
    linear_w0401_refuses("fn grow<T>(x: T):\n    grow([x; 2])\nfn main():\n    return\n", "polymorphic recursion requires identical type substitutions");
    let transitive="fn read_it<T>(p: &Buffer<T>) -> Result<T,U64>:\n    return memory.read(p, 0)\nfn relay<U>(p: &Buffer<U>) -> Result<U,U64>:\n    return read_it(p)\nfn bad(p: &Buffer<[U8;31]>) -> Result<[U8;31],U64>:\n    return relay(p)\nfn main():\n    return\n";
    linear_w0401_refuses(transitive,"at most 30 element carriers");
    linear_w0401_refuses("fn duo<T>(a: T,b: T) -> T:\n    return a\nfn main():\n    let a: [U8;17] = [1;17]\n    let result = duo(a,a)\n", "at most 32 scalar parameters");
}

const GENERIC_VEC_OPS:&str="type Refused<T>:\n    vector: Vec<T>\n    item: T\ntype Popped<T>:\n    vector: Vec<T>\n    item: T\nfn vec_push_spare<T>(input: Vec<T>, item: T) -> Result<Vec<T>,Refused<T>>:\n    var vector = move input\n    let index = vector.length\n    if index < vector.capacity:\n        let initialized = memory.init(&mut vector.data, index, item)\n        match initialized:\n            Ok:\n                vector.length += 1\n                return Ok(move vector)\n            Err(reason):\n                return Err(Refused(move vector,item))\n    return Err(Refused(move vector,item))\nfn vec_take_last<T>(input: Vec<T>) -> Result<Popped<T>,Vec<T>>:\n    var vector = move input\n    if vector.length == 0:\n        return Err(move vector)\n    let index = vector.length - 1\n    let taken = memory.take(&mut vector.data,index)\n    match taken:\n        Ok(item):\n            vector.length = index\n            return Ok(Popped(move vector,item))\n        Err(reason):\n            return Err(move vector)\nfn vec_try_release<T>(input: Vec<T>) -> Result<(),Vec<T>>:\n    let length = input.length\n    let capacity = input.capacity\n    let result = memory.try_release(move input.data)\n    match move result:\n        Ok:\n            return Ok(())\n        Err(data):\n            return Err(Vec(move data,length,capacity))\nfn vec_close<T>(input: Vec<T>):\n    if input.length == 0:\n        let released = vec_try_release(move input)\n        match move released:\n            Ok:\n                return\n            Err(kept):\n                vec_close(move kept)\n                return\n    let popped = vec_take_last(move input)\n    match move popped:\n        Ok(removed):\n            let remaining = move removed.vector\n            vec_close(move remaining)\n        Err(kept):\n            vec_close(move kept)\n";

#[test]
fn generic_source_vec_algorithms_specialize_for_multiple_real_copy_layouts() {
    // Only functions `main` reaches are instantiated since 2026-11-15, so `main` drives each
    // wrapper over a freshly allocated Vec.
    let push = |f: &str, x: &str| format!("            let pushed = {f}(Vec(move b, 0, 1), {x})\n            match move pushed:\n                Ok(v):\n                    vec_close(move v)\n                Err(refused):\n                    let v = move refused.vector\n                    vec_close(move v)\n");
    let driver = |name: &str, element: &str, body: String| format!("fn {name}(arena: &Allocator):\n    let made: Result<Buffer<{element}>, U64> = memory.try_alloc(arena, 1)\n    match move made:\n        Ok(b):\n{body}        Err(code):\n            return\n");
    let mut drivers = driver("bytes", "U8", push("byte", "7"));
    drivers += &driver("words", "U64", "            let taken = word(Vec(move b, 0, 1))\n            match move taken:\n                Ok(popped):\n                    let v = move popped.vector\n                    vec_close(move v)\n                Err(v):\n                    vec_close(move v)\n".to_string());
    drivers += &driver("bools", "Bool", "            boolean(Vec(move b, 0, 1))\n".to_string());
    drivers += &driver("records", "Pair", push("record", "Pair(1, 2)"));
    drivers += "fn main(arena: Allocator):\n    bytes(&arena)\n    words(&arena)\n    bools(&arena)\n    records(&arena)\n";
    let source=format!("{OWNER_VEC}{GENERIC_VEC_OPS}type Pair:\n    small: U8\n    wide: U64\nfn byte(v: Vec<U8>,x: U8) -> Result<Vec<U8>,Refused<U8>>:\n    return vec_push_spare(move v,x)\nfn word(v: Vec<U64>) -> Result<Popped<U64>,Vec<U64>>:\n    return vec_take_last(move v)\nfn boolean(v: Vec<Bool>):\n    vec_close(move v)\nfn record(v: Vec<Pair>,x: Pair) -> Result<Vec<Pair>,Refused<Pair>>:\n    return vec_push_spare(move v,x)\n{drivers}");
    let (_,report)=witchy_two::wasm::compile_report(&source).unwrap();
    for name in ["U8","U64","Bool","Pair"]{assert!(report.contains(&format!("\"name\":\"Buffer<{name}>\"")));}
    assert!(report.contains("\"name\":\"vec_push_spare\",\"checks\":1,\"instances\":2"));
    // Each driver closes what it made, so vec_close is specialized for all four layouts.
    assert!(report.contains("\"name\":\"vec_close\",\"checks\":1,\"instances\":4"));
    assert!(report.contains("\"runtime_profile\":\"witchy.buffer.v2\""));
    assert!(!report.contains("$<"));
    let wide=format!("{OWNER_VEC}{GENERIC_VEC_OPS}fn wide(v: Vec<[U8;26]>,x: [U8;26]) -> Result<Vec<[U8;26]>,Refused<[U8;26]>>:\n    return vec_push_spare(move v,x)\nfn main():\n    return\n");
    let e=compile(&wide).unwrap_err();assert_eq!(e.code,"W0403");assert!(e.message.contains("32 private carriers"));
}

#[test]
fn generic_instances_past_the_old_256_function_limit_compile() {
    let mut source="fn mix<A,B>(a: A,b: B) -> U64:\n    return 1\nfn main():\n".to_string();
    for n in 1..=16{source.push_str(&format!("    let a{n}: [U8;{n}] = [1;{n}]\n"));}
    let calls=(1..=16).flat_map(|a|(1..=16).map(move|b|(a,b))).collect::<Vec<_>>();
    let mut boundary=source.clone();for(a,b)in &calls[..249]{boundary.push_str(&format!("    mix(a{a},a{b})\n"));}
    let (_,report)=witchy_two::wasm::compile_report(&boundary).unwrap();
    // Plus the three call-once trait methods every program selects (2026-11-17).
    assert!(report.contains("\"selected_definitions\":5,\"checked_bodies\":2,\"concrete_instances\":250"));
    // A module may hold 4096 functions; 256 was the limit.
    for(a,b)in &calls[249..]{boundary.push_str(&format!("    mix(a{a},a{b})\n"));}
    let (_,report)=witchy_two::wasm::compile_report(&boundary).unwrap();
    assert!(report.contains("\"concrete_instances\":257"));
}

#[test]
fn source_function_contract_static_scope_and_context_refusals_precede_tools() {
    for (source,reason) in [
        ("fn bad(x: U64) -> U64:\n    requires result == x\n    ensures true\n    proof:\n        auto\n    return x\nfn main():\n    return\n", "function contracts name `result` only in ensures"),
        ("fn bad(result: U64) -> U64:\n    ensures result == 0\n    proof:\n        auto\n    return result\nfn main():\n    return\n", "parameter cannot be named result"),
        ("fn bad(x: U64) -> U64:\n    ensures hidden == x\n    proof:\n        auto\n    let hidden = x\n    return hidden\nfn main():\n    return\n", "clause name is not a parameter"),
        // A contract call inside a branch is proved since 2026-11-14 (4bfeeba4), not refused.
    ] {linear_w0401_refuses(source,reason);}
}
