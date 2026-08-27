//! RFC-0162 row 13: exact pre/post production-Binaryen differential evidence.

use sha2::{Digest as _, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const LOCK: &str = include_str!("../tools/binaryen-version_131.lock");
const EXACT_RECIPE: &str = "-O4|--all-features|--inlining-optimizing|--inline-functions-with-loops|--flexible-inline-max-function-size=100|--one-caller-inline-max-function-size=200";
const SEC_036_037: &str = r#"type R:
    text: String
    values: List(Int)

fn alias_text(s: String) -> String:
    var working = s
    working = working + "!"
    working

fn alias_values(xs: List(Int)) -> List(Int):
    var working = xs
    working = list.concat(working, working)
    working

fn alias_field(r: R) -> String:
    var working = r.text
    working = working.to_upper()
    working

fn main(console: Console):
    let text = "owned".to_upper()
    let values = [3, 5, 8]
    let record = R(text, values)
    console.print(text)
    console.print(alias_text(text))
    console.print("${values}")
    console.print("${alias_values(values)}")
    console.print(record.text)
    console.print(alias_field(record))
"#;
const SEC_038_OOB: &str = r#"import bytes

fn main(console: Console):
    let b = bytes.from_string("hi!")
    console.print("${bytes.at(b, 5)}")
"#;
const SEC_038_NEGATIVE: &str = r#"import bytes

fn main(console: Console):
    let b = bytes.from_string("hi!")
    console.print("${bytes.at(b, 0 - 1)}")
"#;
const SEC_039: &str = r#"fn main(console: Console):
    var xs = [3, 1, 2]
    list.sort(xs)
    console.print("${xs}")
    var t = "abc"
    t = t.trim()
    console.print("[${t}]")
"#;
const SEC_078: &str = r#"fn slice(text: String, depth: Int) -> String:
    if depth <= 0:
        text
    else:
        slice(text, depth - 1)

fn suffix() -> String:
    ".txt"

fn main(console: Console):
    let trusted_path = "safe" + suffix()
    var dependency_result = slice(trusted_path, 0)
    dependency_result = string.substring(dependency_result, 1, string.length(dependency_result))
    let attacker_allocation = "evil" + suffix()
    console.print(trusted_path)
    console.print(dependency_result)
    console.print(attacker_allocation)
"#;
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "witchy-binaryen-corpus-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).expect("create unique private corpus directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug)]
enum Expected {
    Success(String),
    RuntimeFailure(&'static str),
}

#[derive(Clone, Debug)]
struct CorpusCase {
    name: String,
    source: String,
    expected: Expected,
    opts: &'static str,
    sanitizers: bool,
}

fn sec_cases() -> Vec<CorpusCase> {
    vec![
        CorpusCase {
            name: "SEC-036-037-alias-reassignment".into(),
            source: SEC_036_037.into(),
            expected: Expected::Success(
                "OWNED\nOWNED!\n[3, 5, 8]\n[3, 5, 8, 3, 5, 8]\nOWNED\nOWNED\n".into(),
            ),
            opts: "all",
            sanitizers: true,
        },
        CorpusCase {
            name: "SEC-038-positive-oob".into(),
            source: SEC_038_OOB.into(),
            expected: Expected::RuntimeFailure("bytes index 5 out of bounds (length 3)"),
            opts: "all",
            sanitizers: false,
        },
        CorpusCase {
            name: "SEC-038-negative-oob".into(),
            source: SEC_038_NEGATIVE.into(),
            expected: Expected::RuntimeFailure("bytes index -1 out of bounds (length 3)"),
            opts: "all",
            sanitizers: false,
        },
        CorpusCase {
            name: "SEC-039-literal-overwrite".into(),
            source: SEC_039.into(),
            expected: Expected::Success("[1, 2, 3]\n[abc]\n".into()),
            opts: "all",
            sanitizers: true,
        },
        CorpusCase {
            name: "SEC-078-aliased-call-result".into(),
            source: SEC_078.into(),
            expected: Expected::Success("safe.txt\nafe.txt\nevil.txt\n".into()),
            // Folding would erase the recursive dependency-call shape that
            // makes this regression load-bearing.
            opts: "all,-fold",
            sanitizers: true,
        },
    ]
}

fn adversarial_case(seed: u64) -> CorpusCase {
    let page_len = [0_i64, 1, 4095, 4096, 4097, 8191, 8192, 8193][seed as usize % 8];
    let byte = ((seed.wrapping_mul(73).wrapping_add(19)) & 0xff) as i64;
    let rounds = 3 + (seed % 7) as i64;
    let stop = (seed % rounds as u64) as i64;
    let captured = (seed as i64).wrapping_mul(17).wrapping_sub(41);
    let argument = 9_i64.wrapping_sub(seed as i64);
    let triangular_n = (seed % 13) as i64;
    let loop_sum = stop.wrapping_mul(stop.wrapping_sub(1)) / 2;
    let closure_result = captured.wrapping_add(argument);
    let triangular = triangular_n.wrapping_mul(triangular_n.wrapping_add(1)) / 2;
    let last = if page_len == 0 { -1 } else { byte };
    let boxed_score = page_len.wrapping_add(2);
    let source = format!(
        r#"import bytes

type Payload:
    Boxed(String, List(Int))
    Scalar(Int)

type Envelope:
    payload: Payload
    metadata: (Int, String)

fn payload_score(payload: Payload) -> Int:
    match payload:
        Boxed(text, values) -> text.length() + values.length()
        Scalar(value) -> value

fn triangular(n: Int) -> Int:
    if n <= 0:
        0
    else:
        n + triangular(n - 1)

fn apply(f: fn(Int) -> Int, value: Int) -> Int:
    f(value)

fn main(console: Console):
    let page = "x".repeat({page_len})
    let raw = bytes.repeat({byte}, {page_len})
    let payload = Boxed(page, [{captured}, {argument}])
    let envelope = Envelope(payload, ({seed}, "seed-{seed}"))
    var sum = 0
    var i = 0
    while i < {rounds}:
        if i == {stop}:
            break
        sum = sum + i
        i = i + 1
    let captured = fn(value: Int) -> Int: value + ({captured})
    let last = if raw.length() == 0: 0 - 1 else: raw.at(raw.length() - 1)
    let zero = list.repeat({seed}, 0).length()
    let wrapped = 9223372036854775807 + 1
    console.print("${{page.length()}}:${{raw.length()}}:${{last}}:${{sum}}:${{apply(captured, {argument})}}:${{payload_score(envelope.payload)}}:${{triangular({triangular_n})}}:${{zero}}:${{wrapped}}:${{envelope.metadata.0}}")
"#
    );
    CorpusCase {
        name: format!("generated-adversarial-seed-{seed}"),
        source,
        expected: Expected::Success(format!(
            "{page_len}:{page_len}:{last}:{loop_sum}:{closure_result}:{boxed_score}:{triangular}:0:-9223372036854775808:{seed}\n"
        )),
        opts: "all",
        sanitizers: true,
    }
}

fn corpus() -> Vec<CorpusCase> {
    let mut cases = sec_cases();
    cases.extend((0..8).map(adversarial_case));
    cases
}

fn sha256(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn current_asset_block() -> &'static str {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    let platform = "aarch64-linux";
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let platform = "x86_64-linux";
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    let platform = "arm64-macos";
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    let platform = "x86_64-macos";
    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    )))]
    compile_error!("the production Binaryen corpus requires a supported release target");
    LOCK.split("[[asset]]")
        .find(|block| block.contains(&format!("platform = \"{platform}\"")))
        .expect("reviewed lock contains the current release target")
}

fn lock_value(block: &str, key: &str) -> String {
    let prefix = format!("{key} = \"");
    block
        .lines()
        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))
        .unwrap_or_else(|| panic!("reviewed asset lacks {key}"))
        .to_string()
}

fn expected_bundle_sha256(executable: &str, library: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"witchy-binaryen-bundle-v1");
    digest.update(b"bin/wasm-opt");
    digest.update(hex_digest(executable));
    if let Some(library) = library {
        digest.update(b"lib/libbinaryen.dylib");
        digest.update(hex_digest(library));
    }
    hex_bytes(&digest.finalize())
}

fn hex_digest(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64);
    let mut bytes = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn pin_environment() -> Vec<(String, String)> {
    let asset = current_asset_block();
    let executable = lock_value(asset, "executable_sha256");
    let path = std::env::var("WITCHY_PINNED_WASM_OPT_PATH")
        .expect("exact corpus requires WITCHY_PINNED_WASM_OPT_PATH from pinned-binaryen.py");
    assert!(
        Path::new(&path).is_absolute(),
        "pinned wasm-opt path must be absolute"
    );
    assert_eq!(
        std::env::var("WITCHY_PINNED_WASM_OPT_SHA256").unwrap(),
        executable,
        "corpus executable identity must match the reviewed lock"
    );
    let mut environment = vec![
        ("WITCHY_PINNED_WASM_OPT_PATH".into(), path),
        ("WITCHY_PINNED_WASM_OPT_SHA256".into(), executable),
    ];
    if asset.contains("runtime_library_sha256") {
        let library = lock_value(asset, "runtime_library_sha256");
        assert_eq!(
            std::env::var("WITCHY_PINNED_BINARYEN_LIB_SHA256").unwrap(),
            library,
            "corpus runtime-library identity must match the reviewed lock"
        );
        environment.push(("WITCHY_PINNED_BINARYEN_LIB_SHA256".into(), library));
    } else {
        assert!(
            std::env::var_os("WITCHY_PINNED_BINARYEN_LIB_SHA256").is_none(),
            "Linux Binaryen is static and must not accept a runtime-library pin"
        );
    }
    environment
}

fn run_case(
    case: &CorpusCase,
    lane: &str,
    opts: &str,
    pin: &[(String, String)],
) -> (Output, Scratch) {
    let temporary = Scratch::new();
    let source = temporary.path().join(format!("{}.witchy", case.name));
    std::fs::File::create(&source)
        .unwrap()
        .write_all(case.source.as_bytes())
        .unwrap();
    let home = temporary.path().join("home");
    let cache = temporary.path().join("cache");
    let tmp = temporary.path().join("tmp");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&tmp).unwrap();
    let mut command = Command::new(BIN);
    command
        .arg(&source)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .env("TMPDIR", &tmp)
        .env("WITCHY_OPT", opts)
        .env("WITCHY_REQUIRE_PINNED_BINARYEN", "1")
        .env("WIRDIAG", "1");
    if case.sanitizers {
        command
            .env("WITCHY_UAF_CHECK", "1")
            .env("WITCHY_RC_ASSERT", "1");
    }
    if lane == "post" {
        command.envs(pin.iter().cloned());
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cannot execute {} lane for {}: {error}", lane, case.name));
    assert!(
        output.status.code().is_some(),
        "{lane} lane crashed by signal for {}\n{}",
        case.name,
        String::from_utf8_lossy(&output.stderr)
    );
    (output, temporary)
}

fn assert_expected(case: &CorpusCase, lane: &str, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match &case.expected {
        Expected::Success(expected) => {
            assert!(
                output.status.success(),
                "{lane} lane unexpectedly failed for {}\n--- source ---\n{}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
                case.name,
                case.source
            );
            assert_eq!(
                stdout.as_ref(),
                expected,
                "{lane} lane violated the independent oracle for {}",
                case.name
            );
        }
        Expected::RuntimeFailure(expected_error) => {
            assert!(
                !output.status.success(),
                "{lane} lane accepted the independently invalid runtime access for {}",
                case.name
            );
            assert!(
                stdout.is_empty(),
                "{lane} lane produced output before the expected runtime failure for {}: {stdout:?}",
                case.name
            );
            assert!(
                !stderr.contains("required pinned Binaryen"),
                "{lane} lane failed in the evidence machinery instead of the fixture: {stderr}"
            );
            assert!(
                stderr.contains(expected_error),
                "{lane} lane failed outside the independently expected bytes-OOB error for {}: {stderr}",
                case.name
            );
        }
    }
}

fn input_trace(case: &CorpusCase, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let traces: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("WIRTRACE binaryen-input "))
        .collect();
    assert_eq!(
        traces.len(),
        1,
        "{} input trace evidence:\n{stderr}",
        case.name
    );
    let digest = traces[0]
        .strip_prefix("WIRTRACE binaryen-input input_sha256=")
        .expect("input trace has the exact schema");
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    digest.to_string()
}

fn assert_exact_trace(case: &CorpusCase, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let traces: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("WIRTRACE binaryen-applied "))
        .collect();
    assert_eq!(traces.len(), 1, "{} trace evidence:\n{stderr}", case.name);
    let fields: std::collections::BTreeMap<&str, &str> = traces[0]
        .split_ascii_whitespace()
        .skip(2)
        .map(|field| field.split_once('=').expect("trace field is key=value"))
        .collect();
    let asset = current_asset_block();
    let executable = lock_value(asset, "executable_sha256");
    let library = asset
        .contains("runtime_library_sha256")
        .then(|| lock_value(asset, "runtime_library_sha256"));
    let version_digest = sha256(b"wasm-opt version 131 (version_131)\n");
    let bundle_digest = expected_bundle_sha256(&executable, library.as_deref());
    assert_eq!(fields.get("release"), Some(&"version_131"));
    assert_eq!(
        fields.get("version_stdout_sha256"),
        Some(&version_digest.as_str())
    );
    assert_eq!(fields.get("recipe_schema"), Some(&"witchy-wasm-opt-v3"));
    assert_eq!(fields.get("args"), Some(&EXACT_RECIPE));
    assert_eq!(fields.get("executable_sha256"), Some(&executable.as_str()));
    assert_eq!(
        fields.get("library_sha256"),
        Some(&library.as_deref().unwrap_or("none"))
    );
    assert_eq!(fields.get("bundle_sha256"), Some(&bundle_digest.as_str()));
    for field in ["input_sha256", "output_sha256"] {
        let value = fields
            .get(field)
            .unwrap_or_else(|| panic!("trace lacks {field}"));
        assert_eq!(value.len(), 64, "trace {field} is not SHA-256");
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
    fields["input_sha256"].to_string()
}

#[test]
fn corpus_manifest_covers_every_applicable_sec_fixture_and_generated_shape() {
    let cases = corpus();
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for required in [
        "SEC-036-037-alias-reassignment",
        "SEC-038-positive-oob",
        "SEC-038-negative-oob",
        "SEC-039-literal-overwrite",
        "SEC-078-aliased-call-result",
    ] {
        assert!(
            names.contains(&required),
            "missing applicable fixture {required}"
        );
    }
    assert_eq!(cases.len(), 13);
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with("generated-adversarial-seed-"))
            .count(),
        8
    );
    for case in cases {
        assert!(!case.source.is_empty());
        match case.expected {
            Expected::Success(expected) => assert!(!expected.is_empty()),
            Expected::RuntimeFailure(expected_error) => assert!(!expected_error.is_empty()),
        }
    }
}

#[test]
#[ignore = "requires the exact official v131 bundle installed by scripts/pinned-binaryen.py"]
fn exact_pre_post_production_pipeline_corpus() {
    let pin = pin_environment();
    for case in corpus() {
        let pre_opts = format!("{},-wasm-opt", case.opts);
        let (pre, _pre_directory) = run_case(&case, "pre", &pre_opts, &pin);
        assert_expected(&case, "pre", &pre);
        assert!(
            !String::from_utf8_lossy(&pre.stderr).contains("WIRTRACE binaryen-applied"),
            "pre lane unexpectedly passed through Binaryen for {}",
            case.name
        );
        let pre_input = input_trace(&case, &pre.stderr);

        let (post, _post_directory) = run_case(&case, "post", case.opts, &pin);
        assert_expected(&case, "post", &post);
        let post_input = input_trace(&case, &post.stderr);
        let applied_input = assert_exact_trace(&case, &post.stderr);
        assert_eq!(
            post_input, applied_input,
            "post trace input identity for {}",
            case.name
        );
        assert_eq!(
            pre_input, post_input,
            "pre lane and production Binaryen input differ for {}",
            case.name
        );
        assert_eq!(
            (pre.status.success(), pre.stdout.as_slice()),
            (post.status.success(), post.stdout.as_slice()),
            "pre/post production pipeline differential for {}",
            case.name
        );
    }
}
