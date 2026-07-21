//! Build-step execution service (`build` entrypoints run in the grant-minimal
//! WASM sandbox). Extracted from the composition root.

use crate::{ast, codegen, link_file, runtime, typeck};

/// Run a deterministic `build` step in the zero-ambient WASM sandbox. This keeps
/// the old BuildOut/BuildRead-only helper shape used by tests and callers; the
/// grantful production path is [`run_build_step_compiled`].
#[cfg(test)]
pub fn run_build_step_sandboxed(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
) -> Result<Vec<String>, String> {
    run_build_step_compiled(module, out_dir, read_roots, Vec::new(), Vec::new(), Vec::new())
}

/// Run a `build` step in the **grant-minimal WASM sandbox**: compile it (the
/// `build` entrypoint becomes the `run` export), then instantiate under a
/// `Capabilities` granting only the build output sandbox, read roots, and named
/// BuildEnv/BuildExec/BuildNet allow-lists. The module physically has no
/// `dir_*`/runtime `net_*`/`print` import to call, and every build primitive is
/// confined by the same host-side grant tables as the interpreter oracle.
///
/// This is the production build-step path. The interpreter remains the oracle
/// for parity tests, not a package-manager execution backend.
#[cfg(test)]
pub fn run_build_step_compiled(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
    env_keys: Vec<String>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = capture_build_env(&env_keys);
    run_build_step_compiled_with_env(module, out_dir, read_roots, env, exec_tools, net_hosts)
}

fn capture_build_env(
    keys: &[String],
) -> std::collections::BTreeMap<String, Option<String>> {
    let env: std::collections::BTreeMap<_, _> = keys
        .iter()
        .map(|key| (key.clone(), std::env::var(key).ok()))
        .collect();
    debug_assert!(
        keys.iter().all(|key| env.contains_key(key)),
        "every granted env name must be represented in the snapshot"
    );
    env
}

fn run_build_step_compiled_with_env(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
    env: std::collections::BTreeMap<String, Option<String>>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    use runtime::{Capabilities, Runtime};
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("build: output dir: {e}"))?;
    let wasm = match codegen::compile_build_module(&module) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.message),
    };
    let caps = Capabilities {
        build_out: Some(out_dir.clone()),
        build_read_roots: read_roots,
        build_env: Some(env),
        exec_allow: Some(exec_tools),
        build_net_allow: Some(net_hosts),
        ..Default::default()
    };
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(&wasm, caps, crate::RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    let mut generated: Vec<String> = std::fs::read_dir(&out_dir)
        .map_err(|e| format!("build: reading output dir: {e}"))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    generated.sort();
    Ok(generated)
}

/// Parse, link, type-check, and run a file's `build` entrypoint under confined
/// grants, returning the names of the files it generated. The output directory
/// defaults to `./build-out`.
pub(crate) fn run_build_step_file(
    path: &str,
    out_dir: Option<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    env_keys: Vec<String>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = capture_build_env(&env_keys);
    run_build_step_file_with_env(path, out_dir, read_roots, env, exec_tools, net_hosts)
}

fn run_build_step_file_with_env(
    path: &str,
    out_dir: Option<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    env: std::collections::BTreeMap<String, Option<String>>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let (linked, _) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let out = out_dir.unwrap_or_else(|| std::path::PathBuf::from("build-out"));
    run_build_step_compiled_with_env(linked, out, read_roots, env, exec_tools, net_hosts)
}

#[cfg(test)]
mod compiled_build_step_tests {
    use super::{run_build_step_file, run_build_step_file_with_env};

    fn unique(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    fn write_source(dir: &std::path::Path, src: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("build.witchy");
        std::fs::write(&path, src).unwrap();
        path
    }

    #[test]
    fn compiled_build_env_reads_only_allow_listed_keys() {
        let dir = unique("compiled_build_env");
        let _ = std::fs::remove_dir_all(&dir);
        let env: std::collections::BTreeMap<String, Option<String>> =
            [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into();

        let allowed = write_source(
            &dir,
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        );
        run_build_step_file_with_env(
            allowed.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            env.clone(),
            vec![],
            vec![],
        )
        .expect("allow-listed env key reads");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "yes");

        let denied = write_source(
            &dir,
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_SECRET\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        );
        let err = run_build_step_file_with_env(
            denied.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            env,
            vec![],
            vec![],
        )
        .expect_err("unlisted env key is refused");
        assert!(err.contains("not in this BuildEnv grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compiled_build_net_fetches_only_allow_listed_hosts() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let body = "schema-v1";
            let _ = sock.write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes(),
            );
        });

        let dir = unique("compiled_build_net");
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_source(
            &dir,
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"got.txt\", dl.fetch_build(\"{addr}\", \"/schema\"))\n"
            ),
        );
        run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            vec![],
            vec![],
            vec![addr.clone()],
        )
        .expect("allow-listed fetch runs");
        assert_eq!(std::fs::read_to_string(dir.join("out/got.txt")).unwrap(), "schema-v1");
        server.join().unwrap();

        let err = run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            vec![],
            vec![],
            vec!["allowed.example:80".to_string()],
        )
        .expect_err("unlisted host is refused");
        assert!(err.contains("not in this BuildNet grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compiled_build_exec_runs_only_allow_listed_tools() {
        let dir = unique("compiled_build_exec");
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_source(
            &dir,
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"cat\", \"piped-input\"))\n",
        );
        run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            vec![],
            vec!["cat".to_string()],
            vec![],
        )
        .expect("cat is allow-listed");
        assert_eq!(std::fs::read_to_string(dir.join("out/x.txt")).unwrap(), "piped-input");

        let denied = write_source(
            &dir,
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"rm\", \"-rf /\"))\n",
        );
        let err = run_build_step_file(
            denied.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            vec![],
            vec!["cat".to_string()],
            vec![],
        )
        .expect_err("unlisted tool is refused");
        assert!(err.contains("not in this BuildExec grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
