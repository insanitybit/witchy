//! Canonical BuildRead: an instance-bound grant and a single-read immutable
//! response. Neither a path nor a response can mint filesystem authority.
//! Kind13 also carries BuildEnv lookup results; None is not an empty string.
use std::sync::Arc;
use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use super::super::{memory_of, read_slice_str, VmState};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    read: bool,
    roots: Arc<[crate::confine::ConfinedDir]>,
}

#[derive(Clone, Debug)]
struct Response {
    issuer: Arc<()>,
    bytes: Option<Arc<[u8]>>,
}

pub(in crate::runtime) fn mint(state: &VmState) -> Result<Authority> {
    if state.build_read_roots.is_empty() {
        bail!("build step demands `BuildRead` but no read grant was provided");
    }
    Ok(Authority {
        issuer: state.build_read_issuer.clone(), read: true,
        roots: state.build_read_roots.clone().into(),
    })
}

fn authenticate<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| Error::msg("read_build: wrong capability kind"))?;
    if !Arc::ptr_eq(issuer, &authority.issuer) {
        bail!("read_build: capability belongs to another instance");
    }
    if !authority.read { bail!("read_build: capability lacks read right"); }
    Ok(authority)
}

fn authenticate_response<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Response> {
    let response = data.downcast_ref::<Response>()
        .ok_or_else(|| Error::msg("BuildRead response has wrong host kind"))?;
    if !Arc::ptr_eq(issuer, &response.issuer) {
        bail!("BuildRead response belongs to another instance");
    }
    Ok(response)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(&state.build_read_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn authenticate_response_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate_response(&state.build_read_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_read_at", read_at)?;
    linker.func_wrap("witchy", "build_read_response_size", response_size)?;
    linker.func_wrap("witchy", "build_read_response_into", response_into)?;
    linker.func_wrap("witchy", "build_response_is_some", response_is_some)?;
    Ok(())
}

fn checked_response_len(len: usize) -> Result<i32> {
    i32::try_from(len).map_err(|_| Error::msg("BuildRead response exceeds guest ABI size limit"))
}

/// Only host-produced UTF-8 values enter the shared, authority-free carrier.
/// The issuer is taken from the destination Store, never a caller-supplied token.
pub(super) fn new_response(
    mut store: impl wasmtime::AsContextMut<Data = VmState>, value: Option<&str>,
) -> Result<Rooted<ExternRef>> {
    let bytes = value.map(|text| {
        checked_response_len(text.len())?;
        Ok::<Arc<[u8]>, Error>(Arc::from(text.as_bytes()))
    }).transpose()?;
    let issuer = store.as_context().data().build_read_issuer.clone();
    ExternRef::new(&mut store, Response { issuer, bytes })
}

fn read_at(
    mut caller: Caller<'_, VmState>, grant: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let grant = grant.ok_or_else(|| Error::msg("read_build: null capability"))?;
    let data = grant.data(&caller)?.ok_or_else(|| Error::msg("read_build: missing capability data"))?;
    let authority = authenticate(&caller.data().build_read_issuer, data)?.clone();
    // Authenticate kind, instance and Read before memory, then copy the whole
    // strict UTF-8 path before the first filesystem observation. No reentry.
    let memory = memory_of(&mut caller)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    let mut last = String::from("no granted read root");
    for root in authority.roots.iter() {
        match root.file(&path, true) {
            Ok(file) => match file.read_to_string() {
                Ok(contents) => {
                    return new_response(&mut caller, Some(&contents)).map(Some);
                }
                Err(error) => last = format!("`{}`: {error}", file.display_path().display()),
            },
            Err(error) => last = error.message,
        }
    }
    bail!("read_build: `{path}` not found in any granted read root ({last})")
}

fn response_value(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Arc<[u8]>>> {
    let token = token.ok_or_else(|| Error::msg("BuildRead response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("BuildRead response has no host data"))?;
    Ok(authenticate_response(&caller.data().build_read_issuer, data)?.bytes.clone())
}

fn response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Arc<[u8]>> {
    response_value(caller, token)?.ok_or_else(|| Error::msg("Build response is None"))
}

fn response_is_some(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(response_value(&caller, token)?.is_some()))
}

fn response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    checked_response_len(response(&caller, token)?.len())
}

fn output_range(ptr: i32, capacity: i32, limit: usize) -> Result<std::ops::Range<usize>> {
    let start = usize::try_from(ptr).map_err(|_| Error::msg("negative BuildRead output pointer"))?;
    let capacity = usize::try_from(capacity).map_err(|_| Error::msg("negative BuildRead output capacity"))?;
    let end = start.checked_add(capacity).filter(|end| *end <= limit)
        .ok_or_else(|| Error::msg("BuildRead output slice lies outside guest memory"))?;
    Ok(start..end)
}

fn response_into(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>,
    out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = response(&caller, token)?;
    let required = checked_response_len(bytes.len())?;
    let memory = memory_of(&mut caller)?;
    let output = output_range(out_ptr, capacity, memory.data_size(&caller))?;
    // Preflight the entire declared output range even when short or empty.
    // Queries are repeatable: no pending buffer, reread, or filesystem effect.
    if output.len() >= bytes.len() {
        memory.write(&mut caller, output.start, &bytes)?;
    }
    Ok(required)
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "build_read_tests.rs"]
pub mod tests;
