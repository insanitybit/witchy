//! Canonical Net responses and refinement (RFC-0126 rule 3). Every fallible
//! Net operation answers with an immutable response; `net.deny` only removes
//! endpoints, so it answers with the narrowed `Net` directly.
use crate::runtime::GuestAddress;
use super::*;
use super::super::super::read_slice_str;
use witchy_wir::canonical_safewir::NET_RESPONSE_IMPORT_NAMES;
pub(super) use super::super::cap_response::{fail, respond, Minted};

pub(super) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[0], only_at::<P>)?;
    linker.func_wrap("witchy", NET_RESPONSE_IMPORT_NAMES[1], deny_at::<P>)?;
    Ok(())
}

/// The `CapError::Denied` status (its variant ordinal plus one).
pub(super) const DENIED: u8 = 1;
/// The `CapError::AlreadyExists` status.
pub(super) const ALREADY_EXISTS: u8 = 4;
/// The `CapError::Io` status.
pub(super) const IO: u8 = 5;

pub(super) fn slice_text<P: GuestAddress>(caller: &mut Caller<'_, VmState>, ptr: P, len: i32) -> Result<String> {
    let memory = memory_of(caller)?;
    read_slice_str(memory.data(&*caller), ptr, len)
}

/// `net_only_at(net, policy) -> response`: narrow to the policy's endpoints.
/// Each must already be admitted, and endpoints the receiver denies stay
/// denied. The decision reads only the grant, never DNS or a socket, and a
/// refusal names only the pattern the guest supplied.
fn only_at<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    let policy = slice_text(&mut caller, ptr, len)?;
    match witchy_caps::capabilities::net_only(&net.allow, &policy) {
        Ok(allow) => respond(&mut caller, 0, Vec::new(), Some(Minted::Net(NetAuthority { allow, issuer: net.issuer }))),
        Err(pattern) => fail(&mut caller, DENIED, format!("`{pattern}` is not covered by this Net capability")),
    }
}

/// `net_deny_at(net, policy) -> Net`: subtract the policy's endpoints, a
/// monotone exclusion the shared `net_allows` honours.
fn deny_at<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let NetAuthority { mut allow, issuer } = net_authority_ref(&caller, token)?;
    let policy = slice_text(&mut caller, ptr, len)?;
    allow.extend(policy.split('\n').map(|pattern| format!("!{pattern}")));
    ExternRef::new(&mut caller, NetAuthority { allow, issuer }).map(Some)
}
