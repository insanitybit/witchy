//! Canonical Fetch retains the existing HTTP provider, not BuildNet's wire protocol.
use std::sync::Arc;
use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{memory_of, read_slice_str, read_wstr, NetAuthority, VmState};
use crate::fetch::{FetchPolicy, FetchRequest};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    policy: FetchPolicy,
}

#[derive(Clone, Debug)]
struct Response {
    issuer: Arc<()>,
    bytes: Arc<[u8]>,
}

pub(in crate::runtime) fn mint(state: &VmState, ordinal: usize) -> Result<Authority> {
    let origins = state.fetch_grants.get(ordinal)
        .ok_or_else(|| Error::msg(format!("invalid Fetch grant index {ordinal}")))?;
    let policy = FetchPolicy::allow(origins.clone())
        .map_err(|error| Error::msg(format!("invalid Fetch grant: {error}")))?;
    Ok(Authority { issuer: state.fetch_issuer.clone(), policy })
}

fn authenticate<'a>(state: &VmState, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| Error::msg("Fetch has wrong capability kind"))?;
    if !Arc::ptr_eq(&state.fetch_issuer, &authority.issuer) {
        bail!("Fetch capability belongs to another instance");
    }
    Ok(authority)
}

fn authenticate_response<'a>(state: &VmState, data: &'a dyn std::any::Any) -> Result<&'a Response> {
    let response = data.downcast_ref::<Response>()
        .ok_or_else(|| Error::msg("Fetch response has wrong host kind"))?;
    if !Arc::ptr_eq(&state.fetch_issuer, &response.issuer) {
        bail!("Fetch response belongs to another instance");
    }
    Ok(response)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(state, data).map(|_| ())
}

pub(in crate::runtime) fn authenticate_response_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate_response(state, data).map(|_| ())
}

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_fetch_at", net_fetch_at)?;
    linker.func_wrap("witchy", "fetch_only_at", fetch_only_at)?;
    linker.func_wrap("witchy", "fetch_send_at", fetch_send_at)?;
    linker.func_wrap("witchy", "fetch_response_size", response_size)?;
    linker.func_wrap("witchy", "fetch_response_into", response_into)?;
    Ok(())
}

pub(in crate::runtime) fn link_mint(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_fetch", host_mint_fetch)?;
    Ok(())
}

pub(in crate::runtime) fn link_ops(linker: &mut Linker<VmState>) -> Result<()> {
    // Only the existing raw development narrowing wrapper remains here.
    linker.func_wrap("witchy", "fetch_only", host_fetch_only)?;
    Ok(())
}

fn fetch_authority_ref(caller: &Caller<'_, VmState>, fetch: Option<Rooted<ExternRef>>) -> Result<Authority> {
    let fetch = fetch.ok_or_else(|| Error::msg("Fetch externref is null"))?;
    let data = fetch.data(caller)?
        .ok_or_else(|| Error::msg("Fetch externref has no host data"))?;
    authenticate(caller.data(), data).cloned()
}

fn host_mint_fetch(mut caller: Caller<'_, VmState>, ordinal: i32) -> Result<Option<Rooted<ExternRef>>> {
    let ordinal = usize::try_from(ordinal).map_err(|_| Error::msg("negative Fetch grant index"))?;
    let authority = mint(caller.data(), ordinal)?;
    ExternRef::new(&mut caller, authority).map(Some)
}

fn host_fetch_only(
    mut caller: Caller<'_, VmState>, fetch: Option<Rooted<ExternRef>>, origins_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let authority = fetch_authority_ref(&caller, fetch)?;
    let mem = memory_of(&mut caller)?;
    let origins = read_wstr(mem.data(&caller), origins_ptr)?;
    let narrowed = authority.policy.only(origins.lines().map(str::to_owned))
        .map_err(|error| Error::msg(format!("fetch.only: {error}")))?;
    ExternRef::new(&mut caller, Authority { policy: narrowed, ..authority }).map(Some)
}

fn fetch_only_at(
    mut caller: Caller<'_, VmState>, fetch: Option<Rooted<ExternRef>>,
    origins_ptr: i32, origins_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let authority = fetch_authority_ref(&caller, fetch)?;
    let memory = memory_of(&mut caller)?;
    let origins = read_slice_str(memory.data(&caller), origins_ptr, origins_len)?;
    // The existing policy implements exact subset checks and retains the Net
    // floor and transport limits. Narrowing keeps the authenticated issuer.
    let policy = authority.policy.only(origins.lines().map(str::to_owned))
        .map_err(|error| Error::msg(format!("fetch.only: {error}")))?;
    ExternRef::new(&mut caller, Authority { policy, ..authority }).map(Some)
}

fn net_fetch_at(
    mut caller: Caller<'_, VmState>, net: Option<Rooted<ExternRef>>,
    origins_ptr: i32, origins_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let token = net.ok_or_else(|| Error::msg("Net externref is null"))?;
    let data = token.data(&caller)?.ok_or_else(|| Error::msg("Net externref has no host data"))?;
    let net = data.downcast_ref::<NetAuthority>()
        .ok_or_else(|| Error::msg("net.fetch: wrong capability kind"))?;
    if !Arc::ptr_eq(&caller.data().fetch_issuer, &net.issuer) {
        bail!("net.fetch: Net capability belongs to another instance");
    }
    if !caller.data().caps.net_connect { bail!("net.fetch: Net lacks Connect right"); }
    let allow = net.allow.clone();
    let memory = memory_of(&mut caller)?;
    let origins = read_slice_str(memory.data(&caller), origins_ptr, origins_len)?;
    let policy = FetchPolicy::from_net(&allow, origins.lines().map(str::to_owned))
        .map_err(|error| Error::msg(format!("net.fetch: {error}")))?;
    let authority = Authority { issuer: caller.data().fetch_issuer.clone(), policy };
    ExternRef::new(&mut caller, authority).map(Some)
}

fn fetch_send_at(
    mut caller: Caller<'_, VmState>, fetch: Option<Rooted<ExternRef>>,
    method_ptr: i32, method_len: i32, url_ptr: i32, url_len: i32,
    headers_ptr: i32, headers_len: i32, body_ptr: i32, body_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let authority = fetch_authority_ref(&caller, fetch)?;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    // Snapshot ALL four strict UTF-8 ranges before any external effect.
    let method = read_slice_str(data, method_ptr, method_len)?;
    let url = read_slice_str(data, url_ptr, url_len)?;
    let headers = read_slice_str(data, headers_ptr, headers_len)?;
    let body = read_slice_str(data, body_ptr, body_len)?;
    let request = FetchRequest {
        method, url,
        headers: headers.lines().filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string())).collect(),
        body: body.into_bytes(),
    };
    let payload = match crate::fetch::send(&authority.policy, &request) {
        Ok(response) => {
            let mut raw = format!("HTTP/1.1 {}\r\n", response.status);
            for (name, value) in response.headers {
                raw.push_str(&name);
                raw.push_str(": ");
                raw.push_str(&value);
                raw.push_str("\r\n");
            }
            raw.push_str("\r\n");
            // The provider has already transfer-decoded. The source Fetch
            // response parser consumes this payload without decoding it again.
            raw.push_str(&String::from_utf8_lossy(&response.body));
            raw
        }
        Err(error) => format!("WITCHY_FETCH_ERROR:{}:{error}", error.code()),
    };
    new_response(&mut caller, &payload).map(Some)
}

fn checked_response_len(len: usize) -> Result<i32> {
    i32::try_from(len).map_err(|_| Error::msg("Fetch response exceeds the guest ABI size limit"))
}

pub(super) fn new_response(
    mut store: impl wasmtime::AsContextMut<Data = VmState>, payload: &str,
) -> Result<Rooted<ExternRef>> {
    checked_response_len(payload.len())?;
    let response = Response {
        issuer: store.as_context().data().fetch_issuer.clone(),
        bytes: Arc::from(payload.as_bytes()),
    };
    ExternRef::new(&mut store, response)
}

fn response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Arc<[u8]>> {
    let token = token.ok_or_else(|| Error::msg("Fetch response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("Fetch response has no host data"))?;
    Ok(authenticate_response(caller.data(), data)?.bytes.clone())
}

fn response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    checked_response_len(response(&caller, token)?.len())
}

fn response_into(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>,
    out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = response(&caller, token)?;
    let required = checked_response_len(bytes.len())?;
    let memory = memory_of(&mut caller)?;
    let start = usize::try_from(out_ptr).map_err(|_| Error::msg("negative Fetch output pointer"))?;
    let capacity = usize::try_from(capacity).map_err(|_| Error::msg("negative Fetch output capacity"))?;
    start.checked_add(capacity).filter(|end| *end <= memory.data_size(&caller))
        .ok_or_else(|| Error::msg("Fetch output slice lies outside guest memory"))?;
    // Full declared range preflight; only the required prefix is initialized.
    // Short capacity writes nothing, and the immutable result is never consumed.
    if capacity >= bytes.len() { memory.write(&mut caller, start, &bytes)?; }
    Ok(required)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
pub(super) mod tests;
