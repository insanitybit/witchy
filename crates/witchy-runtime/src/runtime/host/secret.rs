use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use std::sync::Arc;

use super::super::{memory_of, read_slice_str, read_wstr, SecretGrant, SecretMaterial, VmState};

/// Neither token exposes material or accepts a guest-supplied grant index.
pub(in crate::runtime) struct StoreAuthority {
    issuer: Arc<()>,
    grants: Arc<Vec<SecretGrant>>,
}

pub(in crate::runtime) struct SecretAuthority {
    issuer: Arc<()>,
    material: SecretMaterial,
}

pub(in crate::runtime) fn mint_store(state: &VmState) -> StoreAuthority {
    // An empty SecretStore is a real zero-resource grant. Keep declaration
    // order: duplicate names have the same first-match semantics as legacy.
    StoreAuthority { issuer: state.registry_issuer.clone(), grants: Arc::new(state.caps.secrets.clone()) }
}

pub(in crate::runtime) fn mint_canonical_secret(state: &VmState) -> Result<SecretAuthority> {
    Ok(SecretAuthority { issuer: state.registry_issuer.clone(), material: root_secret_material(state)? })
}

fn authenticate_store<'a>(state: &VmState, data: &'a dyn std::any::Any) -> Result<&'a StoreAuthority> {
    let root = data.downcast_ref::<StoreAuthority>()
        .ok_or_else(|| Error::msg("SecretStore: wrong capability kind"))?;
    if !Arc::ptr_eq(&state.registry_issuer, &root.issuer) {
        return Err(Error::msg("SecretStore: capability belongs to another instance"));
    }
    Ok(root)
}

pub(in crate::runtime) fn authenticate_store_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate_store(state, data).map(|_| ())
}

fn authenticate_secret<'a>(state: &VmState, data: &'a dyn std::any::Any) -> Result<&'a SecretAuthority> {
    let secret = data.downcast_ref::<SecretAuthority>()
        .ok_or_else(|| Error::msg("Secret: wrong capability kind"))?;
    if !Arc::ptr_eq(&state.registry_issuer, &secret.issuer) {
        return Err(Error::msg("Secret: capability belongs to another instance"));
    }
    Ok(secret)
}

pub(in crate::runtime) fn authenticate_secret_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate_secret(state, data).map(|_| ())
}

pub(in crate::runtime) fn canonical_secret_material_ref(
    caller: &Caller<'_, VmState>, secret: Option<Rooted<ExternRef>>,
) -> Result<SecretMaterial> {
    let secret = secret.ok_or_else(|| Error::msg("Secret: null capability"))?;
    let data = secret.data(caller)?.ok_or_else(|| Error::msg("Secret: missing capability data"))?;
    Ok(authenticate_secret(caller.data(), data)?.material.clone())
}

pub(in crate::runtime) fn link_canonical(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "secretstore_lookup_at", host_secretstore_lookup_at)?;
    Ok(())
}

fn host_secretstore_lookup_at(
    mut caller: Caller<'_, VmState>, store: Option<Rooted<ExternRef>>,
    name_ptr: i32, name_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let store = store.ok_or_else(|| Error::msg("SecretStore: null capability"))?;
    let data = store.data(&caller)?.ok_or_else(|| Error::msg("SecretStore: missing capability data"))?;
    let grants = authenticate_store(caller.data(), data)?.grants.clone();
    let memory = memory_of(&mut caller)?;
    let name = read_slice_str(memory.data(&caller), name_ptr, name_len)?;
    let Some(grant) = grants.iter().find(|grant| grant.name == name) else { return Ok(None); };
    let secret = SecretAuthority {
        issuer: caller.data().registry_issuer.clone(), material: SecretMaterial::from_grant(grant),
    };
    ExternRef::new(&mut caller, secret).map(Some)
}

/// Register the root `Secret` grant mint: the signing-key seed never enters
/// guest memory; `main(key: Secret)` receives an opaque host reference.
pub(in crate::runtime) fn link_mint(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_secret", host_mint_secret)?;
    Ok(())
}

/// Register the always-available store lookup. Resolving a named secret
/// returns only an opaque nullable externref, so it is always linkable; an
/// ungranted store simply yields null for every name.
pub(in crate::runtime) fn link_lookup(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "secretstore_lookup", host_secretstore_lookup)?;
    Ok(())
}

// --- the Secret capability family ---
//
// A guest `Secret` is an opaque externref carrying host-side bytes plus the
// reveal policy. The compiled backend no longer exposes dense secret-table
// integers to guest code: `SecretStore.get` returns a nullable externref,
// `require` checks for null immediately, and the root `Secret` is minted only
// from the signing-key grant.

fn signing_secret_material(caller: &Caller<'_, VmState>, h: i32) -> Result<SecretMaterial> {
    if h != 0 {
        return Err(Error::msg(format!("invalid Secret grant index {h}")));
    }
    root_secret_material(caller.data())
}

fn root_secret_material(state: &VmState) -> Result<SecretMaterial> {
    let caps = &state.caps;
    let signing = caps
        .signing_key
        .ok_or_else(|| Error::msg("root Secret is not granted"))?;
    if let Some(grant) = caps.secrets.iter().find(|grant| {
        grant.name == "signing"
            && witchy_caps::capabilities::secret_is_signing_key(Some(signing.as_slice()), &grant.bytes)
    }) {
        return Ok(SecretMaterial::from_grant(grant));
    }
    Ok(SecretMaterial {
        bytes: signing.to_vec(),
        sealed: false,
    })
}

pub(in crate::runtime) fn secret_material_ref(
    caller: &Caller<'_, VmState>,
    secret: Option<Rooted<ExternRef>>,
) -> Result<SecretMaterial> {
    let secret = secret.ok_or_else(|| Error::msg("Secret externref is null"))?;
    let data = secret.data(caller)?
        .ok_or_else(|| Error::msg("Secret externref has no host data"))?;
    // Legacy reveal/signing adapters may receive a canonical token. Preserve
    // sealing and signing-key identity, but never bypass its issuer check.
    if data.is::<SecretAuthority>() {
        return Ok(authenticate_secret(caller.data(), data)?.material.clone());
    }
    data.downcast_ref::<SecretMaterial>()
        .cloned()
        .ok_or_else(|| Error::msg("Secret externref has wrong host data"))
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "registry_hosts_tests.rs"]
pub mod registry_tests;

fn host_mint_secret(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let secret = signing_secret_material(&caller, i)?;
    ExternRef::new(&mut caller, secret).map(Some)
}

/// `secretstore_lookup(name_ptr) -> externref?`: the host-owned secret named
/// `name`, or null if it was not granted. The bytes never cross into guest memory
/// here; callers receive only an opaque `Secret` externref.
fn host_secretstore_lookup(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let store = super::fixture::root_handle(&caller, "SecretStore")?;
        return match super::fixture::invoke(
            &mut caller,
            witchy_test_host::HostRequest::SecretStoreLookup { store, name },
        )? {
            witchy_test_host::HostResponse::OptionalHandle(Some(handle)) => {
                ExternRef::new(&mut caller, handle).map(Some)
            }
            witchy_test_host::HostResponse::OptionalHandle(None) => Ok(None),
            response => Err(Error::msg(format!(
                "fixture SecretStore.lookup returned unexpected response {response:?}"
            ))),
        };
    }
    let grant = caller
        .data()
        .caps
        .secrets
        .iter()
        .find(|grant| grant.name == name)
        .map(SecretMaterial::from_grant);
    match grant {
        Some(secret) => ExternRef::new(&mut caller, secret).map(Some),
        None => Ok(None),
    }
}
