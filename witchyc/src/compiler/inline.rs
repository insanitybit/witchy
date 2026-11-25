//! Small source functions written into their callers.
//!
//! Wasmtime does not inline calls, and a library method such as `Vec.len` is a call around one
//! load. This pass works on finished function bodies: a `call` of a small source function becomes
//! that function's instructions in a block, with its locals renumbered after the caller's and each
//! `return` a branch out of the block.
//!
//! Only a callee that calls none of the functions being rewritten is written in (a call to an
//! import or a helper is copied as it is: 2026-11-18, binary-trees 477 -> 436 ms, `str` 225 -> 206 ms); after a round, a function whose calls were all
//! written in is such a callee for the next (`ROUNDS`). Measured in Wasmtime: a small scalar function
//! called in a loop ran 2.4 times faster written in, while `Vec.get`, which calls the allocator's
//! access helper, ran no faster and only made the module larger. The allocator's helpers are never
//! candidates. A final pass may inline a small wrapper at a loop call site while keeping its
//! single call to a larger source function. The functions that may name the allocator's memory are exactly those the engine expects.
use super::emit::indexed;

pub(super) struct Body {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
    /// Value types of the locals after the parameters.
    pub locals: Vec<u8>,
    /// Instructions, ending with the function's `end`.
    pub code: Vec<u8>,
}

/// The largest callee, in instruction bytes, that is written into its callers. 400 and four
/// rounds (2026-11-18, from 200 and three): binary-trees 447 -> 367 ms, sort 150 -> 133 ms, others
/// unchanged; modules 25-45% larger and about 1 ms more startup. 800 and five gained nothing more.
const SMALL: usize = 400;
/// No expansion once the module's source code has reached this many bytes...
const MODULE: usize = 40_000;
/// ...except of a callee this small, which grows the module by about what the call took (an
/// accessor such as `Vec.len`). A large program stopped writing even these in. 2026-11-19, a
/// 968 KB JSON parse: 24.2 -> 23.5 ms. 96 bytes (taking `Vec.as_slice`, and so `String.as_bytes`)
/// gave 22.2 ms but modules 8-12% larger: `packages/download-app.wy` went from 64.6 to 72.3 KB,
/// past the host's default 64 KiB; not kept.
const TINY: usize = 32;
/// A callee up to this size (`Vec.as_slice`, 70 bytes after the code-size passes, and
/// `String.as_bytes` once it has written `as_slice` in, about 78; 96 for the two: JSON 20.9 ->
/// 19.8 ms more, `httpd-app` 56,541 -> 58,461 bytes) is also written
/// in past `MODULE` while the source functions' code stays under `ROOM` (that total leaves out the
/// allocator's helpers and the other sections, about a fifth of a module). 2026-11-19: the JSON
/// parse 21.4 -> 20.8 ms, its module 55,039 -> 56,340 bytes; `download-app` unchanged at 61,774
/// (a flat 72 put it at 65,123, 413 bytes under the host's default 64 KiB).
const SMALL_TINY: usize = 96;
const ROOM: usize = 44_000;
const LOCALS: usize = 2_000;
const DEPTH: u32 = 96;

#[derive(Clone, Copy)]
pub(super) struct Instruction { pub at: usize, pub end: usize, pub op: u8, pub index: u32 }

/// The mark (`i32.const 9; drop` before `unreachable`) of a trap written in from a callee, after the
/// callee's function index; the host's `trap_reason` reads both.
const INLINED_TRAP: u8 = 9;
/// The `unreachable` at `at` needs no mark: one is before it (`i32.const k; drop`), or it follows a
/// `return`, `br` or `unreachable` and so is never reached (the end of most functions).
fn marked(instructions: &[Instruction], at: usize) -> bool {
    (at >= 2 && instructions[at - 1].op == 0x1a && instructions[at - 2].op == 0x41)
        || (at >= 1 && matches!(instructions[at - 1].op, 0x0f | 0x0c | 0x00))
}

fn leb(code: &[u8], at: &mut usize) -> Option<u64> {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = *code.get(*at)?;
        *at += 1;
        if shift < 64 { value |= u64::from(byte & 127) << shift; }
        shift += 7;
        if byte & 128 == 0 { return Some(value); }
    }
}

/// Every instruction with where it starts and ends, or `None` for anything this pass does not know.
pub(super) fn decode(code: &[u8]) -> Option<Vec<Instruction>> {
    decode_body(code, false)
}

fn decode_body(code: &[u8], scan_simd: bool) -> Option<Vec<Instruction>> {
    let (mut out, mut at) = (Vec::new(), 0);
    while at < code.len() {
        let (start, op) = (at, code[at]);
        at += 1;
        let mut index = 0;
        match op {
            0x02..=0x04 => { if !matches!(code.get(at)?, 0x40 | 0x7f | 0x7e | 0x7c) { return None; } at += 1; }
            0x0c | 0x0d | 0x10 | 0x20..=0x24 => index = leb(code, &mut at)? as u32,
            0x28..=0x3e => { if leb(code, &mut at)? & 0x40 != 0 { leb(code, &mut at)?; } leb(code, &mut at)?; }
            0x41 | 0x42 => { leb(code, &mut at)?; }
            // The reachability scan only needs instruction boundaries. Keep the
            // scalar rewriting passes conservative around vector stack values.
            0xfd if scan_simd => {
                let immediate = match leb(code, &mut at)? {
                    0x0c | 0x0d => 16, // v128.const and i8x16.shuffle
                    0x1d | 0x1e => 1, // i64x2.extract_lane and replace_lane
                    0x12 | 0x23 | 0x28 | 0x2d | 0x32 | 0x37 | 0x3c |
                    0x4e | 0x50 | 0x51 | 0x64 | 0x6e | 0x71 | 0x8e |
                    0x91 | 0x95 | 0x9e | 0x9f | 0xae | 0xb1 | 0xb5 |
                    0xce | 0xd1 | 0xd5 | 0xd6 | 0xd9 => 0,
                    _ => return None,
                };
                code.get(at..at + immediate)?;
                at += immediate;
            }
            // f64.const: eight bytes.
            0x44 => { if at + 8 > code.len() { return None; } at += 8; }
            0x00 | 0x01 | 0x05 | 0x0b | 0x0f | 0x1a | 0x1b | 0x45..=0xc4 => {}
            _ => return None,
        }
        out.push(Instruction { at: start, end: at, op, index });
    }
    (out.last()?.op == 0x0b).then_some(out)
}

struct Callee { code: Vec<u8>, instructions: Vec<Instruction>, params: Vec<u8>, results: Vec<u8>, locals: Vec<u8>, depth: u32,
    /// Locals after the parameters that are read before a certain write: a call starts them at zero, so the copy must.
    zeroed: Vec<u32> }

/// `bodies` is the index range of the functions being rewritten: a call to one of them is left
/// to a later round, while a call outside it (an import or an allocator helper) names the same
/// function wherever it is written, so it may be copied in.
fn callee(body: &Body, decoded: Option<&Vec<Instruction>>, bodies: std::ops::Range<u32>) -> Option<Callee> {
    if body.code.len() > SMALL { return None; }
    // Zeroing writes an i32, i64 or f64 zero; a callee with a local of another type (f32, v128:
    // none is emitted today) is left to a real call rather than given a wrong zero (review).
    if body.locals.iter().any(|ty| !matches!(ty, 0x7f | 0x7e | 0x7c)) { return None; }
    let instructions = decoded?.clone();
    let (mut depth, mut deepest) = (0u32, 0u32);
    // Per local: whether each written-in call must zero it first. A local is safe when its first
    // access is a write that certainly runs before any read: one before any instruction that can
    // skip code (`if`, `else`, a branch, `return`), or outside every block; or one whose reads all
    // lie inside the block that holds the write, after it. Control enters a block only at its top
    // and a branch only leaves a block or restarts a loop, so any path to such a read passes the
    // write (an `else` arm is a block apart from its `then` arm).
    #[derive(Clone, PartialEq)]
    enum Seen { Unseen, Certain, Scoped(Vec<u32>), Zero }
    let mut seen = vec![Seen::Unseen; body.params.len() + body.locals.len()];
    seen[..body.params.len()].fill(Seen::Certain);
    let (mut path, mut next) = (Vec::<u32>::new(), 0u32);
    let mut branched = false;
    for (position, instruction) in instructions.iter().enumerate() {
        if matches!(instruction.op, 0x04 | 0x05 | 0x0c..=0x0f) { branched = true; }
        match instruction.op {
            0x02..=0x04 => { depth += 1; deepest = deepest.max(depth); next += 1; path.push(next); }
            0x05 => { path.pop()?; next += 1; path.push(next); }
            0x0b if position + 1 < instructions.len() => { depth = depth.checked_sub(1)?; path.pop()?; }
            // A branch to the function's own label, the stack global and any call are left to a real call.
            0x0c | 0x0d if instruction.index >= depth => return None,
            0x23 | 0x24 => return None,
            0x10 if bodies.contains(&instruction.index) => return None,
            0x20..=0x22 => {
                let state = seen.get_mut(instruction.index as usize)?;
                let read = instruction.op == 0x20;
                *state = match std::mem::replace(state, Seen::Zero) {
                    Seen::Unseen if read => Seen::Zero,
                    Seen::Unseen if depth == 0 || !branched => Seen::Certain,
                    Seen::Unseen => Seen::Scoped(path.clone()),
                    Seen::Scoped(at) if read && !path.starts_with(&at) => Seen::Zero,
                    other => other,
                };
            }
            _ => {}
        }
    }
    let zeroed = seen.iter().enumerate().filter(|(_, s)| **s == Seen::Zero).map(|(i, _)| i as u32).collect();
    Some(Callee { code: body.code.clone(), instructions, params: body.params.clone(), results: body.results.clone(), locals: body.locals.clone(), depth: deepest, zeroed })
}

/// Rounds of writing in: a function whose calls were all written in during one round is itself a
/// leaf in the next, so `items[i]` (which calls `Vec.index`, which calls `Vec.get`) ends as code.
const ROUNDS: usize = 4;

/// Rewrites `bodies` in place. `first` is the function index of `bodies[0]`.
pub(super) fn run(bodies: &mut [Body], first: u32) {
    for _ in 0..ROUNDS { if !round(bodies, first, false) { break; } }
    round(bodies, first, true);
    for body in bodies.iter_mut() { super::forward::run(body); tee(body); compact(body); }
}

/// Trace direct calls from the sole function export in a non-task module.
/// Preserve all indices and trap names when dropping uncalled source and runtime bodies.
pub(super) fn trim_uncalled(bodies: &mut [Vec<u8>], first: u32, main: u32) {
    let mut live = vec![false; bodies.len()];
    let mut queue = vec![main as usize];
    while let Some(index) = queue.pop() {
        if live[index] { continue; }
        live[index] = true;
        let body = &bodies[index];
        let mut at = 0;
        let Some(groups) = leb(body, &mut at) else { return };
        for _ in 0..groups {
            if leb(body, &mut at).is_none() || !matches!(body.get(at), Some(0x7f | 0x7e | 0x7c)) {
                return;
            }
            at += 1;
        }
        let Some(instructions) = decode_body(&body[at..], true) else { return };
        for op in instructions {
            if op.op == 0x10 {
                if let Some(target) = op.index.checked_sub(first) {
                    if (target as usize) < bodies.len() { queue.push(target as usize); }
                }
            }
        }
    }
    for (body, reachable) in bodies.iter_mut().zip(live) {
        if !reachable {
            body.clear();
            body.extend([0, 0x00, 0x0b]);
        }
    }
}

/// The locals no instruction names any more (after writing in and `tee`) are not declared, and the
/// rest are renumbered grouped by type, parameters untouched, so each type is one declaration
/// entry (2026-11-19: 10-20% of a benchmark module's declared locals were never used).
fn compact(body: &mut Body) {
    let Some(instructions) = decode(&body.code) else { return };
    let params = body.params.len();
    let mut used = vec![false; body.locals.len()];
    for instruction in &instructions {
        if matches!(instruction.op, 0x20..=0x22) && instruction.index as usize >= params {
            // An index past the declared locals (invalid Wasm, never emitted): the body is left alone.
            let Some(slot) = used.get_mut(instruction.index as usize - params) else { return };
            *slot = true;
        }
    }
    // The ones kept, grouped by type (in order within each), so the declaration is one entry a type.
    let mut kept: Vec<usize> = (0..body.locals.len()).filter(|l| used[*l]).collect();
    kept.sort_by_key(|l| body.locals[*l]);
    let mut number = vec![0u32; body.locals.len()];
    for (position, local) in kept.iter().enumerate() { number[*local] = (params + position) as u32; }
    let mut out = Vec::with_capacity(body.code.len());
    for instruction in &instructions {
        if matches!(instruction.op, 0x20..=0x22) && instruction.index as usize >= params {
            indexed(&mut out, instruction.op, number[instruction.index as usize - params]);
        } else {
            out.extend(&body.code[instruction.at..instruction.end]);
        }
    }
    body.code = out;
    body.locals = kept.iter().map(|l| body.locals[*l]).collect();
}

/// `local.set x; local.get x` written as `local.tee x`: the same value stays on the stack, one
/// instruction fewer (2026-11-19: about 2.4% of the instructions of the benchmark modules).
///
/// A write to a local the function never reads is dropped: `local.set x` is `drop`, `local.tee x`
/// is nothing, and a `local.get y` whose value is then dropped is not written (a copy kept for a
/// reader that never came: 121-267 such writes in a benchmark module). An `i32.const k; drop` is
/// kept: before an `unreachable` it is the trap's reason, which the host reads.
fn tee(body: &mut Body) {
    let Some(instructions) = decode(&body.code) else { return };
    let mut read = vec![false; body.params.len() + body.locals.len()];
    for instruction in &instructions {
        if instruction.op == 0x20 { if let Some(r) = read.get_mut(instruction.index as usize) { *r = true; } }
    }
    let unread = |i: &Instruction| matches!(i.op, 0x21 | 0x22) && read.get(i.index as usize) == Some(&false);
    let mut out = Vec::with_capacity(body.code.len());
    let mut skip = false;
    for (at, instruction) in instructions.iter().enumerate() {
        if skip { skip = false; continue; }
        let next = instructions.get(at + 1);
        // Nothing is dropped just before an `unreachable`: `i32.const k; local.set x; unreachable`
        // would become `i32.const k; drop; unreachable`, which the host reads as trap reason k
        // (review of 61f9fe17).
        let traps_after = |offset: usize| instructions.get(at + offset).is_some_and(|i| i.op == 0x00);
        // An empty `else` (`else` then `end`): the `if` it closes has no result (one with a
        // result needs a value from both arms), so the `else` is not needed.
        if instruction.op == 0x05 && next.is_some_and(|n| n.op == 0x0b) { continue; }
        // `local.get y; local.set x` with x never read: neither is needed.
        if instruction.op == 0x20 && next.is_some_and(|n| n.op == 0x21 && unread(n)) && !traps_after(2) { skip = true; continue; }
        // A constant set into a never-read local (a callee's zeroing, written in): neither is needed.
        if matches!(instruction.op, 0x41 | 0x42) && next.is_some_and(|n| n.op == 0x21 && unread(n)) && !traps_after(2) { skip = true; continue; }
        // Nor is an unread `local.tee` taken from before `drop; unreachable`: a constant before it
        // would be left as that same pattern (review of the forwarding pass, 2026-11-22).
        let reason_after = instruction.op == 0x22 && next.is_some_and(|n| n.op == 0x1a) && traps_after(2);
        if unread(instruction) && !traps_after(1) && !reason_after { if instruction.op == 0x21 { out.push(0x1a); } continue; }
        if instruction.op == 0x21 && next.is_some_and(|n| n.op == 0x20 && n.index == instruction.index) {
            indexed(&mut out, 0x22, instruction.index);
            skip = true;
            continue;
        }
        out.extend(&body.code[instruction.at..instruction.end]);
    }
    body.code = out;
}

/// One round over every body. Whether anything was written in.
fn round(bodies: &mut [Body], first: u32, wrappers: bool) -> bool {
    let range = first..first + bodies.len() as u32;
    // Each body decoded once a round, for judging it as a callee and for rewriting its calls.
    let mut decoded = bodies.iter().map(|body| decode(&body.code)).collect::<Vec<_>>();
    let callees = bodies.iter().zip(&decoded).map(|(body, instructions)| {
        if !wrappers {
            return callee(body, instructions.as_ref(), range.clone());
        }
        // One final round can expose a small fast path whose slow path remains
        // a call. Require a single, larger source callee and keep the size cap.
        let mut calls = instructions.as_ref()?.iter()
            .filter(|i| i.op == 0x10 && range.contains(&i.index));
        let target = (calls.next()?.index - first) as usize;
        if calls.next().is_some() || bodies[target].code.len() <= body.code.len() {
            return None;
        }
        callee(body, instructions.as_ref(), 0..0)
    }).collect::<Vec<_>>();
    if callees.iter().all(Option::is_none) { return false; }
    let mut changed = false;
    let mut total = bodies.iter().map(|body| body.code.len()).sum::<usize>();
    for (position, body) in bodies.iter_mut().enumerate() {
        let Some(instructions) = decoded[position].take() else { continue };
        if !instructions.iter().any(|i| i.op == 0x10 && i.index >= first && callees.get((i.index - first) as usize).is_some_and(Option::is_some)) { continue; }
        let mut out = Vec::with_capacity(body.code.len() * 2);
        let mut depth = 0u32;
        let mut outer_loop = None;
        // Where each callee's locals were placed in this body this round: a later call of the same
        // callee uses the same locals. A retained call has its own Wasm activation and cannot
        // access these locals. A local read before the callee writes it is
        // zeroed at each call, so what one call left there is never seen by the next.
        let mut placed: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        for (at, instruction) in instructions.iter().enumerate() {
            match instruction.op {
                0x02..=0x04 => {
                    depth += 1;
                    if instruction.op == 0x03 && outer_loop.is_none() { outer_loop = Some(depth); }
                }
                0x0b if at + 1 < instructions.len() => {
                    if outer_loop == Some(depth) { outer_loop = None; }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            let target = ((!wrappers || outer_loop.is_some()) && instruction.op == 0x10 && instruction.index >= first && instruction.index - first != position as u32)
                .then(|| callees.get((instruction.index - first) as usize).and_then(Option::as_ref)).flatten()
                .filter(|callee| (total + callee.code.len() + 64 <= MODULE || callee.code.len() <= TINY
                    || (callee.code.len() <= SMALL_TINY && total + callee.code.len() + 64 <= ROOM)) && depth + callee.depth + 1 <= DEPTH
                    // A call that reuses locals already placed adds none (review of 6f3f7756).
                    && (placed.contains_key(&instruction.index)
                        || body.params.len() + body.locals.len() + callee.params.len() + callee.locals.len() + callee.results.len() <= LOCALS));
            let Some(callee) = target else { out.extend(&body.code[instruction.at..instruction.end]); continue; };
            let before = out.len();
            // The callee's parameters and locals after the caller's own. One result is the block's value;
            // several go through a local each, since a block of this profile has at most one.
            let single = callee.results.len() == 1;
            let base = match placed.get(&instruction.index) {
                Some(base) => *base,
                None => {
                    let base = (body.params.len() + body.locals.len()) as u32;
                    body.locals.extend(callee.params.iter().chain(&callee.locals));
                    if !single { body.locals.extend(&callee.results); }
                    placed.insert(instruction.index, base);
                    base
                }
            };
            let results = base + (callee.params.len() + callee.locals.len()) as u32;
            for parameter in (0..callee.params.len() as u32).rev() { indexed(&mut out, 0x21, base + parameter); }
            for local in &callee.zeroed {
                let ty = callee.locals[*local as usize - callee.params.len()];
                // Zero of the local's own type (an f64 local took an i64 zero: invalid Wasm).
                match ty { 0x7f => out.extend([0x41, 0]), 0x7c => { out.push(0x44); out.extend(0f64.to_le_bytes()); } _ => out.extend([0x42, 0]) }
                indexed(&mut out, 0x21, base + local);
            }
            let leave = |out: &mut Vec<u8>| if !single { for result in (0..callee.results.len() as u32).rev() { indexed(out, 0x21, results + result); } };
            out.extend([0x02, if single { callee.results[0] } else { 0x40 }]);
            let mut inner = 0u32;
            let called = instruction.index;
            // A `return` at the callee's top level with nothing after it but a never-reached
            // `unreachable` and the final `end` falls off the end instead: no `br 0`, no trap.
            let mut fell = false;
            for (at, instruction) in callee.instructions.iter().enumerate() {
                let last = at + 1 == callee.instructions.len();
                match instruction.op {
                    0x20..=0x22 => indexed(&mut out, instruction.op, base + instruction.index),
                    0x0f if inner == 0 && callee.instructions[at + 1..].iter().all(|i| i.op == 0x00 || i.op == 0x0b)
                        && callee.instructions.len() - at <= 3 => { leave(&mut out); fell = true; }
                    0x0f => { leave(&mut out); indexed(&mut out, 0x0c, inner); }
                    0x00 if fell => {}
                    0x0b if last => {
                        // Falling off the end with the results on the stack, unless the end cannot be reached.
                        if at == 0 || !matches!(callee.instructions[at - 1].op, 0x00 | 0x0f | 0x0c) { leave(&mut out); }
                        out.push(0x0b);
                    }
                    // A trap with no reason marked before it (an `unreachable` written in the callee)
                    // is marked with the callee it came from: `i32.const index` (five bytes, so it is
                    // read back from the end), `drop`, `i32.const 9`, `drop`. The host names the
                    // function from the module's names (tools/wasmtime-host, `trap_reason`).
                    0x00 if !marked(&callee.instructions, at) => {
                        out.push(0x41);
                        let mut rest = called;
                        for byte in 0..5 { let low = (rest & 0x7f) as u8; rest >>= 7; out.push(if byte < 4 { low | 0x80 } else { low }); }
                        out.extend([0x1a, 0x41, INLINED_TRAP, 0x1a, 0x00]);
                    }
                    op => {
                        if matches!(op, 0x02..=0x04) { inner += 1; } else if op == 0x0b { inner -= 1; }
                        out.extend(&callee.code[instruction.at..instruction.end]);
                    }
                }
            }
            if !single { for result in 0..callee.results.len() as u32 { indexed(&mut out, 0x20, results + result); } }
            total += out.len() - before;
            changed = true;
        }
        body.code = out;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body(params: &[u8], results: &[u8], locals: &[u8], code: &[u8]) -> Body {
        Body { params: params.to_vec(), results: results.to_vec(), locals: locals.to_vec(), code: code.to_vec() }
    }
    #[test]
    fn trimming_follows_only_decoded_calls_from_the_export() {
        let make = || vec![
            vec![0, 0x10, 8, 0x0b],
            vec![0, 0x01, 0x0b],
            vec![0, 0x10, 3, 0x0b],
            vec![1, 2, 0x7e, 0x01, 0x0b],
        ];
        let mut bodies = make();
        trim_uncalled(&mut bodies, 6, 0);
        assert_eq!(bodies[0], [0, 0x10, 8, 0x0b]);
        assert_eq!(bodies[2], [0, 0x10, 3, 0x0b]);
        assert_eq!(bodies[1], [0, 0x00, 0x0b]);
        assert_eq!(bodies[3], [0, 0x00, 0x0b]);
        let mut packed = make();
        let mut code = vec![1, 2, 0x7e, 0xfd, 0x0c];
        code.extend([0x10; 16]); // payload bytes must never be mistaken for calls
        code.extend([0xfd, 0x1d, 0, 0x1a, 0x10, 8, 0x0b]);
        packed[0] = code;
        trim_uncalled(&mut packed, 6, 0);
        assert_eq!(packed[1], [0, 0x00, 0x0b]);
        assert_eq!(packed[2], [0, 0x10, 3, 0x0b]);
        assert!(decode_body(&[0xfd, 0x0c, 0x0b], true).is_none());
        let mut unknown = make();
        unknown[2] = vec![0, 0xfd, 0x00, 0x0b];
        trim_uncalled(&mut unknown, 6, 0);
        assert_eq!(unknown[3], make()[3]);
    }

    #[test]
    fn a_loop_inlines_a_wrapper_and_keeps_its_slow_call() {
        let mut slow = vec![0x01; SMALL + 1];
        slow.push(0x0b);
        let mut bodies = [
            body(&[], &[], &[], &slow),
            body(&[], &[], &[], &[0x10, 0, 0x0b]),
            body(&[], &[], &[], &[0x03, 0x40, 0x10, 1, 0x0b, 0x10, 1, 0x0b]),
        ];
        run(&mut bodies, 0);
        let calls: Vec<_> = decode(&bodies[2].code).unwrap().into_iter()
            .filter(|i| i.op == 0x10).map(|i| i.index).collect();
        assert_eq!(calls, [0, 1]);
    }

    #[test]
    fn a_small_callee_is_written_in_with_its_locals_moved_and_its_return_a_branch() {
        // callee(a) = if a == 0 { return 7 } ; a + 1, written with a scratch local read before any write.
        let callee = body(&[0x7e], &[0x7e], &[0x7e], &[0x20, 0, 0x50, 0x04, 0x40, 0x42, 7, 0x0f, 0x0b, 0x20, 1, 0x20, 0, 0x7c, 0x42, 1, 0x7c, 0x0f, 0x00, 0x0b]);
        let caller = body(&[], &[0x7e], &[0x7e], &[0x42, 5, 0x10, 3, 0x21, 0, 0x20, 0, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        // The callee's instructions in a block whose value is its one result; the `return` inside
        // the `if` is a branch out, the final one falls off the end (no `br 0; unreachable`).
        // Its parameter was set from the constant 5 and its scratch local zeroed: the forwarding
        // pass reads the constants where those locals were read, and no local is left
        // (the caller's own `local.set 0; local.get 0`, its only read, goes too).
        assert_eq!(bodies[1].locals, [] as [u8; 0]);
        assert_eq!(bodies[1].code, [0x02, 0x7e,
            0x42, 5, 0x50, 0x04, 0x40, 0x42, 7, 0x0c, 1, 0x0b,
            0x42, 0, 0x42, 5, 0x7c, 0x42, 1, 0x7c, 0x0b,
            0x0b]);
        assert_eq!(bodies[0].code.len(), 20, "the callee itself is unchanged");
    }

    #[test]
    fn a_reachable_trap_written_in_is_marked_with_its_function() {
        // callee(a) = if a { unreachable } ; a
        let callee = body(&[0x7f], &[0x7f], &[], &[0x20, 0, 0x04, 0x40, 0x00, 0x0b, 0x20, 0, 0x0b]);
        let caller = body(&[], &[0x7f], &[], &[0x41, 1, 0x10, 3, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        // (The argument, a constant, is read where the parameter was.)
        assert_eq!(bodies[1].code, [0x02, 0x7f,
            0x41, 1, 0x04, 0x40, 0x41, 0x83, 0x80, 0x80, 0x80, 0x00, 0x1a, 0x41, 9, 0x1a, 0x00, 0x0b, 0x41, 1, 0x0b, 0x0b]);
    }
    /// An f64 local read before it is written starts at an f64 zero, and `f64.const` is read
    /// whole (it took an i64 zero: invalid Wasm).
    #[test]
    fn an_f64_local_is_zeroed_as_an_f64() {
        // callee() = local + 1.5, its local never written.
        let mut code = vec![0x20, 0, 0x44];
        code.extend(1.5f64.to_le_bytes());
        code.extend([0xa0, 0x0b]);
        let callee = body(&[], &[0x7c], &[0x7c], &code);
        let caller = body(&[], &[0x7c], &[], &[0x10, 3, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        assert_eq!(bodies[1].locals, [0x7c]);
        let mut zero = vec![0x44];
        zero.extend(0f64.to_le_bytes());
        zero.extend([0x21, 0]);
        assert!(bodies[1].code.starts_with(&zero), "{:?}", bodies[1].code);
    }
    /// `local.set 0; local.get 0` becomes `local.tee 0`; a get of another local is left as it is.
    #[test]
    fn a_set_then_get_of_one_local_is_a_tee() {
        let mut same = body(&[], &[0x7e], &[0x7e, 0x7e], &[0x42, 7, 0x21, 0, 0x20, 0, 0x0b]);
        tee(&mut same);
        assert_eq!(same.code, [0x42, 7, 0x22, 0, 0x0b]);
        // Local 0 is never read: its set is a `drop`; set from a constant, neither is written.
        let mut other = body(&[], &[0x7e], &[0x7e, 0x7e], &[0x42, 7, 0x21, 0, 0x20, 1, 0x0b]);
        tee(&mut other);
        assert_eq!(other.code, [0x20, 1, 0x0b]);
        let mut computed = body(&[], &[0x7e], &[0x7e, 0x7e], &[0x20, 1, 0x20, 1, 0x7c, 0x21, 0, 0x20, 1, 0x0b]);
        tee(&mut computed);
        assert_eq!(computed.code, [0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x20, 1, 0x0b]);
        // `local.get 1; local.set 0` with local 0 never read: neither is written. A trap's
        // `i32.const 3; drop; unreachable` is kept.
        let mut copy = body(&[], &[], &[0x7e, 0x7e], &[0x20, 1, 0x21, 0, 0x41, 3, 0x1a, 0x00, 0x0b]);
        tee(&mut copy);
        assert_eq!(copy.code, [0x41, 3, 0x1a, 0x00, 0x0b]);
        // A constant zeroing a never-read local is not written at all.
        let mut zeroing = body(&[], &[], &[0x7e], &[0x42, 0, 0x21, 0, 0x0b]);
        tee(&mut zeroing);
        assert_eq!(zeroing.code, [0x0b]);
        // `if ... else end` loses its empty `else`.
        let mut empty_else = body(&[], &[], &[], &[0x41, 1, 0x04, 0x40, 0x01, 0x05, 0x0b, 0x0b]);
        tee(&mut empty_else);
        assert_eq!(empty_else.code, [0x41, 1, 0x04, 0x40, 0x01, 0x0b, 0x0b]);
        // `i32.const 1; local.set 0; unreachable` with local 0 never read stays as it is: as a
        // `drop` it would read as trap reason 1.
        let mut before_trap = body(&[], &[], &[0x7f], &[0x41, 1, 0x21, 0, 0x00, 0x0b]);
        tee(&mut before_trap);
        assert_eq!(before_trap.code, [0x41, 1, 0x21, 0, 0x00, 0x0b]);
    }
    /// A local no instruction names is not declared, and the ones after it are renumbered.
    #[test]
    fn an_unused_local_is_not_declared() {
        // param 0; locals 1 (unused, f64), 2 (i64): local.get 2; local.get 0; i64.add
        let mut b = body(&[0x7e], &[0x7e], &[0x7c, 0x7e], &[0x20, 2, 0x20, 0, 0x7c, 0x0b]);
        compact(&mut b);
        assert_eq!(b.locals, [0x7e]);
        assert_eq!(b.code, [0x20, 1, 0x20, 0, 0x7c, 0x0b]);
    }

    /// Two calls of one callee in a body share its locals: the caller gains them once.
    /// `i32.const 5; local.tee x; drop; unreachable` with x unread keeps its tee: without it the
    /// host would read trap reason 5.
    #[test]
    fn an_unread_tee_before_a_dropped_trap_stays() {
        let code = [0x20, 0, 0x21, 1, 0x41, 5, 0x22, 0, 0x1a, 0x00, 0x0b];
        let mut bodies = [body(&[0x7f], &[], &[0x7f], &code)];
        run(&mut bodies, 0);
        assert_eq!(bodies[0].code, [0x41, 5, 0x22, 0, 0x1a, 0x00, 0x0b]);
    }
    #[test]
    fn two_calls_of_one_callee_share_its_locals() {
        // callee(a) = a + 1; caller() = callee(1) + callee(2).
        let callee = body(&[0x7e], &[0x7e], &[], &[0x20, 0, 0x42, 1, 0x7c, 0x0b]);
        let caller = body(&[], &[0x7e], &[], &[0x42, 1, 0x10, 3, 0x42, 2, 0x10, 3, 0x7c, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        // Each call's argument is a constant, read where the shared parameter local was.
        assert_eq!(bodies[1].locals, [] as [u8; 0]);
        assert_eq!(bodies[1].code, [0x02, 0x7e, 0x42, 1, 0x42, 1, 0x7c, 0x0b, 0x02, 0x7e, 0x42, 2, 0x42, 1, 0x7c, 0x0b, 0x7c, 0x0b]);
        // With arguments that are not constants the parameter local is kept, once for both calls:
        // caller(p) = callee(p * p) + callee(p * p).
        let callee = body(&[0x7e], &[0x7e], &[], &[0x20, 0, 0x20, 0, 0x7c, 0x0b]);
        let caller = body(&[0x7e], &[0x7e], &[], &[0x20, 0, 0x20, 0, 0x7e, 0x10, 3, 0x20, 0, 0x20, 0, 0x7e, 0x10, 3, 0x7c, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        assert_eq!(bodies[1].locals, [0x7e], "one local for both calls' parameter");
    }

    /// A callee ending `return; unreachable; end` is written in falling off its block's end: no
    /// `br 0`, no trap marker.
    #[test]
    fn a_final_return_falls_off_the_end() {
        // callee() = return 7; unreachable
        let callee = body(&[], &[0x7e], &[], &[0x42, 7, 0x0f, 0x00, 0x0b]);
        let caller = body(&[], &[0x7e], &[], &[0x10, 3, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        assert_eq!(bodies[1].code, [0x02, 0x7e, 0x42, 7, 0x0b, 0x0b]);
    }
    /// A local written inside a block before any branch is written before it is read: not
    /// zeroed. One written only after an `if` may not be: zeroed.
    #[test]
    fn a_local_written_before_any_branch_is_not_zeroed() {
        // block (result i64) { local.set 0 (i64.const 7); local.get 0 } end
        let straight = body(&[], &[0x7e], &[0x7e], &[0x02, 0x7e, 0x42, 7, 0x21, 0, 0x20, 0, 0x0b, 0x0b]);
        assert!(callee(&straight, decode(&straight.code).as_ref(), 0..0).unwrap().zeroed.is_empty());
        // i32.const 1; if { local.set 0 (i64.const 7) } end; local.get 0
        let after_if = body(&[], &[0x7e], &[0x7e], &[0x41, 1, 0x04, 0x40, 0x42, 7, 0x21, 0, 0x0b, 0x20, 0, 0x0b]);
        assert_eq!(callee(&after_if, decode(&after_if.code).as_ref(), 0..0).unwrap().zeroed, [0]);
        // Written and read inside the same `if` arm, after a branch: the read follows the write on
        // every path into it. Not zeroed.
        let same_arm = body(&[], &[], &[0x7e], &[0x41, 1, 0x04, 0x40, 0x42, 7, 0x21, 0, 0x20, 0, 0x1a, 0x0b, 0x0b]);
        assert!(callee(&same_arm, decode(&same_arm.code).as_ref(), 0..0).unwrap().zeroed.is_empty());
        // Written in the `then` arm, read in the `else` arm: zeroed.
        let other_arm = body(&[], &[], &[0x7e], &[0x41, 1, 0x04, 0x40, 0x42, 7, 0x21, 0, 0x05, 0x20, 0, 0x1a, 0x0b, 0x0b]);
        assert_eq!(callee(&other_arm, decode(&other_arm.code).as_ref(), 0..0).unwrap().zeroed, [0]);
        // Written inside a block that a `br_if` can leave early, read after it: zeroed.
        let skipped = body(&[], &[], &[0x7e], &[0x41, 1, 0x04, 0x40, 0x0b, 0x02, 0x40, 0x41, 1, 0x0d, 0, 0x42, 7, 0x21, 0, 0x0b, 0x20, 0, 0x1a, 0x0b]);
        assert_eq!(callee(&skipped, decode(&skipped.code).as_ref(), 0..0).unwrap().zeroed, [0]);
    }
    #[test]
    fn a_function_that_becomes_a_leaf_is_written_in_the_next_round() {
        // leaf() = 9; middle() = leaf(); top() = middle(). Function indices 3, 4, 5.
        let leaf = body(&[], &[0x7e], &[], &[0x42, 9, 0x0b]);
        let middle = body(&[], &[0x7e], &[], &[0x10, 3, 0x0b]);
        let top = body(&[], &[0x7e], &[], &[0x10, 4, 0x0b]);
        let mut bodies = [leaf, middle, top];
        run(&mut bodies, 3);
        assert!(!bodies[2].code.contains(&0x10), "no call is left in top: {:?}", bodies[2].code);
        assert_eq!(bodies[2].code, [0x02, 0x7e, 0x02, 0x7e, 0x42, 9, 0x0b, 0x0b, 0x0b]);
    }
    #[test]
    fn several_results_leave_through_a_local_each() {
        // callee() = (1, 2), by falling off the end.
        let callee = body(&[], &[0x7f, 0x7e], &[], &[0x41, 1, 0x42, 2, 0x0b]);
        let caller = body(&[], &[], &[], &[0x10, 3, 0x1a, 0x1a, 0x0b]);
        let mut bodies = [callee, caller];
        run(&mut bodies, 3);
        // The two result locals, grouped by type by `compact`: the i64 (0x7e) first, then the i32.
        assert_eq!(bodies[1].locals, [0x7e, 0x7f]);
        assert_eq!(bodies[1].code, [0x02, 0x40, 0x41, 1, 0x42, 2, 0x21, 0, 0x21, 1, 0x0b, 0x20, 1, 0x20, 0, 0x1a, 0x1a, 0x0b]);
    }
    #[test]
    fn a_callee_that_calls_the_stack_global_and_unknown_instructions_stay_calls() {
        // A call to an import or helper (here 9, outside the rewritten functions 3 and 4) is copied in as it is.
        let caller = body(&[], &[], &[], &[0x10, 3, 0x0b]);
        let mut bodies = [body(&[], &[], &[], &[0x10, 9, 0x0b]), caller];
        run(&mut bodies, 3);
        assert_eq!(bodies[1].code, [0x02, 0x40, 0x10, 9, 0x0b, 0x0b]);
        // A call to one of the functions being rewritten (4) is left to a later round.
        for code in [vec![0x10, 4, 0x0b], vec![0x23, 0, 0x1a, 0x0b], vec![0xfc, 0x0a, 0, 0, 0x0b], vec![0x02, 0x40, 0x0c, 1, 0x0b, 0x0b]] {
            let caller = body(&[], &[], &[], &[0x10, 3, 0x0b]);
            let mut bodies = [body(&[], &[], &[], &code), caller];
            run(&mut bodies, 3);
            assert_eq!(bodies[1].code, [0x10, 3, 0x0b], "{code:?}");
        }
    }
}
