//! Build-only plaintext fetches through an instance-bound exact host grant.
//! Responses use the shared immutable carrier, never pending guest buffers.
use std::sync::Arc;
use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use super::super::{memory_of, read_slice_str, VmState};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    read: bool,
    hosts: Arc<[String]>,
}

pub(in crate::runtime) fn mint(state: &VmState) -> Result<Authority> {
    let hosts = state.caps.build_net_allow.as_ref()
        .ok_or_else(|| Error::msg("fetch_build: no BuildNet grant"))?;
    // Some(empty) is an explicit zero-authority root. Neither ordinary Net
    // grants nor later changes to the host's input table widen this snapshot.
    Ok(Authority {
        issuer: state.build_net_issuer.clone(), read: true,
        hosts: hosts.clone().into(),
    })
}

fn authenticate<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| Error::msg("fetch_build: wrong capability kind"))?;
    if !Arc::ptr_eq(issuer, &authority.issuer) {
        bail!("fetch_build: capability belongs to another instance");
    }
    if !authority.read { bail!("fetch_build: capability lacks read right"); }
    Ok(authority)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(&state.build_net_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    // Linkage exposes no mint operation and grants no network authority.
    linker.func_wrap("witchy", "build_net_fetch", fetch)?;
    Ok(())
}

fn fetch(
    mut caller: Caller<'_, VmState>, grant: Option<Rooted<ExternRef>>,
    host_ptr: i32, host_len: i32, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let grant = grant.ok_or_else(|| Error::msg("fetch_build: null capability"))?;
    let data = grant.data(&caller)?.ok_or_else(|| Error::msg("fetch_build: missing capability data"))?;
    let authority = authenticate(&caller.data().build_net_issuer, data)?.clone();
    // Authenticate kind, current issuer and Read before memory; copy and
    // strictly decode both full slices before DNS, connection or request I/O.
    let memory = memory_of(&mut caller)?;
    let host = read_slice_str(memory.data(&caller), host_ptr, host_len)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    if !authority.hosts.iter().any(|allowed| allowed == &host) {
        bail!("fetch_build: `{host}` is not in this BuildNet grant's allow-list");
    }

    // Preserve fetch_build's existing wire semantics, not ordinary Fetch:
    // plaintext GET, literal path/Host construction, EOF framing, no status,
    // redirect, TLS or transfer-encoding interpretation and no added retry.
    use std::io::{Read as _, Write as _};
    let mut socket = std::net::TcpStream::connect(host.as_str())
        .map_err(|error| Error::msg(format!("fetch_build: cannot connect to `{host}`: {error}")))?;
    let hostname = host.split(':').next().unwrap_or(host.as_str());
    let request = format!("GET {path} HTTP/1.1\r\nHost: {hostname}\r\nConnection: close\r\n\r\n");
    socket.write_all(request.as_bytes())
        .map_err(|error| Error::msg(format!("fetch_build: sending to `{host}`: {error}")))?;
    let mut raw = Vec::new();
    socket.read_to_end(&mut raw)
        .map_err(|error| Error::msg(format!("fetch_build: reading from `{host}`: {error}")))?;
    let text = String::from_utf8_lossy(&raw);
    let body = text.split_once("\r\n\r\n").map_or(text.as_ref(), |(_, body)| body);
    // This checks the UTF-8 byte length against i32 before allocating the
    // response. Later size/copy calls cannot connect, reread or refetch.
    super::build_read::new_response(&mut caller, Some(body)).map(Some)
}

#[cfg(test)]
#[path = "build_net_tests.rs"]
pub(super) mod tests;
