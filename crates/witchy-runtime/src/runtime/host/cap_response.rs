//! The one immutable response every fallible capability operation answers
//! (RFC-0126 rule 3).
//!
//! A `Dir`, `File`, `Env`, `Net` or `Secret` operation consults its grant and
//! the host once, then answers with a response: a status (0 success, else the
//! operation's failure code), a payload (the contents, or a guest-safe failure
//! message), an optional predicate answer, and on success whatever capability
//! the operation minted. The getters only borrow that snapshot and exercise no
//! authority: a response exists only because an authorized operation issued it.
use std::sync::Arc;

use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use witchy_wir::canonical_safewir::CAP_RESPONSE_IMPORT_NAMES;

use super::super::{
    memory_of, DirAuthority, FileAuthority, ListenerResource, NetAuthority, SocketResource, VmState,
};

/// Private immutable operation result, not a grant. Neither an integer kind
/// nor bytes supplied by the guest can construct one, and it answers only to
/// the instance that issued it.
#[derive(Clone)]
pub(in crate::runtime) struct CapResponse {
    issuer: Arc<()>,
    status: u8,
    bytes: Arc<Vec<u8>>,
    minted: Option<Minted>,
    flag: bool,
}

/// The capability a successful operation minted, taken by its getter.
#[derive(Clone)]
pub(in crate::runtime) enum Minted {
    Dir(DirAuthority),
    File(FileAuthority),
    Net(NetAuthority),
    Socket(SocketResource),
    Listener(ListenerResource),
}

/// Register the response getters. They grant nothing, so they link
/// unconditionally: without an operation that issued one, no response exists.
pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    let [status, size, into, flag, take_dir, take_file, take_net, take_socket, take_listener] = CAP_RESPONSE_IMPORT_NAMES;
    linker.func_wrap("witchy", status, host_status)?;
    linker.func_wrap("witchy", size, host_size)?;
    linker.func_wrap("witchy", into, host_into)?;
    linker.func_wrap("witchy", flag, host_flag)?;
    linker.func_wrap("witchy", take_dir, |caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>| take(caller, token, "Dir", |m| matches!(m, Minted::Dir(_))))?;
    linker.func_wrap("witchy", take_file, |caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>| take(caller, token, "File", |m| matches!(m, Minted::File(_))))?;
    linker.func_wrap("witchy", take_net, |caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>| take(caller, token, "Net", |m| matches!(m, Minted::Net(_))))?;
    linker.func_wrap("witchy", take_socket, |caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>| take(caller, token, "Socket", |m| matches!(m, Minted::Socket(_))))?;
    linker.func_wrap("witchy", take_listener, |caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>| take(caller, token, "Listener", |m| matches!(m, Minted::Listener(_))))?;
    Ok(())
}

/// Issue a response: `status` with `bytes` and, on success, what the
/// operation minted.
pub(in crate::runtime) fn respond(
    caller: &mut Caller<'_, VmState>, status: u8, bytes: Vec<u8>, minted: Option<Minted>,
) -> Result<Option<Rooted<ExternRef>>> {
    issue(caller, status, bytes, minted, false)
}

/// A failure response: `status` with its guest-visible message.
pub(in crate::runtime) fn fail(caller: &mut Caller<'_, VmState>, status: u8, message: String) -> Result<Option<Rooted<ExternRef>>> {
    issue(caller, status, message.into_bytes(), None, false)
}

/// A predicate response: `status` with `bytes` and the answer `flag`.
pub(in crate::runtime) fn respond_flag(
    caller: &mut Caller<'_, VmState>, status: u8, bytes: Vec<u8>, flag: bool,
) -> Result<Option<Rooted<ExternRef>>> {
    issue(caller, status, bytes, None, flag)
}

fn issue(
    caller: &mut Caller<'_, VmState>, status: u8, bytes: Vec<u8>, minted: Option<Minted>, flag: bool,
) -> Result<Option<Rooted<ExternRef>>> {
    length(bytes.len())?;
    let response = CapResponse { issuer: caller.data().registry_issuer.clone(), status, bytes: Arc::new(bytes), minted, flag };
    ExternRef::new(caller, response).map(Some)
}

fn length(bytes: usize) -> Result<i32> {
    i32::try_from(bytes).map_err(|_| Error::msg("capability response exceeds the guest ABI size limit"))
}

/// The response `token` names. A null, foreign-kind, or another instance's
/// token traps.
pub(in crate::runtime) fn response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<CapResponse> {
    let token = token.ok_or_else(|| Error::msg("capability response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("capability response has no host data"))?;
    let response = data.downcast_ref::<CapResponse>()
        .ok_or_else(|| Error::msg("capability response has wrong host kind"))?;
    if !Arc::ptr_eq(&caller.data().registry_issuer, &response.issuer) {
        return Err(Error::msg("capability response belongs to another instance"));
    }
    Ok(response.clone())
}

fn host_status(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(response(&caller, token)?.status))
}

fn host_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    length(response(&caller, token)?.bytes.len())
}

/// Copy the whole payload when `capacity` holds it and answer its length
/// either way. The buffer is preflighted in full and a short one is left
/// untouched: no partial copy, no consumed state.
fn host_into(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, out_ptr: i32, capacity: i32) -> Result<i32> {
    let bytes = response(&caller, token)?.bytes;
    let required = length(bytes.len())?;
    let start = usize::try_from(out_ptr).map_err(|_| Error::msg("negative capability response pointer"))?;
    let room = usize::try_from(capacity).map_err(|_| Error::msg("negative capability response capacity"))?;
    let memory = memory_of(&mut caller)?;
    let end = start.checked_add(room).filter(|end| *end <= memory.data_size(&caller))
        .ok_or_else(|| Error::msg("capability response buffer lies outside guest memory"))?;
    if end - start >= bytes.len() {
        memory.write(&mut caller, start, bytes.as_slice())?;
    }
    Ok(required)
}

/// A predicate response's answer (`exists`, `is_dir`).
fn host_flag(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(response(&caller, token)?.flag))
}

/// The capability a successful operation minted, when it is the `kind` the
/// getter names. Any other response traps; the guest takes only on status 0.
fn take(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, kind: &str, is: fn(&Minted) -> bool,
) -> Result<Option<Rooted<ExternRef>>> {
    let minted = response(&caller, token)?.minted.filter(is)
        .ok_or_else(|| Error::msg(format!("capability response carries no {kind}")))?;
    match minted {
        Minted::Dir(dir) => ExternRef::new(&mut caller, dir),
        Minted::File(file) => ExternRef::new(&mut caller, file),
        Minted::Net(net) => ExternRef::new(&mut caller, net),
        Minted::Socket(socket) => ExternRef::new(&mut caller, socket),
        Minted::Listener(listener) => ExternRef::new(&mut caller, listener),
    }.map(Some)
}
