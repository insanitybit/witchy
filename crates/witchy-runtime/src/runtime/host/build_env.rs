//! BuildEnv is a sealed snapshot of explicitly granted names, not ambient Env.
use crate::runtime::GuestAddress;
use std::{collections::BTreeMap, sync::Arc};
use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use super::super::{memory_of, read_slice_str, VmState};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    read: bool,
    values: Arc<BTreeMap<String, Option<String>>>,
}

pub(in crate::runtime) fn mint(state: &VmState) -> Result<Authority> {
    let values = state.caps.build_env.as_ref()
        .ok_or_else(|| Error::msg("build step demands `BuildEnv` but no environment grant was provided"))?;
    // An explicitly supplied empty snapshot grants no names, but is not an
    // absent grant. Preserve the existing source-interpreter distinction.
    Ok(Authority {
        issuer: state.build_env_issuer.clone(), read: true,
        values: Arc::new(values.clone()),
    })
}

fn authenticate<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| Error::msg("get_build_env: wrong capability kind"))?;
    if !Arc::ptr_eq(issuer, &authority.issuer) {
        bail!("get_build_env: capability belongs to another instance");
    }
    if !authority.read { bail!("get_build_env: capability lacks read right"); }
    Ok(authority)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(&state.build_env_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    // Linking exposes no mint operation and grants no access by itself.
    linker.func_wrap("witchy", "build_env_at", env_at::<P>)?;
    Ok(())
}

fn env_at<P: GuestAddress>(
    mut caller: Caller<'_, VmState>, grant: Option<Rooted<ExternRef>>,
    key_ptr: P, key_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let grant = grant.ok_or_else(|| Error::msg("get_build_env: null capability"))?;
    let data = grant.data(&caller)?.ok_or_else(|| Error::msg("get_build_env: missing capability data"))?;
    let authority = authenticate(&caller.data().build_env_issuer, data)?.clone();
    // Authenticate actual kind, issuer and Read before touching guest memory.
    // Copy and decode the whole key before consulting the immutable snapshot.
    let memory = memory_of(&mut caller)?;
    let key = read_slice_str(memory.data(&caller), key_ptr, key_len)?;
    let value = authority.values.get(&key).ok_or_else(|| Error::msg(format!(
        "get_build_env: `{key}` is not in this BuildEnv grant's allow-list"
    )))?;
    super::build_read::new_response(&mut caller, value.as_deref()).map(Some)
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "build_env_tests.rs"]
pub mod tests;
