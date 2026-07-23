use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{memory_of, read_wstr, read_wstr_list, EnvAuthority, VmState};

/// Register the `Clock` observation imports.
pub(in crate::runtime) fn link_clock(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "now", host_now)?;
    linker.func_wrap("witchy", "now_monotonic", host_now_monotonic)?;
    Ok(())
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
    Ok(())
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
    let allow = caller
        .data()
        .caps
        .env_allow
        .as_ref()
        .map(|names| names.iter().cloned().collect());
    ExternRef::new(&mut caller, EnvAuthority { allow }).map(Some)
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
    if let Some(allow) = &env.allow {
        if !allow.contains(&name) {
            return Err(Error::msg(format!("get_env: `{name}` is not in this Env grant's allow-list")));
        }
    }
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
    if let Some(allow) = &env.allow {
        if !allow.contains(&name) {
            return Err(Error::msg(format!("get_env: `{name}` is not in this Env grant's allow-list")));
        }
    }
    let value = std::env::var(&name).unwrap_or_default();
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| Error::msg(format!("writing env value into guest memory: {e}")))
}
