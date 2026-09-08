use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{
    fill_slice, memory_of, read_slice_str, read_wstr, read_wstr_list, EnvAuthority, VmState,
};

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
    // (RFC-0165 slice 42) The canonical route's Env pair, under the SAME `Env`
    // grant as its frozen siblings: an ungranted program cannot link them.
    linker.func_wrap("witchy", "env_has_var", host_env_has_var)?;
    linker.func_wrap("witchy", "env_get_into", host_env_get_into)?;
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

/// Trap unless the grant's allow-list admits `name` — the frozen `env_len` rule,
/// shared so a canonical lookup can never see a name the frozen one refuses.
fn env_require_allowed(env: &EnvAuthority, name: &str) -> Result<()> {
    match &env.allow {
        Some(allow) if !allow.contains(name) => Err(Error::msg(format!(
            "get_env: `{name}` is not in this Env grant's allow-list"
        ))),
        _ => Ok(()),
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

// --- (RFC-0165 slice 42) the canonical Env pair ---
//
// The frozen `env_len`/`env_fill` pair encodes "unset" as a -1 length and then
// trusts the guest to have sized its buffer from the first call. The canonical
// pair splits the two questions instead: existence is its own total answer, and
// a read is bounded by the capacity the guest declares, so no host state and no
// guest-chosen length survives between calls.

/// `env_has_var(env, name, name_len) -> Bool`: 1 iff the variable is set AND the
/// grant's allow-list admits it. A name outside the allow-list traps — refusing
/// it quietly would let a program probe for names it was never granted.
fn host_env_has_var(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_ptr: i32,
    name_len: i32,
) -> Result<i32> {
    let env = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let name = read_slice_str(mem.data(&caller), name_ptr, name_len)?;
    env_require_allowed(&env, &name)?;
    Ok(std::env::var(&name).is_ok() as i32)
}

/// `env_get_into(env, name, name_len, out, out_cap) -> Size`: write
/// `min(length, out_cap)` bytes of the variable's value at `out` and return its
/// FULL length. An unset (or non-Unicode) variable traps: the guest asks
/// `env_has_var` first, so reaching here for an absent name is a contract
/// violation, not a value the read can report.
fn host_env_get_into(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_ptr: i32,
    name_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i32> {
    let env = env_authority_ref(&caller, env)?;
    let mem = memory_of(&mut caller)?;
    let name = read_slice_str(mem.data(&caller), name_ptr, name_len)?;
    env_require_allowed(&env, &name)?;
    let value = std::env::var(&name)
        .map_err(|_| Error::msg(format!("get_env: `{name}` is not set")))?;
    fill_slice(&mut caller, out_ptr, out_cap, value.as_bytes(), "environment value")
}

#[cfg(test)]
mod tests {
    use super::super::super::{Capabilities, Runtime};

    /// A name no process sets, so the probes below need no ambient fixture.
    const UNSET: &str = "WITCHY_TRACK_R_UNSET_NAME";

    /// Ask about `UNSET` (25 bytes at 0) and record the answer at 256.
    const HAS_VAR: &str = r#"
        (module
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_has_var"
            (func $env_has_var (param externref i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "WITCHY_TRACK_R_UNSET_NAME")
          (func (export "run")
            (i32.store (i32.const 256)
              (call $env_has_var (call $mint_env) (i32.const 0) (i32.const 25)))))
    "#;

    /// Ask about `OTHER_NAME` (10 bytes at 0), which the allow-list omits.
    const HAS_VAR_DENIED: &str = r#"
        (module
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_has_var"
            (func $env_has_var (param externref i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "OTHER_NAME")
          (func (export "run")
            (drop (call $env_has_var (call $mint_env) (i32.const 0) (i32.const 10)))))
    "#;

    /// Read `UNSET` into a 64-byte buffer without asking whether it is set.
    const GET_INTO: &str = r#"
        (module
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_get_into"
            (func $env_get_into (param externref i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "WITCHY_TRACK_R_UNSET_NAME")
          (func (export "run")
            (drop (call $env_get_into (call $mint_env)
              (i32.const 0) (i32.const 25) (i32.const 64) (i32.const 64)))))
    "#;

    fn env_capabilities(allow: Option<Vec<String>>) -> Capabilities {
        Capabilities { env: true, env_allow: allow, ..Capabilities::default() }
    }

    /// Existence is a total answer, so an unset name is `0` rather than a trap
    /// — that is what makes it safe to ask before reading.
    #[test]
    fn env_has_var_reports_zero_for_an_unset_name() {
        assert!(std::env::var(UNSET).is_err(), "the probe name must be unset");
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(HAS_VAR, env_capabilities(None), 4)
            .expect("instantiate the existence probe");
        vm.run().expect("run the existence probe");

        let memory = vm.instance.get_memory(&mut vm.store, "memory").expect("guest memory");
        let mut answer = [0u8; 4];
        memory.read(&vm.store, 256, &mut answer).expect("read the reported answer");
        assert_eq!(i32::from_le_bytes(answer), 0, "an unset variable is not present");
    }

    /// A name outside the allow-list is refused loudly, exactly as the frozen
    /// `env_len` refuses it — answering `0` would turn the grant into an oracle
    /// for names it never conferred.
    #[test]
    fn env_has_var_traps_on_a_name_outside_the_allow_list() {
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(HAS_VAR_DENIED, env_capabilities(Some(vec!["ALLOWED".to_string()])), 4)
            .expect("instantiate the allow-list probe");
        let error = vm.run().expect_err("an unlisted name must trap");
        assert!(
            format!("{error:?}").contains("allow-list"),
            "expected an allow-list refusal, got: {error:?}"
        );
    }

    /// The read reports a length, so it has no way to encode absence; a guest
    /// that skipped `env_has_var` broke the contract and traps.
    #[test]
    fn env_get_into_traps_on_an_unset_name() {
        assert!(std::env::var(UNSET).is_err(), "the probe name must be unset");
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(GET_INTO, env_capabilities(None), 4)
            .expect("instantiate the unset-read probe");
        let error = vm.run().expect_err("reading an unset variable must trap");
        assert!(
            format!("{error:?}").contains("is not set"),
            "expected an unset-variable trap, got: {error:?}"
        );
    }
}
