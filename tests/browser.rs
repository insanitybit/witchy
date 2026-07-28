//! Driver binary consolidating the browser integration tests into one test
//! binary. Each module below was formerly its own `tests/browser_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/browser/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

pub fn run_node_driver(driver: &str, args: &[&str], marker: &str, label: &str) {
    run_node_driver_inner(driver, args, marker, label, false);
}

pub fn run_node_jspi_driver(driver: &str, args: &[&str], marker: &str, label: &str) {
    run_node_driver_inner(driver, args, marker, label, true);
}

fn run_node_driver_inner(driver: &str, args: &[&str], marker: &str, label: &str, requires_jspi: bool) {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join(driver);
    assert!(driver.exists(), "the committed {label} driver must exist at {}", driver.display());
    let has_jspi_flag = requires_jspi
        && Command::new("node")
            .arg("--v8-options")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains("--experimental-wasm-jspi"))
            .unwrap_or(false);
    let mut node = Command::new("node");
    if has_jspi_flag {
        node.arg("--experimental-wasm-jspi");
    }
    if requires_jspi {
        let jspi_probe = node
            .args(["-e", "process.stdout.write(String(typeof WebAssembly.Suspending === 'function' && typeof WebAssembly.promising === 'function'))"])
            .output();
        if !jspi_probe
            .as_ref()
            .is_ok_and(|output| output.status.success() && output.stdout == b"true")
        {
            eprintln!("skipping: Node does not provide WebAssembly JSPI");
            return;
        }
    }
    let mut node = Command::new("node");
    if has_jspi_flag {
        node.arg("--experimental-wasm-jspi");
    }
    let output = node
        .arg(&driver)
        .args(args)
        .current_dir(manifest)
        .output()
        .unwrap_or_else(|error| panic!("spawn {label} driver: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the {label} driver failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains(marker), "{label} driver did not report success:\n{stdout}");
}

#[path = "browser/encoding.rs"]
mod encoding;
#[path = "browser/shim.rs"]
mod shim;
