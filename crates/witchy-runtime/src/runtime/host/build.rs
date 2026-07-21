use wasmtime::{Caller, Error, Linker, Result};

use super::super::{confine, memory_of, read_wstr, VmState};

/// Register the BuildOut write import.
pub(in crate::runtime) fn link_out(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_out_write", host_build_out_write)?;
    Ok(())
}

/// Register the BuildRead size import. `read_build` is staged like `dir_read`;
/// `fill_pending` (linked unconditionally by the kernel) does the transfer.
pub(in crate::runtime) fn link_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_read_len", host_build_read_len)?;
    Ok(())
}

/// Register the BuildEnv lookup imports.
pub(in crate::runtime) fn link_env(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_env_len", host_build_env_len)?;
    linker.func_wrap("witchy", "build_env_fill", host_build_env_fill)?;
    Ok(())
}

/// Register the BuildNet fetch import.
pub(in crate::runtime) fn link_fetch(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_fetch_len", host_build_fetch_len)?;
    Ok(())
}

/// Register the build Exec import.
pub(in crate::runtime) fn link_exec(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_exec_run", host_build_exec_run)?;
    Ok(())
}

// --- build-time host ops ---
//
// The build sandbox grants zero-representation capabilities: source must hold
// `BuildOut`/`BuildRead`/`BuildEnv`/`BuildNet`/`BuildExec` to call these ops, but
// no guest value crosses the host boundary. The host import is linked only when
// the corresponding grant exists, and the grant material stays in `VmState`.
// `write_out` confines through the same `resolve_write` as a runtime `Dir`;
// `read_build` tries each granted root, matching the interpreter.

/// `build_out_write(name, contents)`: write generated source into the build
/// output sandbox (trap on escape — a `..` can't leave it).
fn host_build_out_write(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let name = read_wstr(data, name_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let base = caller
        .data()
        .build_out
        .clone()
        .ok_or_else(|| Error::msg("write_out: no BuildOut grant"))?;
    let path = confine(crate::confine::resolve_write(&base, &name))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, contents)
        .map_err(|e| Error::msg(format!("write_out failed for `{}`: {e}", path.display())))
}

/// `build_read_len(rel) -> byte length`: read the file from the first granted
/// read root that holds it, stage its bytes, and report the length; the guest
/// allocates and calls `fill_pending` (identical staging to `dir_read_len`).
fn host_build_read_len(mut caller: Caller<'_, VmState>, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let roots = caller.data().build_read_roots.clone();
    let mut last = String::from("no granted read root");
    for base in &roots {
        match crate::confine::resolve(base, &rel) {
            Ok(path) => match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let len = contents.len() as i32;
                    caller.data_mut().pending = Some(contents.into_bytes());
                    return Ok(len);
                }
                Err(e) => last = format!("`{}`: {e}", path.display()),
            },
            Err(e) => last = e.0,
        }
    }
    Err(Error::msg(format!("read_build: `{rel}` not found in any granted read root ({last})")))
}

fn host_build_env_len(mut caller: Caller<'_, VmState>, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let env = caller
        .data()
        .caps
        .build_env
        .as_ref()
        .ok_or_else(|| Error::msg("get_build_env: no BuildEnv grant"))?;
    let value = env.get(&name).ok_or_else(|| {
        Error::msg(format!(
            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
        ))
    })?;
    match value {
        Some(value) => checked_build_env_len(&name, value.len()),
        None => Ok(-1),
    }
}

fn checked_build_env_len(name: &str, len: usize) -> Result<i32> {
    let len = i32::try_from(len)
        .map_err(|_| Error::msg(format!("get_build_env: `{name}` exceeds the guest ABI size limit")))?;
    debug_assert!(len >= 0, "a checked byte length must not collide with the unset sentinel");
    Ok(len)
}

fn host_build_env_fill(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let env = caller
        .data()
        .caps
        .build_env
        .as_ref()
        .ok_or_else(|| Error::msg("get_build_env: no BuildEnv grant"))?;
    let value = env.get(&name).ok_or_else(|| {
        Error::msg(format!(
            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
        ))
    })?;
    let bytes = value.as_deref().unwrap_or_default().as_bytes().to_vec();
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing build env value into guest memory: {e}")))
}

fn host_build_fetch_len(
    mut caller: Caller<'_, VmState>,
    host_ptr: i32,
    path_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let host = read_wstr(data, host_ptr)?;
    let path = read_wstr(data, path_ptr)?;
    let allow = caller
        .data()
        .caps
        .build_net_allow
        .as_ref()
        .ok_or_else(|| Error::msg("fetch_build: no BuildNet grant"))?;
    if !allow.iter().any(|h| h == &host) {
        return Err(Error::msg(format!(
            "fetch_build: `{host}` is not in this BuildNet grant's allow-list"
        )));
    }

    use std::io::{Read as _, Write as _};
    let mut sock = std::net::TcpStream::connect(host.as_str())
        .map_err(|e| Error::msg(format!("fetch_build: cannot connect to `{host}`: {e}")))?;
    let hostname = host.split(':').next().unwrap_or(host.as_str());
    let req = format!("GET {path} HTTP/1.1\r\nHost: {hostname}\r\nConnection: close\r\n\r\n");
    sock.write_all(req.as_bytes())
        .map_err(|e| Error::msg(format!("fetch_build: sending to `{host}`: {e}")))?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw)
        .map_err(|e| Error::msg(format!("fetch_build: reading from `{host}`: {e}")))?;
    let text = String::from_utf8_lossy(&raw);
    let body = match text.split_once("\r\n\r\n") {
        Some((_, b)) => b.to_string(),
        None => text.into_owned(),
    };
    let len = body.len() as i32;
    caller.data_mut().pending = Some(body.into_bytes());
    Ok(len)
}

fn host_build_exec_run(
    mut caller: Caller<'_, VmState>,
    tool_ptr: i32,
    input_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let tool = read_wstr(data, tool_ptr)?;
    let input = read_wstr(data, input_ptr)?;
    let allow = caller
        .data()
        .caps
        .exec_allow
        .as_ref()
        .ok_or_else(|| Error::msg("run_tool: no BuildExec grant"))?;
    if !allow.iter().any(|t| t == &tool) {
        return Err(Error::msg(format!(
            "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
        )));
    }

    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&tool)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("run_tool: cannot start `{tool}`: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| Error::msg(format!("run_tool: writing to `{tool}` stdin: {e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::msg(format!("run_tool: `{tool}` failed: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!("run_tool: `{tool}` exited with {}", out.status)));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let len = stdout.len() as i32;
    caller.data_mut().pending = Some(stdout.into_bytes());
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::checked_build_env_len;

    #[test]
    fn build_env_length_rejects_values_outside_the_guest_abi() {
        assert_eq!(checked_build_env_len("SMALL", 42).unwrap(), 42);
        let err = checked_build_env_len("HUGE", usize::MAX)
            .expect_err("a host length must not truncate into the guest ABI");
        assert!(err.to_string().contains("HUGE"), "{err}");
        assert!(err.to_string().contains("ABI size limit"), "{err}");
    }
}
