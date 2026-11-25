//! One bounded, parameterized transaction on an operator-selected SQLite file.

mod grant;
mod transaction;

pub(super) use grant::{Grant, Rights};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Linker};

use super::{State, guest_range, reject};
use transaction::transaction;

const HANDLE: i64 = 7;
const MAX_PLAN: usize = 16_384;
const MAX_BINDINGS: usize = 32_768;
const MAX_BLOB: usize = 1_048_576;
const DEFAULT_REPLY_BYTES: usize = 16_384;
pub(super) const MAX_REPLY_BYTES: usize = 2 << 20;

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, "sqlite_authorize", |mut caller: Caller<'_, State>, code: i64| -> wasmtime::Result<i64> {
        if !caller.data().active || code != 0 { return Err(reject("invalid SQLite capability")); }
        let grant = caller.data_mut().sqlite.as_mut().ok_or_else(|| reject("invalid SQLite capability"))?;
        if std::mem::replace(&mut grant.authorized, true) { return Err(reject("invalid SQLite capability")); }
        Ok(HANDLE)
    })?;
    linker.func_wrap(namespace, "sqlite_run", |mut caller: Caller<'_, State>, handle: i64, plan: i64, bindings: i64, blob: i64, reply: i64| -> wasmtime::Result<i64> {
        if !caller.data().active || handle != HANDLE { return Err(reject("invalid SQLite capability")); }
        let (memory, p, pl) = guest_range(&mut caller, plan)?;
        let (_, b, bl) = guest_range(&mut caller, bindings)?;
        let (_, v, vl) = guest_range(&mut caller, blob)?;
        let (_, _, capacity) = guest_range(&mut caller, reply)?;
        let grant = caller.data_mut().sqlite.as_mut().ok_or_else(|| reject("invalid SQLite capability"))?;
        if !grant.authorized || std::mem::replace(&mut grant.used, true) { return Err(reject("invalid SQLite capability")); }
        if pl > MAX_PLAN || bl > MAX_BINDINGS || vl > MAX_BLOB || capacity == 0 { return Ok(0); }
        let grant = caller.data().sqlite.as_ref().unwrap();
        let bytes = memory.data(&caller);
        let local = Instant::now() + Duration::from_secs(5);
        let until = super::DEADLINE_AT.get().map_or(local, |at| local.min(*at));
        let result = transaction(grant, &bytes[p..p + pl], &bytes[b..b + bl], &bytes[v..v + vl], capacity, until)
            .unwrap_or_else(|| vec![2]);
        super::write_guest(&mut caller, reply, &result)?;
        Ok(result.len() as i64)
    })?;
    Ok(())
}
