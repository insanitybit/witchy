//! RFC-0032 multi-core / cross-VM runtime helpers: the `vm.par_map` staging
//! (scalar + bytes), the guest allocator `$__galloc`, the closure-call
//! trampolines (`$__call_idx`/`$__call2`), and the `vm.serve`/`vm.with_dir`
//! entry helpers. Split out of `wir_helpers/mod.rs`; re-exported by the parent.

use super::host::{two_phase_helper, two_phase_helper_typed};
use crate::wir::*;

/// `$vm_par_map(xs, f) -> i32` — scalar `vm.par_map`: results as a flat `List(Int)`.
pub(super) fn vm_par_map_helper() -> WirFunc {
    two_phase_helper("vm_par_map", &["xs", "f"], "vm_par_map_run", "vm_par_map_write")
}


/// `$vm_par_map_bytes(xs, f) -> i32` — the `String`/`Bytes` `vm.par_map`: raw buffer
/// payloads, results as a `List(Bytes)`/`List(String)` (identical layout).
pub(super) fn vm_par_map_bytes_helper() -> WirFunc {
    two_phase_helper("vm_par_map_bytes", &["xs", "f"], "vm_par_map_bytes_run", "vm_par_map_bytes_write")
}

/// (RFC-0032) `$__galloc(len) -> i32` — a bump allocator export the host calls to place
/// bytes (e.g. a `vm.par_map` input string) into a worker VM's memory: grow if needed,
/// return the old heap top, advance. Mirrors the inline `__galloc` the string-export
/// wrappers expose; emitted for a worker when the `String` `vm.par_map` path is linked.
/// (RFC-0051 I2) Delegates to `$bump_alloc` — the single ensure-prefixed allocator —
/// rather than bumping `$heap` itself. The worker's whole-heap-drop model wants no
/// `[rc][size]` header, so it takes the shared bump core, not `$rc_alloc`.
pub fn galloc_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    WirFunc {
        name: "__galloc".into(),
        params: vec![WirLocal { name: "len".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::Push(E::Call {
            func: "bump_alloc".into(),
            args: vec![E::GetLocal("len".into())],
        })],
        raw_body: None,
    }
}

/// (RFC-0032) A closure-call trampoline `$name(idx, x0..x{arity-1}) -> i64`: `call_indirect`s
/// table slot `idx` with a null uniform-wrapper reference and the `arity` slot args. Sound because
/// `vm.par_map`/`vm.with_dir`/`vm.serve` all require capture-free functions, so the body never
/// reads the environment — and a worker VM has its own linear memory, where a parent closure's
/// environment record would not live anyway. The host invokes these re-entrantly (including
/// from a fresh worker VM) to apply the user function to one element.
fn call_trampoline_helper(name: &str, arity: usize) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let arg_names: Vec<String> = (0..arity).map(|i| format!("x{i}")).collect();
    let mut params = vec![WirLocal { name: "idx".into(), ty: WirTy::Bool }];
    params.extend(arg_names.iter().map(|n| WirLocal { name: n.clone(), ty: WirTy::Int }));
    let mut args = vec![E::RefNull(Kind::GcRef(0))];
    args.extend(arg_names.iter().map(|n| E::GetLocal(n.clone())));
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Int],
        locals: vec![],
        body: vec![N::Push(E::CallIndirect {
            signature: gc_slot_closure_signature(arity, 1),
            args,
            index: Box::new(E::GetLocal("idx".into())),
        })],
        raw_body: None,
    }
}

/// `$__call_idx(idx, arg) -> i64` — the one-argument closure trampoline (`vm.par_map`'s
/// `fn(T) -> U`). See [`call_trampoline_helper`].
pub fn call_idx_helper() -> WirFunc {
    call_trampoline_helper("__call_idx", 1)
}

/// `$__call2(idx, a, b) -> i64` — the two-argument closure trampoline (a capability handle or
/// state + a `Bytes` pointer, for `vm.with_dir`'s `fn(Dir, Bytes) -> Bytes` and `vm.serve`'s
/// `fn(State, Bytes) -> State`). See [`call_trampoline_helper`].
pub fn call2_helper() -> WirFunc {
    call_trampoline_helper("__call2", 2)
}

/// (RFC-0032) `$vm_serve(init, requests, handler) -> i32` — a stateful service on a
/// long-lived isolated worker VM: process the request stream in order, threading state
/// through `handler`, emitting each new state. The host stages the response `List(Bytes)`
/// (`vm_serve_run`), laid out by `vm_par_map_bytes_write` (same `List(Bytes)` structure).
pub(super) fn vm_serve_helper() -> WirFunc {
    two_phase_helper(
        "vm_serve",
        &["init", "requests", "handler"],
        "vm_serve_run",
        "vm_par_map_bytes_write",
    )
}

/// (RFC-0032) `$vm_with_dir(dir, f, input) -> i32` — capability-passing: run `f` on
/// `input` in an isolated worker VM granted exactly `dir`. The host stages the result
/// `Bytes` (`vm_with_dir_run`), which `fill_pending` lays out into the reserved block.
pub(super) fn vm_with_dir_helper() -> WirFunc {
    two_phase_helper_typed(
        "vm_with_dir",
        &[
            ("dir".to_string(), WirTy::Extern),
            ("f".to_string(), WirTy::Bool),
            ("input".to_string(), WirTy::Bool),
        ],
        "vm_with_dir_run",
        "fill_pending",
    )
}
