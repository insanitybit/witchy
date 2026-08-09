//! CLI regression tests for the `witchy` subcommand-consistency fixes: several
//! subcommands used to bypass `mode opt` enforcement, mis-handle a compiled
//! `.wasm` artifact's launch, or mis-count in-language tests. Each test drives the
//! real `witchy` binary (`CARGO_BIN_EXE_witchy`) and is hermetic (its own temp
//! dir), so they can run in parallel. Bug numbers reference `bugs/`.

use std::process::Command;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, Shutdown};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;

use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemorySection, MemoryType, Module, TypeSection, ValType,
};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

#[path = "support/temp_dir.rs"]
mod temp_dir;
use temp_dir::TempDir;

/// A fresh, unique temp directory for one test.
fn workdir(tag: &str) -> TempDir {
    TempDir::new(&format!("cli-{tag}"))
}

fn write(dir: &std::path::Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p.to_str().unwrap().to_string()
}

struct HeaderProbeServer {
    port: u16,
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for HeaderProbeServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_header_probe_server(routes: Vec<(&str, Vec<(&str, &str)>)>) -> HeaderProbeServer {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let map: HashMap<String, Vec<(String, String)>> = routes
        .into_iter()
        .map(|(route, headers)| {
            (
                route.to_string(),
                headers
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
            )
        })
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = thread::spawn(move || {
        loop {
            match stop_rx.try_recv() {
                Ok(()) => break,
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    handle_header_probe_connection(&mut stream, &map);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    HeaderProbeServer { port, stop: stop_tx, thread: Some(thread) }
}

fn handle_header_probe_connection(
    stream: &mut TcpStream,
    routes: &HashMap<String, Vec<(String, String)>>,
) {
    let mut request = [0u8; 1024];
    let n = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..n]);
    let route = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let (status, headers) = routes.get(&route).map_or_else(
        || (404u16, vec![("Content-Type".to_string(), "text/plain".to_string())]),
        |custom| {
            (
                200,
                custom
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            )
        },
    );
    let mut response = format!(
        "HTTP/1.1 {} {}\r\n",
        status,
        if status == 200 { "OK" } else { "Not Found" }
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("Connection: close\r\nContent-Length: 0\r\n\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

/// A legacy/external wasm module with no `witchy.launch` metadata. `call_args`
/// optionally makes `run` call the import with i32 constants; omitting it tests
/// whether an import family is linked without needing valid operation inputs.
fn legacy_import_module(
    name: &str,
    params: &[ValType],
    results: &[ValType],
    call_args: Option<&[i32]>,
) -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function(params.iter().copied(), results.iter().copied());
    types.ty().function([], []);

    let mut imports = ImportSection::new();
    imports.import("witchy", name, EntityType::Function(0));

    let mut functions = FunctionSection::new();
    functions.function(1);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("run", ExportKind::Func, 1);

    let mut run = Function::new([]);
    if let Some(args) = call_args {
        for arg in args {
            run.instruction(&Instruction::I32Const(*arg));
        }
        run.instruction(&Instruction::Call(0));
        for _ in results {
            run.instruction(&Instruction::Drop);
        }
    }
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);

    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

/// Raw external wasm with no launch metadata: mint the root `Secret` through the
/// public ABI, then try to reveal it. This proves imported artifacts receive the
/// same non-revealable signing-key authority as source modules without ever using
/// the old forged `i32` Secret handle representation.
fn signing_key_reveal_module() -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::EXTERNREF]);
    types.ty().function([ValType::EXTERNREF], [ValType::I32]);
    types.ty().function([], []);

    let mut imports = ImportSection::new();
    imports.import("witchy", "mint_secret", EntityType::Function(0));
    imports.import("witchy", "crypto_reveal_len", EntityType::Function(1));

    let mut functions = FunctionSection::new();
    functions.function(2);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("run", ExportKind::Func, 2);

    let mut run = Function::new([]);
    run.instruction(&Instruction::I32Const(0));
    run.instruction(&Instruction::Call(0));
    run.instruction(&Instruction::Call(1));
    run.instruction(&Instruction::Drop);
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);

    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn cli_help_version_and_bare_invocation_are_stable() {
    let expected_help = include_str!("fixtures/cli-help.txt");
    for args in [vec!["--help"], Vec::new()] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected_help, "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let version = match option_env!("WITCHY_BUILD_COMMIT").filter(|commit| !commit.is_empty()) {
        Some(commit) => format!("witchy {} (commit {commit})\n", env!("CARGO_PKG_VERSION")),
        None => format!("witchy {}\n", env!("CARGO_PKG_VERSION")),
    };
    assert_eq!(String::from_utf8_lossy(&out.stdout), version);
    assert!(out.stderr.is_empty());
}

#[test]
fn web_commands_route_natively_and_complete_clean_clone_flow() {
    let dir = workdir("web-flow");
    let project = dir.join("hello-web");
    let project_text = project.to_str().unwrap();

    let created = run(&["new", "--web", project_text]);
    assert_eq!(created.status.code(), Some(0), "{}", String::from_utf8_lossy(&created.stderr));
    assert!(project.join("src/hello_web.witchy").is_file());
    assert!(project.join("web/public").is_dir());

    let tested = run(&["test", "--web", project_text]);
    assert_eq!(tested.status.code(), Some(0), "{}", String::from_utf8_lossy(&tested.stderr));
    assert!(String::from_utf8_lossy(&tested.stdout).contains("typed static Site"));

    let built = run(&["build", "--web", project_text]);
    assert_eq!(built.status.code(), Some(0), "{}", String::from_utf8_lossy(&built.stderr));
    for artifact in [
        "index.html",
        "witchy-web-manifest.json",
        "witchy-build-report.json",
        "witchy-sbom.cdx.json",
        "_headers",
    ] {
        assert!(project.join("dist").join(artifact).is_file(), "{artifact}");
    }

    let doctor = run(&["doctor", "--web", "--format", "json", project_text]);
    assert_eq!(doctor.status.code(), Some(0), "{}", String::from_utf8_lossy(&doctor.stderr));
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["schema"], "witchy.web.doctor.v1");
    assert_eq!(report["ok"], true);

    let missing_deployment = run(&["doctor", "--web", "--deployment"]);
    assert_eq!(
        missing_deployment.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&missing_deployment.stderr)
    );
    assert!(String::from_utf8_lossy(&missing_deployment.stderr).contains("`--deployment` requires a URL"));

    let duplicate_deployment = run(&[
        "doctor",
        "--web",
        "--deployment",
        "https://example.com",
        "--deployment",
        "https://example.org",
        project_text,
    ]);
    assert_eq!(
        duplicate_deployment.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&duplicate_deployment.stderr)
    );
    assert!(String::from_utf8_lossy(&duplicate_deployment.stderr).contains("`--deployment` was supplied more than once"));

    let misplaced = run(&["new", project_text, "--web"]);
    assert_eq!(misplaced.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&misplaced.stderr).contains("requires exactly one destination"));

    let dev_usage = run(&["dev", "--port", "0", project_text]);
    assert_eq!(dev_usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&dev_usage.stderr).contains("1..=65535"));
}

#[test]
fn static_web_flow_needs_no_client_scaffold_or_browser_runtime() {
    let project = workdir("static-web-flow");
    std::fs::create_dir(project.join("src")).unwrap();
    write(
        &project,
        "witchy.toml",
        "[rune]\nname = \"static-web\"\nversion = \"0.1.0\"\n\n\
         [capabilities]\nruntime = []\n\n\
         [dependencies]\n\n\
         [web]\ndelivery = \"static\"\nentry = \"src/site.witchy\"\n",
    );
    write(
        &project,
        "src/site.witchy",
        r#"from glamour import Site

type Message:
    Unused

fn page(text: String) -> glamour.Ui(Message):
    glamour.ui(glamour.element("main", [], [glamour.text(text)]))

pub fn web() -> Site:
    glamour.site([
        glamour.static_page("/", page("Home")),
        glamour.static_page("/about", page("About")),
    ])
"#,
    );
    let project_text = project.to_str().unwrap();

    let tested = run(&["test", "--web", project_text]);
    assert_eq!(
        tested.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&tested.stderr)
    );
    assert!(String::from_utf8_lossy(&tested.stdout).contains("zero browser runtime"));

    let built = run(&["build", "--web", project_text]);
    assert_eq!(
        built.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(project.join("dist/index.html").is_file());
    assert!(project.join("dist/about/index.html").is_file());
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(project.join("dist/witchy-web-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["delivery"], "static");
    assert_eq!(manifest["runtime"]["javascript"], false);
    assert_eq!(manifest["runtime"]["wasm"], false);
    assert_eq!(
        std::fs::read_dir(project.join("dist/assets"))
            .unwrap()
            .count(),
        0
    );

    let doctor = run(&["doctor", "--web", "--format", "json", project_text]);
    assert_eq!(
        doctor.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["id"] == "runtime" && check["status"] == "pass"));
}

#[test]
fn web_doctor_deployment_headers_check_declared_route_policy() {
    let project = workdir("web-deployed-header-probe");
    std::fs::create_dir(project.join("src")).unwrap();
    write(
        &project,
        "witchy.toml",
        "[rune]\nname = \"deployed-header\"\nversion = \"0.1.0\"\n\n\
         [capabilities]\nruntime = []\n\n\
         [dependencies]\n\n\
         [web]\ndelivery = \"static\"\nentry = \"src/site.witchy\"\n",
    );
    write(
        &project,
        "src/site.witchy",
        r#"from glamour import Site

type Message:
    Unused

fn page(text: String) -> glamour.Ui(Message):
    glamour.ui(glamour.element("main", [], [glamour.text(text)]))

pub fn web() -> Site:
    glamour.site([
        glamour.static_page("/", page("Home")),
    ])
"#,
    );
    let project_text = project.to_str().unwrap();

    let built = run(&["build", "--web", project_text]);
    assert!(
        built.status.success(),
        "build should succeed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let manifest_path = project.join("dist/witchy-web-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["browserPolicy"] = serde_json::json!([
        {
            "route": "/",
            "enforcement": "required",
            "contentSecurityPolicy": "default-src 'self'",
            "permissionsPolicy": "camera=()",
        }
    ]);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    {
        let server = spawn_header_probe_server(vec![(
            "/",
            vec![("Content-Security-Policy", "default-src 'self'"), ("Permissions-Policy", "camera=()")],
        )]);
        let good_deployment = format!("http://127.0.0.1:{}", server.port);
        let doctor = run(&[
            "doctor",
            "--web",
            "--format",
            "json",
            "--deployment",
            &good_deployment,
            project_text,
        ]);
        assert_eq!(
            doctor.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&doctor.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
        let deployed = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == "deployed-headers")
            .unwrap();
        assert_eq!(deployed["status"], "pass");
    }

    {
        let server = spawn_header_probe_server(vec![("/", vec![("Permissions-Policy", "camera=()")])]);
        let bad_deployment = format!("http://127.0.0.1:{}", server.port);
        let bad_doctor = run(&[
            "doctor",
            "--web",
            "--format",
            "human",
            "--deployment",
            &bad_deployment,
            project_text,
        ]);
        assert_eq!(
            bad_doctor.status.code(),
            Some(1),
            "missing CSP should fail: {}",
            String::from_utf8_lossy(&bad_doctor.stderr)
        );
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&bad_doctor.stdout),
            String::from_utf8_lossy(&bad_doctor.stderr),
        );
        assert!(output.contains("deployed-headers"));
        assert!(output.contains("missing required Content-Security-Policy"));
    }
}

#[test]
fn glamour_server_adapter_enforces_progressive_form_boundary() {
    let out = Command::new(BIN)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "projects/glamour-server/examples/basic"])
        .output()
        .expect("run Glamour server example");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let behavior = String::from_utf8_lossy(&out.stdout)
        .lines()
        .take(9)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        behavior,
        "200 values:1;secret:s3cret\n\
         403 form origin rejected\n\
         422 duplicate submitted form field `name`\n\
         400 malformed form encoding\n\
         200 values:1;secret:s3cret\n\
         405 method not allowed\n\
         404 form action not found\n\
         415 expected application/x-www-form-urlencoded\n\
         413 form body exceeds configured limit"
    );
}

#[test]
fn missing_secret_values_are_exact_usage_errors() {
    for flag in ["--secret", "--secret-file"] {
        let out = run(&[flag]);
        assert_eq!(out.status.code(), Some(1), "{flag}");
        assert!(out.stdout.is_empty(), "{flag}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), format!("{flag} requires a value\n"));
    }
}

#[test]
fn secret_file_argument_preserves_exact_bytes() {
    let dir = workdir("secret-file");
    let secret = dir.join("token.txt");
    std::fs::write(&secret, b" hunter2 ").unwrap();
    let src = write(
        &dir,
        "secret_file.witchy",
        "import secretstore\nimport crypto\n\nfn main(console: Console, store: SecretStore):\n    console.print(crypto.reveal(secretstore.require(store, \"token\")))\n",
    );
    let spec = format!("token={}", secret.display());
    let out = run(&["--secret-file", &spec, &src]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), " hunter2 \n");
    assert!(out.stderr.is_empty());
}

#[test]
fn crypto_verify_malformed_inputs_are_result_errors() {
    let dir = workdir("crypto-verify-result");
    let p256_basepoint = "046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
    let src = write(
        &dir,
        "crypto_result.witchy",
        &format!(
            "import crypto\nfn main(console: Console):\n    match crypto.ed25519_verify(\"not-hex\", \"msg\", \"00\"):\n        Err(e) -> console.print(crypto.verify_error_message(e))\n        Ok(_v) -> console.print(\"unexpected ed25519\")\n    match crypto.ecdsa_p256_verify_hex(\"{p256_basepoint}\", \"zz\", \"00\"):\n        Err(e) -> console.print(crypto.verify_error_message(e))\n        Ok(_v) -> console.print(\"unexpected ecdsa\")\n"
        ),
    );
    let out = run(&[&src]);
    assert!(out.status.success(), "run failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Ed25519 public key is malformed\nECDSA P-256 message is malformed\n"
    );

    let parity = run(&["parity", &src]);
    assert!(
        parity.status.success() && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
        "parity failed: {}{}",
        String::from_utf8_lossy(&parity.stdout),
        String::from_utf8_lossy(&parity.stderr)
    );
}

// A file that violates `mode opt` (a heap parameter with no ownership convention).
// `check` rejects it; every other compile-ish path must too.
const BAD_OPT: &str = "mode opt\n\nfn helper(xs: List(Int)) -> Int:\n    var acc = 0\n    for x in xs:\n        acc = acc + x\n    acc\n\nfn main(console: Console):\n    console.print(\"sum: ${helper([1, 2, 3])}\")\n";

#[test]
fn compile_accepts_complete_authenticated_package_owners() {
    let dir = workdir("authenticated-package-owners");
    let entry = write(
        &dir,
        "app.witchy",
        "import model\n\nfn main(console: Console):\n    console.print(\"${model.answer()}\")\n",
    );
    let dependency = write(&dir, "model.witchy", "pub fn answer() -> Int:\n    42\n");
    let wasm = dir.join("app.wasm");
    let dep_arg = format!("model={dependency}");
    let out = Command::new(BIN)
        .args([
            "compile",
            &entry,
            "--dep",
            &dep_arg,
            "--package-owner",
            "workspace",
            "example/app",
            "0.1.0",
            "src.app",
            "--dep-owner",
            "model",
            "registry:test-root-key",
            "acme/model",
            "1.2.3",
            "src.model",
            "--out",
            wasm.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "authenticated compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(wasm.exists());

    let missing = Command::new(BIN)
        .args([
            "compile",
            &entry,
            "--dep",
            &dep_arg,
            "--package-owner",
            "workspace",
            "example/app",
            "0.1.0",
            "src.app",
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("missing --dep-owner"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

// ---- BUG-104 / BUG-106: compiled `.wasm` artifact launch ----

#[test]
fn wasm_nil_main_int_output_is_not_eaten_as_exit_code() {
    // The BUG-104 guard: a Nil `main` that prints ints keeps every line and exits 0.
    let dir = workdir("wasm-print");
    let src = write(&dir, "p.witchy", "fn main(console: Console):\n    console.print(\"42\")\n    console.print(\"99\")\n");
    let wasm = dir.join("p.wasm");
    assert!(run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]).status.success());
    let out = run(&[wasm.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n99\n");
}

#[test]
fn sandbox_wasm_requires_explicit_dir_grant() {
    // BUG-106: `sandbox <dir-importing.wasm>` must require `--dir`, like the source
    // form — not silently run against the cwd.
    let dir = workdir("wasm-dir");
    let src = write(
        &dir,
        "d.witchy",
        "fn main(console: Console, dir: Dir) -> Int:\n    if dir.exists(\"x\"):\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n    0\n",
    );
    let wasm = dir.join("d.wasm");
    assert!(run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]).status.success());

    let denied = run(&["sandbox", wasm.to_str().unwrap()]);
    assert!(!denied.status.success(), "sandbox must refuse a Dir-importing wasm with no --dir");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("Dir"));

    let granted = run(&["sandbox", "--dir", dir.to_str().unwrap(), wasm.to_str().unwrap()]);
    assert!(granted.status.success(), "sandbox --dir must run it: {}", String::from_utf8_lossy(&granted.stderr));
}

#[test]
fn fetch_root_requires_an_explicit_valid_origin_for_source_and_wasm() {
    let dir = workdir("fetch-grant");
    let src = write(
        &dir,
        "fetch.witchy",
        "fn main(console: Console, fetch: Fetch):\n    console.print(\"fetch-ready\")\n",
    );

    for args in [
        vec![src.as_str()],
        vec!["sandbox", src.as_str()],
    ] {
        let denied = run(&args);
        assert!(!denied.status.success(), "{args:?} must not mint ambient Fetch");
        assert!(
            String::from_utf8_lossy(&denied.stderr).contains("--fetch"),
            "{args:?}: {}",
            String::from_utf8_lossy(&denied.stderr),
        );
    }

    let malformed = run(&["--fetch", "not-an-origin", &src]);
    assert!(!malformed.status.success());
    assert!(
        String::from_utf8_lossy(&malformed.stderr).contains("invalid `--fetch` grant")
    );

    let granted = run(&["--fetch", "http://127.0.0.1:1", &src]);
    assert!(
        granted.status.success(),
        "valid Fetch origin must launch: {}",
        String::from_utf8_lossy(&granted.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&granted.stdout), "fetch-ready\n");

    let wasm = dir.join("fetch.wasm");
    assert!(
        run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()])
            .status
            .success()
    );
    let denied_wasm = run(&["sandbox", wasm.to_str().unwrap()]);
    assert!(!denied_wasm.status.success());
    assert!(String::from_utf8_lossy(&denied_wasm.stderr).contains("--fetch"));
    let granted_wasm = run(&[
        "sandbox",
        "--fetch",
        "http://127.0.0.1:1",
        wasm.to_str().unwrap(),
    ]);
    assert!(
        granted_wasm.status.success(),
        "valid Fetch origin must launch precompiled Wasm: {}",
        String::from_utf8_lossy(&granted_wasm.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&granted_wasm.stdout),
        "fetch-ready\n"
    );

    let grants = write(
        &dir,
        "fetch.grants.toml",
        "[fetch]\nfetch = [\"http://127.0.0.1:1\"]\n",
    );
    let documented = run(&[
        "sandbox",
        "--grants",
        &grants,
        "--accept-grants",
        &src,
    ]);
    assert!(
        documented.status.success(),
        "named Fetch grant must bind through the document: {}",
        String::from_utf8_lossy(&documented.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&documented.stdout),
        "fetch-ready\n"
    );
}

// ---- BUG-112: SecretStore is mintable empty ----

#[test]
fn secretstore_main_runs_without_a_secret() {
    // BUG-112: a `main(Console, SecretStore)` runs on both backends with an empty
    // store, so `run`/`sandbox` must NOT demand `--secret`; align with `parity`.
    let dir = workdir("secretstore");
    let src = write(&dir, "s.witchy", "fn main(console: Console, store: SecretStore):\n    console.print(\"hi\")\n");
    for args in [vec![src.as_str()], vec!["sandbox", "--dir", dir.to_str().unwrap(), src.as_str()], vec!["parity", src.as_str()]] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?} should exit 0: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn wasm_preserves_an_unused_root_secret_contract() {
    // BUG-113: imports alone cannot reveal an unused capability parameter. The
    // artifact's launch metadata must preserve the source-level `Secret` request.
    let dir = workdir("wasm-unused-secret");
    let src = write(
        &dir,
        "unused_secret.witchy",
        "fn main(console: Console, key: Secret):\n    console.print(\"must not run\")\n",
    );
    let wasm = dir.join("unused_secret.wasm");
    let emit = run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]);
    assert!(emit.status.success(), "emit-wasm failed: {}", String::from_utf8_lossy(&emit.stderr));

    for program in [src.as_str(), wasm.to_str().unwrap()] {
        let denied = run(&[program]);
        let error = String::from_utf8_lossy(&denied.stderr);
        assert!(!denied.status.success(), "{program} must require its declared root Secret");
        assert!(
            error.contains("Secret") && error.contains("--signing-key"),
            "source and artifact should expose the same launch requirement: {error}",
        );
        assert!(String::from_utf8_lossy(&denied.stdout).is_empty());
    }
}

#[test]
fn raw_wasm_reveal_import_receives_the_signing_key_grant() {
    // BUG-427/RFC-0005: the ABI imports are `mint_secret` + `crypto_reveal_len`,
    // not source spellings. Classification must install the granted key as an
    // externref before the reveal host applies the sign-only policy.
    let dir = workdir("wasm-reveal-import");
    let wasm = dir.join("legacy_reveal.wasm");
    std::fs::write(&wasm, signing_key_reveal_module()).unwrap();
    let seed = dir.join("seed.hex");
    std::fs::write(&seed, "41".repeat(32)).unwrap();

    let out = run(&[
        "sandbox",
        "--signing-key",
        seed.to_str().unwrap(),
        wasm.to_str().unwrap(),
    ]);
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success(), "the signing key must remain non-revealable");
    assert!(error.contains("not revealable"), "expected the reveal policy guard: {error}");
    assert!(!error.contains("Secret externref is null"), "the granted key was not installed: {error}");
}

#[test]
fn legacy_clock_and_tls_import_variants_are_classified() {
    let dir = workdir("wasm-import-variants");

    let clock = dir.join("clock.wasm");
    std::fs::write(
        &clock,
        legacy_import_module("now_monotonic", &[], &[ValType::I64], Some(&[])),
    )
    .unwrap();
    let clock_run = run(&[clock.to_str().unwrap()]);
    assert!(
        clock_run.status.success(),
        "now_monotonic must receive the Clock family: {}",
        String::from_utf8_lossy(&clock_run.stderr),
    );

    let tls = dir.join("tls.wasm");
    std::fs::write(
        &tls,
        legacy_import_module(
            "net_listen_tls",
            &[ValType::EXTERNREF, ValType::I32, ValType::I32, ValType::EXTERNREF],
            &[ValType::EXTERNREF],
            None,
        ),
    )
    .unwrap();
    let seed = dir.join("tls-seed.hex");
    std::fs::write(&seed, "42".repeat(32)).unwrap();

    let no_key = run(&[
        "sandbox",
        "--net",
        "127.0.0.1:0",
        tls.to_str().unwrap(),
    ]);
    assert!(!no_key.status.success());
    assert!(String::from_utf8_lossy(&no_key.stderr).contains("Secret"));

    let granted = run(&[
        "sandbox",
        "--net",
        "127.0.0.1:0",
        "--signing-key",
        seed.to_str().unwrap(),
        tls.to_str().unwrap(),
    ]);
    assert!(
        granted.status.success(),
        "net_listen_tls must receive Net Listen and Secret families: {}",
        String::from_utf8_lossy(&granted.stderr),
    );
}

#[test]
fn named_secret_does_not_satisfy_a_bare_secret() {
    // BUG-116/RFC-0005: a bare `Secret` main parameter is the ROOT signing-key
    // externref. A `--secret name=value` populates a SecretStore; if it also
    // satisfied the bare Secret, `crypto.reveal(key)` would leak it as the root
    // key. Only `--signing-key` mints a bare Secret.
    let dir = workdir("bare-secret");
    let src = write(
        &dir,
        "s.witchy",
        "import crypto\n\nfn main(console: Console, key: Secret):\n    console.print(crypto.reveal(key))\n",
    );
    for pre in [vec![], vec!["sandbox", "--dir", dir.to_str().unwrap()]] {
        let mut args = pre.clone();
        args.push("--secret");
        args.push("token=abc123");
        args.push(src.as_str());
        let out = run(&args);
        assert!(
            !out.status.success(),
            "{args:?}: a named secret must NOT satisfy a bare Secret"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !combined.contains("abc123"),
            "{args:?}: the named secret must never be revealed as the root key: {combined}"
        );
    }
}

// ---- BUG-119 / BUG-163 / BUG-177: mode-opt enforced consistently ----

#[test]
fn mode_opt_enforced_across_subcommands() {
    let dir = workdir("mode-opt");
    let bad = write(&dir, "bad.witchy", BAD_OPT);
    let compiled = dir.join("bad.wasm");
    let grants = write(&dir, "empty.grants.toml", "");
    // `check` is the reference: it rejects.
    assert!(!run(&["check", &bad]).status.success(), "check must reject the mode-opt violation");
    // BUG-163 artifact paths, BUG-119 parity, BUG-177 stats + test must all reject too.
    assert!(
        !run(&["compile", &bad, "--out", compiled.to_str().unwrap()]).status.success(),
        "BUG-163: compile must enforce mode opt",
    );
    assert!(!compiled.exists(), "BUG-163: a rejected compile must not write an artifact");
    assert!(!run(&["emit-wat", &bad]).status.success(), "BUG-163: emit-wat must enforce mode opt");
    assert!(
        !run(&["sandbox", "--grants", &grants, "--accept-grants", &bad]).status.success(),
        "BUG-163: grant-document sandbox must enforce mode opt",
    );
    assert!(!run(&["parity", &bad]).status.success(), "BUG-119: parity must enforce mode opt");
    assert!(!run(&["stats", &bad]).status.success(), "BUG-177: stats must enforce mode opt");
    assert!(!run(&["test", &bad]).status.success(), "BUG-177: test must enforce mode opt");
}

#[test]
fn plain_run_does_not_emit_copy_cliff_notes_for_comprehensions() {
    // BUG-560: the plain CLI path used to print a performance note for ordinary
    // code and could expose the parser's synthetic comprehension accumulator
    // (`__comprN`) on stderr. Non-`mode` code is valid; editor advisories belong
    // to the LSP path, not `witchy run`.
    let dir = workdir("plain-compr-note");
    let src = write(
        &dir,
        "compr.witchy",
        "fn main(console: Console):\n    let squares = [n * n for n in 1..6]\n    console.print(\"${squares}\")\n",
    );
    let out = run(&[&src]);
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[1, 4, 9, 16, 25]\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn check_rejects_programs_the_compiled_backend_cannot_accept() {
    // RFC-0070 D2: `check` is the acceptance boundary for runnable programs. A
    // program that typechecks but cannot be compiled must fail at `check`, not
    // surprise the user later at `run`/`emit-wasm`.
    let dir = workdir("check-compiled-acceptance");
    let bad = write(
        &dir,
        "dict_record_key.witchy",
        "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, console, \"one\")\n    console.print(\"${dict.length(d)}\")\n",
    );
    let out = run(&["check", &bad]);
    assert!(!out.status.success(), "check must fail for a compiled-backend reject");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot compile to WASM") && stderr.contains("Dict key type"),
        "check should surface the compiled acceptance error, got: {stderr}"
    );

    let lib = write(&dir, "lib.witchy", "pub fn answer() -> Int:\n    42\n");
    let out = run(&["check", &lib]);
    assert!(out.status.success(), "library-only files remain valid check inputs: {}", String::from_utf8_lossy(&out.stderr));
}

// ---- BUG-120 / BUG-184 / BUG-185: `witchy test` ----

#[test]
fn test_dir_fails_on_a_broken_test_file() {
    // BUG-120: a test file that links but doesn't type-check must FAIL the run, not
    // be silently skipped as "ok. 0 passed".
    let dir = workdir("test-broken");
    write(&dir, "good.witchy", "import testing\nfn test_good():\n    testing.assert(1 + 1 == 2, \"math\")\n");
    write(&dir, "broken.witchy", "fn test_broken():\n    let x = missing_function()\n");
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "a broken test file must fail the run: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn test_imported_module_tests_are_not_double_counted() {
    // BUG-185: `test_lib` in `lib` runs once (when lib.witchy is swept), not again
    // via `suite` which imports lib.
    let dir = workdir("test-dbl");
    write(&dir, "lib.witchy", "import testing\npub fn double(n: Int) -> Int:\n    n * 2\nfn test_lib():\n    testing.assert(double(2) == 4, \"double\")\n");
    write(&dir, "suite.witchy", "import lib\nimport testing\nfn test_suite():\n    testing.assert(lib.double(3) == 6, \"via lib\")\n");
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(out.status.success(), "both tests pass: {}", String::from_utf8_lossy(&out.stdout));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 passed"), "exactly 2 distinct tests, not 3: {stdout}");
    assert_eq!(stdout.matches("test_lib ... ok").count(), 1, "test_lib must run exactly once: {stdout}");
}

#[test]
fn async_and_gen_tests_do_not_silently_pass() {
    // BUG-184: an async test with `fail_with` must FAIL (its body actually runs);
    // a passing async test passes; a gen test is reported as a failure.
    let dir = workdir("test-async");
    write(
        &dir,
        "a.witchy",
        "import testing\nasync fn test_async_fails() -> Nil:\n    testing.fail_with(\"boom\")\nasync fn test_async_passes() -> Nil:\n    testing.assert(1 + 1 == 2, \"ok\")\n",
    );
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "the failing async test must fail the run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test_async_fails ... FAILED"), "async fail must FAIL: {stdout}");
    assert!(stdout.contains("test_async_passes ... ok"), "async pass must pass: {stdout}");
}

// ---- BUG-178 / BUG-179: `witchy caps` ----

#[test]
fn caps_counts_comptime_emitted_apis() {
    // BUG-178: a `comptime:` block that emits `pub fn generated(net: Net)` adds a
    // real capability-bearing API to the footprint.
    let dir = workdir("caps-comptime");
    let src = write(
        &dir,
        "c.witchy",
        "comptime:\n    emit(\"pub fn generated(net: Net, addr: String) -> String:\")\n    emit(\"    net.connect(addr).recv_all()\")\n\nfn main(console: Console):\n    console.print(\"hi\")\n",
    );
    let out = run(&["caps", &src]);
    assert!(out.status.success(), "caps should succeed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("generated") && stdout.contains("Net"), "comptime-emitted Net API must appear: {stdout}");
}

#[test]
fn doc_counts_comptime_emitted_apis() {
    // BUG-178: docs are release-facing API introspection too; generated public
    // functions must render like handwritten public functions on the CLI path.
    let dir = workdir("doc-comptime");
    let src = write(
        &dir,
        "d.witchy",
        "comptime:\n    emit(\"pub fn generated(net: Net) -> Int:\")\n    emit(\"    7\")\n\npub fn direct() -> Int:\n    0\n",
    );
    let out = run(&["doc", &src]);
    assert!(out.status.success(), "doc should succeed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#### `fn generated(net: Net) -> Int`"), "generated API must render: {stdout}");
    assert!(stdout.contains("#### `fn direct() -> Int`"), "handwritten API must still render: {stdout}");
}

#[test]
fn caps_requires_a_typechecking_source() {
    // BUG-179: a footprint over code that doesn't type-check is meaningless.
    let dir = workdir("caps-typeck");
    let src = write(&dir, "m.witchy", "fn main(console: Console):\n    missing(console)\n");
    assert!(!run(&["caps", &src]).status.success(), "caps must require a type-checking source");
}

#[test]
fn caps_diff_grantable_widening_message_matches_exit_code() {
    // BUG-314: adding a grantable (user) capability is a widening — the message and
    // the exit code (2) must agree (was 'OK: no widening' yet exit 2).
    let dir = workdir("caps-diff-uc");
    let old = write(&dir, "old.witchy", "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console):\n    console.print(\"hi\")\n");
    let new = write(&dir, "new.witchy", "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console, ui: UiRoot):\n    console.print(\"hi\")\n");
    let out = run(&["caps-diff", &old, &new]);
    assert_eq!(out.status.code(), Some(2), "a grantable-cap widening must exit 2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("WIDENING"), "the message must flag the widening: {stdout}");
    assert!(!stdout.contains("no widening"), "must not claim 'no widening': {stdout}");
}

// ---- BUG-146: grant-document use-only ----

#[test]
fn grant_document_use_only_blocks_reveal() {
    // BUG-146: a `[secrets]` entry declared `use-only = true` is unrevealable.
    let dir = workdir("grant-use-only");
    let prog = write(
        &dir,
        "r.witchy",
        "import secretstore\nimport crypto\n\nfn main(console: Console, store: SecretStore):\n    let s = secretstore.require(store, \"token\")\n    console.print(crypto.reveal(s))\n",
    );
    let use_only = write(&dir, "use_only.toml", "[secrets]\ntoken = { from = \"env:MY_TOKEN\", use-only = true }\n");
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", &use_only, "--accept-grants", &prog])
        .env("MY_TOKEN", "hunter2")
        .output()
        .expect("spawn");
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(combined.contains("use-only") || combined.contains("cannot be revealed"), "reveal must be refused: {combined}");
    assert!(!combined.contains("hunter2"), "the secret must never be revealed: {combined}");

    // A misspelled modifier is a loud parse error (deny_unknown_fields).
    let bad = write(&dir, "bad.toml", "[secrets]\ntoken = { from = \"env:MY_TOKEN\", use_only = true }\n");
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", &bad, "--accept-grants", &prog])
        .env("MY_TOKEN", "hunter2")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "a misspelled grant key must be rejected");
}

// ---- BUG-610: the reveal policy must be VISIBLE, not merely enforced ----

#[test]
fn grant_approval_row_marks_the_reveal_policy() {
    // BUG-610: enforcement of `use-only` was already correct, but the approval
    // prompt printed a revealable and a use-only secret IDENTICALLY — hiding the
    // strongest per-secret guarantee at the one point a human decides.
    let dir = workdir("grant-approval-reveal-policy");
    let prog = write(
        &dir,
        "r.witchy",
        "import secretstore\nimport crypto\n\nfn main(console: Console, store: SecretStore):\n    console.print(crypto.reveal(secretstore.require(store, \"apikey\")))\n",
    );
    let doc = write(
        &dir,
        "g.toml",
        "[secrets]\napikey = { from = \"env:MY_API_KEY\" }\ntlskey = { from = \"env:MY_TLS_KEY\", use-only = true }\n",
    );
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", &doc, "--accept-grants", &prog])
        .env("MY_API_KEY", "aaa")
        .env("MY_TLS_KEY", "bbb")
        .output()
        .expect("spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("secret apikey: env:MY_API_KEY (revealable)"),
        "a revealable secret must say so: {stderr}"
    );
    assert!(
        stderr.contains("secret tlskey: env:MY_TLS_KEY (use-only)"),
        "a use-only secret must say so: {stderr}"
    );

    // The same policy must show up in the cross-check a reviewer/CI runs.
    let check = run(&["grants-check", &prog, &doc]);
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("apikey: env:MY_API_KEY (revealable)"), "grants-check must show the policy: {stdout}");
    assert!(stdout.contains("tlskey: env:MY_TLS_KEY (use-only)"), "grants-check must show the policy: {stdout}");
}

#[test]
fn grants_diff_flags_a_loosened_reveal_policy() {
    // BUG-610: dropping `use-only = true` grants the same `SecretStore`, so the
    // footprint axis cross-checks as "matches exactly" while the program gains the
    // authority to read that secret's bytes. `grants-diff` is the gate for it.
    let dir = workdir("grants-diff-reveal-policy");
    let tight = write(&dir, "tight.toml", "[secrets]\ntlskey = { from = \"env:MY_TLS_KEY\", use-only = true }\n");
    let loose = write(&dir, "loose.toml", "[secrets]\ntlskey = { from = \"env:MY_TLS_KEY\" }\n");

    let out = run(&["grants-diff", &tight, &loose]);
    assert_eq!(out.status.code(), Some(2), "dropping use-only must exit 2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("SECRET WIDENING"), "the loosening must be named: {stdout}");
    assert!(stdout.contains("tlskey"), "the secret must be named: {stdout}");

    // Unchanged is clean, and TIGHTENING is not a widening.
    let same = run(&["grants-diff", &tight, &tight]);
    assert_eq!(same.status.code(), Some(0), "an unchanged document must exit 0");
    let tightened = run(&["grants-diff", &loose, &tight]);
    assert_eq!(tightened.status.code(), Some(0), "tightening to use-only must not be a widening");

    // A brand-new revealable secret is new read authority.
    let added = write(
        &dir,
        "added.toml",
        "[secrets]\ntlskey = { from = \"env:MY_TLS_KEY\", use-only = true }\nnewkey = { from = \"env:NEW\" }\n",
    );
    let out = run(&["grants-diff", &tight, &added]);
    assert_eq!(out.status.code(), Some(2), "a new revealable secret must exit 2");
    assert!(String::from_utf8_lossy(&out.stdout).contains("newkey"), "the new secret must be named");
}
