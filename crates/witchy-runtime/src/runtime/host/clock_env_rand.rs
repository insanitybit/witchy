use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use std::sync::Arc;
use witchy_wir::canonical_safewir::ENV_IMPORT_NAMES;

use super::super::{
    memory_of, read_slice_str, read_wstr, read_wstr_list, EnvAuthority, VmState,
};

/// Register the `Clock` observation imports.
pub(in crate::runtime) fn link_clock(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "now", host_now)?;
    linker.func_wrap("witchy", "now_monotonic", host_now_monotonic)?;
    Ok(())
}

pub(in crate::runtime) struct ClockAuthority {
    issuer: Arc<()>,
}

pub(in crate::runtime) fn mint_clock(state: &VmState) -> Result<ClockAuthority> {
    if !state.caps.clock {
        return Err(Error::msg("the program's `main` requires a Clock capability that was not granted"));
    }
    Ok(ClockAuthority { issuer: state.registry_issuer.clone() })
}

pub(in crate::runtime) fn authenticate_clock_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    let authority = data.downcast_ref::<ClockAuthority>()
        .ok_or_else(|| Error::msg("clock_now: wrong capability kind"))?;
    if !Arc::ptr_eq(&state.registry_issuer, &authority.issuer) {
        return Err(Error::msg("clock_now: capability belongs to another instance"));
    }
    Ok(())
}

pub(in crate::runtime) fn link_canonical_clock(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "clock_now", host_clock_now)?;
    linker.func_wrap("witchy", "clock_now_monotonic", host_clock_now_monotonic)?;
    Ok(())
}

fn host_clock_now_monotonic(caller: Caller<'_, VmState>, clock: Option<Rooted<ExternRef>>) -> Result<i64> {
    let clock = clock.ok_or_else(|| Error::msg("clock_now_monotonic: null capability"))?;
    let data = clock.data(&caller)?.ok_or_else(|| Error::msg("clock_now_monotonic: missing capability data"))?;
    authenticate_clock_capture(caller.data(), data)?;
    Ok(host_now_monotonic(caller))
}

fn host_clock_now(caller: Caller<'_, VmState>, clock: Option<Rooted<ExternRef>>) -> Result<i64> {
    let clock = clock.ok_or_else(|| Error::msg("clock_now: null capability"))?;
    let data = clock.data(&caller)?.ok_or_else(|| Error::msg("clock_now: missing capability data"))?;
    authenticate_clock_capture(caller.data(), data)?;
    // Reuse the exact provider, including its pre-epoch fallback and units.
    Ok(host_now(caller))
}

/// Register the `Rand` draw import.
pub(in crate::runtime) fn link_rand(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "rand_u64", host_rand_u64)?;
    Ok(())
}

/// Register the `Env` lookup imports.
pub(in crate::runtime) fn link_env(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_env", host_mint_env)?;
    linker.func_wrap("witchy", "env_only", host_env_only)?;
    linker.func_wrap("witchy", "env_len", host_env_len)?;
    linker.func_wrap("witchy", "env_fill", host_env_fill)?;
    // The canonical route's Env lookup, under the SAME `Env` grant as its
    // frozen siblings: an ungranted program cannot link it.
    linker.func_wrap("witchy", ENV_IMPORT_NAMES[0], host_env_get_at)?;
    linker.func_wrap("witchy", ENV_IMPORT_NAMES[1], host_env_response_status)?;
    linker.func_wrap("witchy", ENV_IMPORT_NAMES[2], host_env_response_size)?;
    linker.func_wrap("witchy", ENV_IMPORT_NAMES[3], host_env_response_into)?;
    Ok(())
}

/// The `Env` authority an `Env` grant confers: the coarse grant, narrowed by the
/// launch allow-list when one was named. The frozen route reaches it through
/// `mint_env` and the canonical launcher mints its entry parameter from it, so
/// both routes hand the guest the SAME authority rather than two constructions.
pub(in crate::runtime) fn env_authority(state: &VmState) -> EnvAuthority {
    EnvAuthority {
        allow: state.caps.env_allow.as_ref().map(|names| names.iter().cloned().collect()),
    }
}

/// Whether the grant's allow-list admits `name`: the one rule the frozen
/// `env_len` trap and the canonical `Denied` answer share, so neither route can
/// see a name the other refuses.
fn env_allows(env: &EnvAuthority, name: &str) -> bool {
    env.allow.as_ref().is_none_or(|allow| allow.contains(name))
}

/// Trap unless the grant's allow-list admits `name` (the frozen route).
fn env_require_allowed(env: &EnvAuthority, name: &str) -> Result<()> {
    if env_allows(env, name) {
        Ok(())
    } else {
        Err(Error::msg(format!("get_env: `{name}` is not in this Env grant's allow-list")))
    }
}

fn env_authority_ref(
    caller: &Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
) -> Result<EnvAuthority> {
    let env = env.ok_or_else(|| Error::msg("null Env externref"))?;
    env.data(caller)?
        .ok_or_else(|| Error::msg("Env externref has no host data"))?
        .downcast_ref::<EnvAuthority>()
        .cloned()
        .ok_or_else(|| Error::msg("Env externref has wrong host data"))
}

fn host_mint_env(mut caller: Caller<'_, VmState>) -> Result<Option<Rooted<ExternRef>>> {
    let authority = env_authority(caller.data());
    ExternRef::new(&mut caller, authority).map(Some)
}

fn host_env_only(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    names_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let current = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let requested: std::collections::BTreeSet<_> =
        read_wstr_list(mem.data(&caller), names_ptr)?.into_iter().collect();
    let allow = Some(match current.allow {
        Some(current) => requested.intersection(&current).cloned().collect(),
        None => requested,
    });
    ExternRef::new(&mut caller, EnvAuthority { allow }).map(Some)
}

/// `now() -> Int`: wall-clock milliseconds since the Unix epoch — the same value
/// the interpreter's `now(Clock)` produces. Linked only when the VM was
/// granted a `Clock` capability.
/// `rand_u64() -> i64`: a fresh draw of the `Rand` capability. Seeded (WITCHY_RAND_SEED)
/// it advances the per-VM splitmix64 state so a run is deterministic and parity-stable;
/// unseeded it draws 8 bytes from the OS CSPRNG. Linked only when `Rand` was granted.
fn host_rand_u64(mut caller: Caller<'_, VmState>) -> i64 {
    match &mut caller.data_mut().rand_state {
        Some(state) => crate::rand::seeded_next(state) as i64,
        None => {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).expect("OS CSPRNG is available");
            i64::from_le_bytes(b)
        }
    }
}

fn host_now(_caller: Caller<'_, VmState>) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `now_monotonic() -> i64`: nanoseconds elapsed on a monotonic (steady) clock
/// since first use. The reference `Instant` is set lazily on the first call, so
/// a start/stop bracket around a computation measures its elapsed time without
/// the wall-clock jump hazard of `now`.
fn host_now_monotonic(_caller: Caller<'_, VmState>) -> i64 {
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    START.elapsed().as_nanos() as i64
}

/// `env_len(name_ptr) -> Int`: the byte length of the named environment
/// variable's value, or -1 when unset (or not valid Unicode — matching the
/// interpreter's `std::env::var`, which errors on both). The guest sizes its
/// buffer from this, then calls `env_fill`. Linked only under an `Env` grant.
fn host_env_len(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_ptr: i32,
) -> Result<i32> {
    let env = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    env_require_allowed(&env, &name)?;
    match std::env::var(&name) {
        Ok(v) => Ok(v.len() as i32),
        Err(_) => Ok(-1),
    }
}

/// `env_fill(name_ptr, out_ptr)`: write the named environment variable's value
/// bytes into guest memory at `out_ptr` (the guest pre-allocated `env_len`
/// bytes). Linked only under an `Env` grant.
fn host_env_fill(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    let env = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    env_require_allowed(&env, &name)?;
    let value = std::env::var(&name).unwrap_or_default();
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| Error::msg(format!("writing env value into guest memory: {e}")))
}

// --- the canonical Env lookup (RFC-0126 rule 3) ---
//
// The frozen `env_len`/`env_fill` pair encodes "unset" as a -1 length and then
// trusts the guest to have sized its buffer from the first call. The canonical
// lookup consults the grant and the environment once and answers with an
// immutable response: later getters only borrow that snapshot, so no
// guest-chosen length or second lookup can disagree with the first.

/// `env_get_at`'s answer for a set variable.
const ENV_SET: u8 = 0;
/// `env_get_at`'s answer for an unset (or non-Unicode) variable.
const ENV_UNSET: u8 = 1;
/// `env_get_at`'s answer for a name outside the grant's allow-list.
const ENV_DENIED: u8 = 2;

/// Private immutable lookup result, not an `Env` grant. Neither an integer
/// kind nor bytes supplied by the guest can construct one. `status` is
/// `ENV_SET` with the value, `ENV_UNSET` with no payload, or `ENV_DENIED`
/// with the denial message.
#[derive(Clone)]
pub(in crate::runtime) struct EnvResponse {
    issuer: Arc<()>,
    status: u8,
    bytes: Arc<Vec<u8>>,
}

/// The status and payload `env_get_at` answers for `name`. The allow-list is
/// consulted before the environment, so a denied name answers identically
/// whether or not the host sets it. The denial message is guest-visible data:
/// it names only the variable the guest asked for, never another allowed
/// name, a value, or host detail.
fn env_lookup(env: &EnvAuthority, name: &str) -> (u8, Vec<u8>) {
    if !env_allows(env, name) {
        return (ENV_DENIED, format!("`{name}` is not covered by this Env capability").into_bytes());
    }
    match std::env::var(name) {
        Ok(value) => (ENV_SET, value.into_bytes()),
        Err(_) => (ENV_UNSET, Vec::new()),
    }
}

fn env_bytes_length(length: usize) -> Result<i32> {
    i32::try_from(length).map_err(|_| Error::msg("Env response exceeds the guest ABI size limit"))
}

/// `env_get_at(env, name, name_len) -> response`: look `name` up once. A
/// forged or wrong-kind token traps; a name the grant does not cover is the
/// response's `Denied` status, not a trap.
fn host_env_get_at(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_ptr: i32,
    name_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let env = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let name = read_slice_str(mem.data(&caller), name_ptr, name_len)?;
    let (status, bytes) = env_lookup(&env, &name);
    env_bytes_length(bytes.len())?;
    let response = EnvResponse { issuer: caller.data().registry_issuer.clone(), status, bytes: Arc::new(bytes) };
    ExternRef::new(&mut caller, response).map(Some)
}

fn env_response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<EnvResponse> {
    let token = token.ok_or_else(|| Error::msg("Env response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("Env response has no host data"))?;
    let response = data.downcast_ref::<EnvResponse>()
        .ok_or_else(|| Error::msg("Env response has wrong host kind"))?;
    if !Arc::ptr_eq(&caller.data().registry_issuer, &response.issuer) {
        return Err(Error::msg("Env response belongs to another instance"));
    }
    Ok(response.clone())
}

fn host_env_response_status(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(env_response(&caller, token)?.status))
}

fn host_env_response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    env_bytes_length(env_response(&caller, token)?.bytes.len())
}

/// Copy the whole payload when `capacity` holds it and answer its length
/// either way; a short buffer is left untouched.
fn host_env_response_into(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = env_response(&caller, token)?.bytes;
    let required = env_bytes_length(bytes.len())?;
    let start = usize::try_from(out_ptr).map_err(|_| Error::msg("negative Env response pointer"))?;
    let length = usize::try_from(capacity).map_err(|_| Error::msg("negative Env response capacity"))?;
    let memory = memory_of(&mut caller)?;
    let end = start.checked_add(length).filter(|end| *end <= memory.data_size(&caller))
        .ok_or_else(|| Error::msg("Env response buffer lies outside guest memory"))?;
    if end - start >= bytes.len() {
        memory.write(&mut caller, start, bytes.as_slice())?;
    }
    Ok(required)
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod tests {
    use crate::runtime::{Capabilities, Runtime, Vm};

    /// Look up the name at 0..len and leave (status, payload) in
    /// `status`/`out`, `out` holding the payload bytes at 64.
    const LOOKUP: &str = r#"
        (module
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_get_at" (func $get (param externref i32 i32) (result externref)))
          (import "witchy" "env_response_status" (func $status (param externref) (result i32)))
          (import "witchy" "env_response_size" (func $size (param externref) (result i32)))
          (import "witchy" "env_response_into" (func $copy (param externref i32 i32) (result i32)))
          (memory (export "memory") 1)
          (global $status (export "status") (mut i32) (i32.const -1))
          (global $length (export "length") (mut i32) (i32.const -1))
          (func (export "lookup") (param $len i32)
            (local $response externref)
            (local.set $response (call $get (call $mint_env) (i32.const 0) (local.get $len)))
            (global.set $status (call $status (local.get $response)))
            (global.set $length (call $size (local.get $response)))
            (drop (call $copy (local.get $response) (i32.const 64) (global.get $length)))))
    "#;

    fn lookup(allow: Option<Vec<String>>, name: &str) -> (i32, String) {
        let caps = Capabilities { env: true, env_allow: allow, ..Capabilities::default() };
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime.spawn(LOOKUP, caps, 4).expect("instantiate the Env lookup probe");
        let memory = vm.instance.get_memory(&mut vm.store, "memory").expect("memory");
        memory.write(&mut vm.store, 0, name.as_bytes()).expect("write the name");
        let run = vm.instance.get_typed_func::<i32, ()>(&mut vm.store, "lookup").expect("lookup export");
        run.call(&mut vm.store, i32::try_from(name.len()).unwrap()).expect("a lookup never traps on its answer");
        let global = |vm: &mut Vm, name: &str| {
            vm.instance.get_global(&mut vm.store, name).expect(name).get(&mut vm.store).i32().expect(name)
        };
        let status = global(&mut vm, "status");
        let length = usize::try_from(global(&mut vm, "length")).expect("payload length");
        let mut payload = vec![0; length];
        memory.read(&vm.store, 64, &mut payload).expect("read the payload");
        (status, String::from_utf8(payload).expect("UTF-8 payload"))
    }

    /// Set, unset and denied answers. A name outside the allow-list answers
    /// the same `Denied` whether or not the host sets it, and the message
    /// names only that name: never another allowed name or a value.
    pub fn env_get_at_answers_denied_without_an_existence_oracle() {
        // The crate forbids `unsafe`, so the probe reads the environment the
        // test process inherited rather than setting one.
        const SET: &str = "PATH";
        const UNSET: &str = "WITCHY_ENV_RESPONSE_PROBE_UNSET";
        let value = std::env::var(SET).expect("the test process has PATH");
        assert!(std::env::var_os(UNSET).is_none(), "{UNSET} must be unset");
        let only = |names: &[&str]| Some(names.iter().map(|name| name.to_string()).collect());
        assert_eq!(lookup(only(&[SET, UNSET]), SET), (0, value.clone()));
        assert_eq!(lookup(only(&[SET, UNSET]), UNSET), (1, String::new()));
        assert_eq!(lookup(None, SET), (0, value.clone()));
        let denied_set = lookup(only(&["ALLOWED"]), SET);
        let denied_unset = lookup(only(&["ALLOWED"]), UNSET);
        assert_eq!(denied_set, (2, format!("`{SET}` is not covered by this Env capability")));
        assert_eq!(denied_unset, (2, format!("`{UNSET}` is not covered by this Env capability")));
        for (_, message) in [denied_set, denied_unset] {
            assert!(!message.contains("ALLOWED") && !message.contains(&value), "{message}");
        }
    }
}
