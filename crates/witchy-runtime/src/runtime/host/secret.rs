use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{memory_of, read_wstr, SecretMaterial, VmState};

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
    let caps = &caller.data().caps;
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
        use_only: false,
    })
}

pub(in crate::runtime) fn secret_material_ref(
    caller: &Caller<'_, VmState>,
    secret: Option<Rooted<ExternRef>>,
) -> Result<SecretMaterial> {
    let secret = secret.ok_or_else(|| Error::msg("Secret externref is null"))?;
    secret
        .data(caller)?
        .ok_or_else(|| Error::msg("Secret externref has no host data"))?
        .downcast_ref::<SecretMaterial>()
        .cloned()
        .ok_or_else(|| Error::msg("Secret externref has wrong host data"))
}

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
