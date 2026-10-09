//! RFC-0179 R4: `@large_heap` on `main` declares the program's heap mode. The
//! admitted module declares the mode's memory maximum, a large program runs on
//! the large-reservation engine, and a refused growth names the limit hit.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn admit(source: &str) -> witchy_wir::AdmittedCanonicalArtifact {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program")
}

fn declared_maximum_pages(wasm: &[u8]) -> Option<u64> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::MemorySection(memories) = payload.expect("valid module") {
            return memories.into_iter().next().expect("one memory").expect("memory type").maximum;
        }
    }
    None
}

fn console() -> Capabilities {
    Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }
}

#[test]
fn large_heap_program_declares_32_gib_and_runs_on_admitted_wasm() {
    let artifact = admit("@large_heap\nfn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${xs.length()}\")\n");
    assert_eq!(artifact.heap_mode(), witchy_wir::HeapMode::Large);
    assert_eq!(declared_maximum_pages(artifact.wasm()), Some(524_288));
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut vm = runtime.spawn_admitted(&artifact, console(), 64).expect("spawn on the large engine");
    vm.run().expect("run program");
    assert_eq!(vm.output(), ["3"]);
}

#[test]
fn standard_program_keeps_its_4_gib_maximum() {
    let artifact = admit("fn main(console: Console):\n    console.print(\"small\")\n");
    assert_eq!(artifact.heap_mode(), witchy_wir::HeapMode::Standard);
    assert_eq!(declared_maximum_pages(artifact.wasm()), Some(65_536));
}

#[test]
fn refused_growth_names_the_run_limit_and_the_marker() {
    let artifact = admit("fn main(console: Console):\n    var xs: List(Int) = []\n    for i in 0..400000:\n        xs.push(i)\n    console.print(\"${xs.length()}\")\n");
    let mut runtime = Runtime::batch().expect("create runtime");
    // 32 pages: a 2 MiB run limit the 3.2 MB list cannot fit.
    let mut vm = runtime.spawn_admitted(&artifact, console(), 32).expect("spawn program");
    let error = vm.run().expect_err("the list outgrows the run limit").root_cause().to_string();
    assert!(error.contains("out of memory: the 2 MiB memory limit for a run was reached"), "{error}");
    assert!(error.contains("mark `main` with `@large_heap` to ask for a larger heap"), "{error}");
}

#[test]
fn large_heap_objects_live_above_4_gib() {
    // Two untouched 2.4 GB lists put every later object past 4 GiB of bytes:
    // eight-byte references reach it, and the heap audit walks it.
    let artifact = admit("@large_heap\nfn main(console: Console):\n    let a: List(Int) = list.with_capacity(300_000_000)\n    let b: List(Int) = list.with_capacity(300_000_000)\n    var xs: List(Int) = []\n    for i in 0..1000:\n        xs.push(i)\n    let s = \"above \" + \"four \" + \"gibibytes\"\n    console.print(\"${s} ${xs[999]} ${a.length() + b.length()}\")\n");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut vm = runtime.spawn_admitted(&artifact, console(), 524_288).expect("spawn on the large engine");
    vm.run().expect("run program");
    assert_eq!(vm.output(), ["above four gibibytes 999 0"]);
    assert!(vm.heap_bytes().expect("canonical heap") > 1 << 32);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

/// The shifted-reference fuzz target: one program interprets an op script
/// from its arguments over a list, a dictionary and a list of strings, so a
/// random script drives allocation, reuse, reallocation, drops and element
/// addressing through every scaled address form. Each case runs on both heaps
/// and against a Rust model. `PROPTEST_CASES` widens the campaign.
const OPS: &str = "fn main(console: Console, args: List(String)):
    var xs: List(Int) = []
    var d: Dict(Int, Int) = dict.new()
    var ss: List(String) = []
    var acc = 0
    for arg in args:
        let parts = arg.split(\":\")
        let op = parts[0].to_int()
        let a = parts[1].to_int()
        let b = parts[2].to_int()
        match op:
            0 -> xs.push(a)
            1 -> acc = acc + (xs.pop() ?? 0)
            2 ->
                if xs.length() > 0:
                    xs.set_at(a % xs.length(), b)
            3 -> acc = acc + (d.insert(a % 64, b) ?? 0)
            4 -> acc = acc + (d.remove(a % 64) ?? 0)
            5 -> ss.push(show(a) + \"-\" + show(b))
            6 ->
                if ss.length() > 0:
                    ss = ss.concat(ss.slice(0, 1))
            _ -> xs.sort()
    var total = 0
    for k in d.keys():
        total = total + k * 1000 + d.get_or(k, 0)
    console.print(\"${acc} ${total} ${d.length()}\")
    console.print(xs.map(fn(x: Int) -> String: \"${x}\").join(\",\"))
    console.print(ss.join(\",\"))
";

fn ops_model(script: &[(u8, i64, i64)]) -> Vec<String> {
    let (mut xs, mut d, mut ss, mut acc) = (Vec::new(), std::collections::HashMap::new(), Vec::<String>::new(), 0i64);
    for &(op, a, b) in script {
        match op {
            0 => xs.push(a),
            1 => acc += xs.pop().unwrap_or(0),
            2 => if !xs.is_empty() { let at = (a % xs.len() as i64) as usize; xs[at] = b },
            3 => acc += d.insert(a % 64, b).unwrap_or(0),
            4 => acc += d.remove(&(a % 64)).unwrap_or(0),
            5 => ss.push(format!("{a}-{b}")),
            6 => if !ss.is_empty() { ss.push(ss[0].clone()) },
            _ => xs.sort(),
        }
    }
    let total: i64 = d.iter().map(|(k, v)| k * 1000 + v).sum();
    let xs = xs.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    vec![format!("{acc} {total} {}", d.len()), xs, ss.join(",")]
}

fn run_ops(artifact: &witchy_wir::AdmittedCanonicalArtifact, script: &[(u8, i64, i64)]) -> Vec<String> {
    let args = script.iter().map(|(op, a, b)| format!("{op}:{a}:{b}")).collect();
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut vm = runtime.spawn_admitted(artifact, Capabilities { args, ..console() }, 1024).expect("spawn program");
    vm.run().expect("run program");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    vm.output().to_vec()
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(24)))]
    #[test]
    fn scaled_references_match_byte_references_and_the_model(
        script in proptest::collection::vec((0u8..8, 0i64..1000, 0i64..1000), 0..160)
    ) {
        static ARTIFACTS: std::sync::OnceLock<[witchy_wir::AdmittedCanonicalArtifact; 2]> = std::sync::OnceLock::new();
        let [standard, large] = ARTIFACTS.get_or_init(|| [admit(OPS), admit(&format!("@large_heap\n{OPS}"))]);
        let expected = ops_model(&script);
        proptest::prop_assert_eq!(run_ops(standard, &script), expected.clone());
        proptest::prop_assert_eq!(run_ops(large, &script), expected);
    }
}

#[test]
fn large_heap_signs_with_a_granted_secret() {
    // Keyed crypto reads and writes guest buffers through 64-bit addresses.
    const SIGN: &str = "import crypto\nfn main(console: Console, signer: Secret):\n    let sig = crypto.sign(signer, \"sign me\")\n    console.print(crypto.public_key(signer))\n    match crypto.ed25519_verify(crypto.public_key(signer), \"sign me\", sig):\n        Ok(true) -> console.print(\"verified\")\n        _ -> console.print(\"FAILED\")\n";
    let run = |source: &str| {
        let seed = [7u8; 32];
        let caps = Capabilities {
            signing_key: Some(seed),
            secrets: vec![witchy::runtime::SecretGrant::new("signing", seed.to_vec())],
            ..console()
        };
        let mut runtime = Runtime::batch().expect("create runtime");
        let mut vm = runtime.spawn_admitted(&admit(source), caps, 64).expect("spawn program");
        vm.run().expect("run program");
        vm.output().to_vec()
    };
    let standard = run(SIGN);
    assert_eq!(standard[1], "verified");
    assert_eq!(run(&SIGN.replace("fn main(", "@large_heap\nfn main(")), standard);
}

#[test]
fn large_heap_entry_receives_its_arguments() {
    let artifact = admit("@large_heap\nfn main(console: Console, args: List(String)):\n    for arg in args:\n        console.print(arg.to_upper())\n");
    let mut runtime = Runtime::batch().expect("create runtime");
    let caps = Capabilities { args: vec!["one".into(), "seventeen bytes!!".into()], ..console() };
    let mut vm = runtime.spawn_admitted(&artifact, caps, 64).expect("spawn program");
    vm.run().expect("run program");
    assert_eq!(vm.output(), ["ONE", "SEVENTEEN BYTES!!"]);
}
