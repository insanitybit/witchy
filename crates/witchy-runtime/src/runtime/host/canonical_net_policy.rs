//! Canonical Net responses and refinement (RFC-0126 rule 3). Every fallible
//! Net operation answers with an immutable response; `net.deny` only removes
//! endpoints, so it answers with the narrowed `Net` directly.
use super::*;
use super::super::super::read_slice_str;
use witchy_wir::canonical_safewir::NET_RESPONSE_IMPORT_NAMES;

pub(super) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[0], only_at)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[1], deny_at)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[2], response_status)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[3], response_size)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[4], response_into)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[5], response_take_net)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[6], response_take_socket)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[7], response_take_listener)?;
    Ok(())
}

/// The `CapError::Denied` status (its variant ordinal plus one).
pub(super) const DENIED: u8 = 1;
/// The `CapError::AlreadyExists` status.
pub(super) const ALREADY_EXISTS: u8 = 4;
/// The `CapError::Io` status.
pub(super) const IO: u8 = 5;

/// Private immutable result of a fallible Net operation, not a Net grant.
/// Neither an integer kind nor bytes supplied by the guest can construct one.
/// `status` 0 carries the payload and whatever the operation minted; any
/// other is a `CapError` status carrying the failure message.
#[derive(Clone)]
pub(in crate::runtime) struct NetResponse {
    issuer: Arc<()>,
    status: u8,
    bytes: Arc<Vec<u8>>,
    minted: Option<Minted>,
}

#[derive(Clone)]
pub(super) enum Minted {
    Net(NetAuthority),
    Socket(SocketResource),
    Listener(ListenerResource),
}

pub(super) fn respond(caller: &mut Caller<'_, VmState>, status: u8, bytes: Vec<u8>, minted: Option<Minted>) -> Result<Option<Rooted<ExternRef>>> {
    response_length(bytes.len())?;
    let response = NetResponse { issuer: caller.data().registry_issuer.clone(), status, bytes: Arc::new(bytes), minted };
    ExternRef::new(caller, response).map(Some)
}

/// A failure response: `status` with its guest-visible message.
pub(super) fn fail(caller: &mut Caller<'_, VmState>, status: u8, message: String) -> Result<Option<Rooted<ExternRef>>> {
    respond(caller, status, message.into_bytes(), None)
}

fn response_length(length: usize) -> Result<i32> {
    i32::try_from(length).map_err(|_| Error::msg("Net response exceeds the guest ABI size limit"))
}

fn net_response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<NetResponse> {
    let token = token.ok_or_else(|| Error::msg("Net response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("Net response has no host data"))?;
    let response = data.downcast_ref::<NetResponse>()
        .ok_or_else(|| Error::msg("Net response has wrong host kind"))?;
    if !Arc::ptr_eq(&caller.data().registry_issuer, &response.issuer) {
        return Err(Error::msg("Net response belongs to another instance"));
    }
    Ok(response.clone())
}

pub(super) fn slice_text(caller: &mut Caller<'_, VmState>, ptr: i32, len: i32) -> Result<String> {
    let memory = memory_of(caller)?;
    read_slice_str(memory.data(&*caller), ptr, len)
}

/// `net_only_at(net, policy) -> response`: narrow to the policy's endpoints.
/// Each must already be admitted, and endpoints the receiver denies stay
/// denied. The decision reads only the grant, never DNS or a socket, and a
/// refusal names only the pattern the guest supplied.
fn only_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: i32, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    let policy = slice_text(&mut caller, ptr, len)?;
    match witchy_caps::capabilities::net_only(&net.allow, &policy) {
        Ok(allow) => respond(&mut caller, 0, Vec::new(), Some(Minted::Net(NetAuthority { allow, issuer: net.issuer }))),
        Err(pattern) => fail(&mut caller, DENIED, format!("`{pattern}` is not covered by this Net capability")),
    }
}

/// `net_deny_at(net, policy) -> Net`: subtract the policy's endpoints, a
/// monotone exclusion the shared `net_allows` honours.
fn deny_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: i32, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let NetAuthority { mut allow, issuer } = net_authority_ref(&caller, token)?;
    let policy = slice_text(&mut caller, ptr, len)?;
    allow.extend(policy.split('\n').map(|pattern| format!("!{pattern}")));
    ExternRef::new(&mut caller, NetAuthority { allow, issuer }).map(Some)
}

fn response_status(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(net_response(&caller, token)?.status))
}

fn response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    response_length(net_response(&caller, token)?.bytes.len())
}

/// Copy the whole payload when `capacity` holds it and answer its length
/// either way; a short buffer is left untouched.
fn response_into(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, out_ptr: i32, capacity: i32) -> Result<i32> {
    let bytes = net_response(&caller, token)?.bytes;
    let required = response_length(bytes.len())?;
    let start = usize::try_from(out_ptr).map_err(|_| Error::msg("negative Net response pointer"))?;
    let length = usize::try_from(capacity).map_err(|_| Error::msg("negative Net response capacity"))?;
    let memory = memory_of(&mut caller)?;
    let end = start.checked_add(length).filter(|end| *end <= memory.data_size(&caller))
        .ok_or_else(|| Error::msg("Net response buffer lies outside guest memory"))?;
    if end - start >= bytes.len() {
        memory.write(&mut caller, start, bytes.as_slice())?;
    }
    Ok(required)
}

/// `net_response_take_net(response) -> Net`: the Net a successful `only`
/// minted. Any other response traps; the guest takes it only on status 0.
fn response_take_net(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    match net_response(&caller, token)?.minted {
        Some(Minted::Net(net)) => ExternRef::new(&mut caller, net).map(Some),
        _ => Err(Error::msg("Net response carries no Net")),
    }
}

/// `net_response_take_socket(response) -> Socket`: the Socket a successful
/// `connect` or `accept` minted. Any other response traps.
fn response_take_socket(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    match net_response(&caller, token)?.minted {
        Some(Minted::Socket(socket)) => ExternRef::new(&mut caller, socket).map(Some),
        _ => Err(Error::msg("Net response carries no Socket")),
    }
}

/// `net_response_take_listener(response) -> Listener`: the Listener a
/// successful `listen` minted. Any other response traps.
fn response_take_listener(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    match net_response(&caller, token)?.minted {
        Some(Minted::Listener(listener)) => ExternRef::new(&mut caller, listener).map(Some),
        _ => Err(Error::msg("Net response carries no Listener")),
    }
}
