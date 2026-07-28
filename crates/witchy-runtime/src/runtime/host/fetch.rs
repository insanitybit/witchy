use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{memory_of, read_wstr, VmState};
use crate::fetch::{FetchPolicy, FetchRequest};

pub(in crate::runtime) fn link_mint(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_fetch", host_mint_fetch)?;
    Ok(())
}

pub(in crate::runtime) fn link_ops(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "fetch_only", host_fetch_only)?;
    linker.func_wrap("witchy", "fetch_send_len", host_fetch_send_len)?;
    Ok(())
}

fn fetch_authority_ref(
    caller: &Caller<'_, VmState>,
    fetch: Option<Rooted<ExternRef>>,
) -> Result<FetchPolicy> {
    let fetch = fetch.ok_or_else(|| Error::msg("Fetch externref is null"))?;
    fetch
        .data(caller)?
        .ok_or_else(|| Error::msg("Fetch externref has no host data"))?
        .downcast_ref::<FetchPolicy>()
        .cloned()
        .ok_or_else(|| Error::msg("Fetch externref has wrong host data"))
}

fn host_mint_fetch(
    mut caller: Caller<'_, VmState>,
    ordinal: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let origins = caller
        .data()
        .fetch_grants
        .get(ordinal as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Fetch grant index {ordinal}")))?;
    let policy = FetchPolicy::allow(origins)
        .map_err(|error| Error::msg(format!("invalid Fetch grant: {error}")))?;
    ExternRef::new(&mut caller, policy).map(Some)
}

fn host_fetch_only(
    mut caller: Caller<'_, VmState>,
    fetch: Option<Rooted<ExternRef>>,
    origins_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let origins = read_wstr(mem.data(&caller), origins_ptr)?;
    let policy = fetch_authority_ref(&caller, fetch)?;
    let narrowed = policy
        .only(origins.lines().map(str::to_owned))
        .map_err(|error| Error::msg(format!("fetch.only: {error}")))?;
    ExternRef::new(&mut caller, narrowed).map(Some)
}

fn host_fetch_send_len(
    mut caller: Caller<'_, VmState>,
    fetch: Option<Rooted<ExternRef>>,
    method_ptr: i32,
    url_ptr: i32,
    headers_ptr: i32,
    body_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let method = read_wstr(data, method_ptr)?;
    let url = read_wstr(data, url_ptr)?;
    let headers = read_wstr(data, headers_ptr)?;
    let body = read_wstr(data, body_ptr)?;
    let policy = fetch_authority_ref(&caller, fetch)?;
    let request = FetchRequest {
        method,
        url,
        headers: headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect(),
        body: body.into_bytes(),
    };
    let payload = match crate::fetch::send(&policy, &request) {
        Ok(response) => {
            let mut raw = format!("HTTP/1.1 {}\r\n", response.status);
            for (name, value) in response.headers {
                raw.push_str(&name);
                raw.push_str(": ");
                raw.push_str(&value);
                raw.push_str("\r\n");
            }
            raw.push_str("\r\n");
            raw.push_str(&String::from_utf8_lossy(&response.body));
            raw
        }
        Err(error) => format!("WITCHY_FETCH_ERROR:{}:{error}", error.code()),
    };
    let len = i32::try_from(payload.len())
        .map_err(|_| Error::msg("Fetch response exceeds the guest ABI size limit"))?;
    caller.data_mut().pending = Some(payload.into_bytes());
    Ok(len)
}
