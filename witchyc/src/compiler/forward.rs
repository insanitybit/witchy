//! Copies forwarded in finished function bodies.
//!
//! The emitter gives every value a local of its own: a callee written in has its arguments set
//! into its parameters' locals, a field read goes through a temporary, a constant is set and
//! read back. Wasmtime's compiler removes those copies, so they cost no time, but each
//! `local.get` and `local.set` is an instruction of the run's count, about a third of any loop
//! (docs/decisions.md, 2026-11-22). This pass reads a body and, where a local is known to hold
//! what another local or a small constant holds, writes the read of the source instead
//! (`forward`); then the writes nothing reads any more go, with the plain read or constant that
//! fed each (`sweep`). `inline::tee` runs after it, as before.
//!
//! What is known at an instruction is what holds on every path to it:
//!
//! - On a straight line: `local.get a; local.set b` (or `local.tee b`; or several reads and then
//!   their sets, as a written-in call's arguments are) makes `b` a copy of `a`, and a small
//!   constant set into `b` makes `b` that constant, until `a` or `b` is written again. Entering
//!   a `block` or an `if` keeps this (control comes in only from the instruction before); a
//!   `loop`, an `else` and an `end` forget it: a branch back to the loop's top, the other arm,
//!   and the branches out of the block being closed all arrive from code that may have written
//!   any local. The `end` of an `if` whose body only traps is the exception: nothing arrives
//!   there but the `if` itself.
//! - For a local the body writes once: that write, when it is such a copy of a constant, of a
//!   parameter the body never writes, or of another once-written local whose write comes before
//!   in a block still open, is what the local holds from there to the end of the block the
//!   write is in, inner blocks and loops included. Control enters a block only at its top and a
//!   branch only leaves a block or restarts a loop, so every path to a later instruction of that
//!   block passes the write; and the source cannot be written again without the copy being
//!   written after it, by the same argument one level up.
//!
//! No call, load, store, branch or trap is moved, added or removed, and each has the same
//! operands, so the count changes only by the copies and no trap changes place. Two rewrites of
//! a test are made on the way, each to the same value: a comparison with a zero just pushed is
//! `eqz`, and an `i64` comparison (or `i32.eq`, `i32.ne`) whose result is then inverted is the
//! opposite comparison. `i32.const k; drop` before an `unreachable` is a trap's reason, which the
//! host reads: no constant is put before a `drop`, and nothing next to an `unreachable` is
//! changed. A body the decoder does not know is left as it is.
use super::emit::indexed;
use super::inline::{Body, Instruction, decode};

/// A constant up to this many bytes (opcode and value) stands in for the read of a local that
/// holds it: `i64.const 4095` is as long as most `local.get`s. A longer one would grow the module.
const SMALL_CONSTANT: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Local(u32),
    /// The decoded instruction that is the constant.
    Constant(usize),
}

pub(super) fn run(body: &mut Body) {
    let before = body.code.clone();
    forward(body);
    returns(body);
    sweep(body);
    // No trap gains a reason it did not have: were one to appear, the body stays as it was.
    if reasons(&body.code) != reasons(&before) { body.code = before; }
}

/// How many times `i32.const k; drop; unreachable` (k in one byte) is written: what the host
/// reads as a trap's reason.
fn reasons(code: &[u8]) -> Option<usize> {
    let instructions = decode(code)?;
    Some(instructions.windows(3).filter(|w| w[0].op == 0x41 && w[0].end - w[0].at == 2 && w[1].op == 0x1a && w[2].op == 0x00).count())
}

fn forward(body: &mut Body) {
    let Some(instructions) = decode(&body.code) else { return };
    let (params, count) = (body.params.len(), body.params.len() + body.locals.len());
    let mut writes = vec![0u32; count];
    for instruction in &instructions {
        if matches!(instruction.op, 0x20..=0x22) {
            // An index past the declared locals (invalid Wasm, never emitted): the body is left alone.
            if instruction.index as usize >= count { return; }
            if instruction.op != 0x20 { writes[instruction.index as usize] += 1; }
        }
    }
    // On the straight line: what each local is a copy of, if anything. `copies[a]` lists the
    // locals that were, at some time, copies of `a`: a write to `a` looks only there. `known`
    // lists the locals with an entry.
    let mut source: Vec<Option<Source>> = vec![None; count];
    let mut copies: Vec<Vec<u32>> = vec![Vec::new(); count];
    let mut known: Vec<u32> = Vec::new();
    // For a local written once: the block its write is in, once the write has been passed, and
    // what it copied, if that cannot change while the block is open.
    let mut written_in: Vec<Option<u32>> = vec![None; count];
    let mut always: Vec<Option<Source>> = vec![None; count];
    // The blocks open here, innermost last, each with a number of its own (an `else` arm is a
    // block apart from its `then` arm); `open[n]` says whether block `n` is one of them.
    let (mut path, mut open) = (vec![0u32], vec![true]);
    let mut out = Vec::with_capacity(body.code.len());
    // The values on top of the stack that the instructions just written pushed by reading a
    // local or a small constant and doing nothing else; `None` for one whose local has been
    // written since.
    let mut top: Vec<Option<Source>> = Vec::new();
    // The `end` of the trap-only `if` being read, if one is.
    let mut trap_end: Option<usize> = None;
    for (at, instruction) in instructions.iter().enumerate() {
        match instruction.op {
            0x20 => {
                let read = instruction.index as usize;
                // `i32.const k; drop; unreachable` is a trap's reason, which the host reads: no
                // constant is put before a `drop`.
                let dropped = instructions.get(at + 1).is_some_and(|next| next.op == 0x1a);
                let held = source[read].or_else(|| always[read].filter(|_| written_in[read].is_some_and(|block| open[block as usize])));
                let leaves = match held {
                    Some(Source::Local(from)) => { indexed(&mut out, 0x20, from); Source::Local(from) }
                    Some(Source::Constant(constant)) if !dropped => {
                        let constant_at = &instructions[constant];
                        out.extend(&body.code[constant_at.at..constant_at.end]);
                        Source::Constant(constant)
                    }
                    _ => { out.extend(&body.code[instruction.at..instruction.end]); Source::Local(instruction.index) }
                };
                top.push(Some(leaves));
            }
            0x41 | 0x42 if instruction.end - instruction.at <= SMALL_CONSTANT => {
                out.extend(&body.code[instruction.at..instruction.end]);
                top.push(Some(Source::Constant(at)));
            }
            0x21 | 0x22 => {
                let written = instruction.index;
                let from = if instruction.op == 0x21 { top.pop().flatten() } else { top.last().copied().flatten() };
                // The local changes: it is no copy of anything, nothing is a copy of it, and a
                // value read from it that is still on the stack is not what it holds now.
                source[written as usize] = None;
                for copy in std::mem::take(&mut copies[written as usize]) {
                    if source[copy as usize] == Some(Source::Local(written)) { source[copy as usize] = None; }
                }
                for value in top.iter_mut() { if *value == Some(Source::Local(written)) { *value = None; } }
                if let Some(from) = from && from != Source::Local(written) {
                    source[written as usize] = Some(from);
                    if let Source::Local(from) = from { copies[from as usize].push(written); }
                    known.push(written);
                    // Its one write, of a value that stays what it is while this block is open.
                    let lasting = match from {
                        Source::Constant(_) => true,
                        Source::Local(a) => {
                            let a = a as usize;
                            if a < params { writes[a] == 0 } else { writes[a] == 1 && written_in[a].is_some_and(|block| open[block as usize]) }
                        }
                    };
                    if written as usize >= params && writes[written as usize] == 1 && lasting { always[written as usize] = Some(from); }
                }
                if written as usize >= params && writes[written as usize] == 1 { written_in[written as usize] = path.last().copied(); }
                out.extend(&body.code[instruction.at..instruction.end]);
            }
            op => {
                // An instruction that only takes values and leaves one: what is under them is
                // still what it was. Anything else, nothing is said of the stack after it.
                match effect(op) {
                    Some((takes, leaves)) => {
                        top.truncate(top.len().saturating_sub(takes));
                        if leaves { top.push(None); }
                    }
                    None => top.clear(),
                }
                // An `if` that only traps (`if; i32.const k; drop; unreachable; end`: every check
                // of an overflow, an index or a shift) writes no local and never reaches its
                // `end`, which control therefore reaches only from the `if` itself: what was
                // known there is known after it.
                if op == 0x04 {
                    let body = instructions[at + 1..].iter().take_while(|i| matches!(i.op, 0x41 | 0x1a | 0x00)).count();
                    if body > 0 && instructions[at + body].op == 0x00 && instructions.get(at + body + 1).is_some_and(|i| i.op == 0x0b) { trap_end = Some(at + body + 1); }
                }
                if matches!(op, 0x03 | 0x05 | 0x0b) && trap_end != Some(at) { for local in known.drain(..) { source[local as usize] = None; } }
                match op {
                    0x02..=0x04 => { path.push(open.len() as u32); open.push(true); }
                    0x05 => {
                        if let Some(closed) = path.pop() { open[closed as usize] = false; }
                        path.push(open.len() as u32); open.push(true);
                    }
                    // The function's own `end` closes block 0; nothing follows it.
                    0x0b => if let Some(closed) = path.pop() { open[closed as usize] = false; },
                    _ => {}
                }
                out.extend(&body.code[instruction.at..instruction.end]);
            }
        }
    }
    body.code = out;
}

/// For an instruction that does nothing but take operands from the stack and leave at most one
/// value (a load, a store, arithmetic, a comparison, a conversion, `drop`, `select`): how many
/// it takes and whether it leaves one. A load or store may trap, and a division; none writes a
/// local or moves control.
fn effect(op: u8) -> Option<(usize, bool)> {
    Some(match op {
        0x1a => (1, false),
        0x1b => (3, true),
        0x28..=0x35 => (1, true),
        0x36..=0x3e => (2, false),
        0x41 | 0x42 | 0x44 => (0, true),
        0x45 | 0x50 | 0x67..=0x69 | 0x79..=0x7b | 0x99..=0x9f | 0xa7..=0xbf => (1, true),
        0x46..=0x4f | 0x51..=0x5a | 0x61..=0x66 | 0x6a..=0x78 | 0x7c..=0x8a | 0xa0..=0xa6 => (2, true),
        _ => return None,
    })
}

/// Results that leave a block through locals only to be returned: `local.set b; local.set a` and
/// then a branch to (or the end of) a block whose `end` is followed by exactly `local.get a;
/// local.get b; return`, for as many locals as the function has results, is `return` there: the
/// values are on the stack, in order, and nothing runs between the sets and the return but the
/// branch. (A callee written in hands several results over this way, and a wrapper returns them.)
fn returns(body: &mut Body) {
    let results = body.results.len();
    if results == 0 { return; }
    let Some(instructions) = decode(&body.code) else { return };
    // For each `end` of a block or an if: the locals read after it and then returned, in order.
    let handed = |end: usize| -> Option<Vec<u32>> {
        let after = instructions.get(end + 1..end + 2 + results)?;
        let locals: Vec<u32> = after[..results].iter().map(|i| i.index).collect();
        // Each local once: set twice, the second read would not be the value under the first.
        let distinct = locals.iter().enumerate().all(|(k, local)| !locals[..k].contains(local));
        (after[..results].iter().all(|i| i.op == 0x20) && after[results].op == 0x0f && distinct).then_some(locals)
    };
    // The sets that end at `before` (not included) write exactly those locals, last first.
    let set_before = |before: usize, locals: &[u32]| before >= results
        && instructions[before - results..before].iter().rev().zip(locals).all(|(set, local)| set.op == 0x21 && set.index == *local);
    // Each open block: (the instruction that opened it, its `end`), found by a first walk.
    let mut ends = vec![0usize; instructions.len()];
    let mut open = Vec::new();
    for (at, instruction) in instructions.iter().enumerate() {
        match instruction.op {
            0x02..=0x04 => open.push(at),
            0x0b => if let Some(opened) = open.pop() { ends[opened] = at; },
            _ => {}
        }
    }
    // What to write at each instruction: itself, nothing, or `return`.
    #[derive(Clone, Copy, PartialEq)]
    enum Write { Same, Nothing, Return }
    let mut write = vec![Write::Same; instructions.len()];
    let mut path: Vec<usize> = Vec::new();
    let mut any = false;
    for (at, instruction) in instructions.iter().enumerate() {
        // The block left here, by a branch or by running into its `end` (or its `else`, which
        // goes to the same `end`): never a loop, whose branch goes back to its top.
        let target = match instruction.op {
            0x0c => path.len().checked_sub(1 + instruction.index as usize).map(|depth| path[depth]).filter(|opened| instructions[*opened].op != 0x03),
            0x05 | 0x0b => path.last().copied().filter(|opened| instructions[*opened].op != 0x03),
            _ => None,
        };
        if let Some(opened) = target && let Some(locals) = handed(ends[opened]) && set_before(at, &locals) {
            write[at - results] = Write::Return;
            for set in at - results + 1..at { write[set] = Write::Nothing; }
            // The branch goes too; an `end` or `else` stays, now after a `return`.
            if instruction.op == 0x0c { write[at] = Write::Nothing; }
            any = true;
        }
        match instruction.op {
            0x02..=0x04 => path.push(at),
            0x0b => { path.pop(); }
            _ => {}
        }
    }
    if !any { return; }
    let mut out = Vec::with_capacity(body.code.len());
    for (instruction, write) in instructions.iter().zip(write) {
        match write {
            Write::Same => out.extend(&body.code[instruction.at..instruction.end]),
            Write::Return => out.push(0x0f),
            Write::Nothing => {}
        }
    }
    body.code = out;
}

/// The opposite of a comparison, where the profile has one (tools/wasmtime-host admits
/// `i32.eq`, `ne`, `lt_u` and `gt_u` only, and every `i64` comparison).
fn opposite(op: u8) -> Option<u8> {
    Some(match op {
        0x46 => 0x47, 0x47 => 0x46,
        0x51 => 0x52, 0x52 => 0x51,
        0x53 => 0x59, 0x59 => 0x53, 0x55 => 0x57, 0x57 => 0x55,
        0x54 => 0x5a, 0x5a => 0x54, 0x56 => 0x58, 0x58 => 0x56,
        _ => return None,
    })
}

/// After `forward`: a write to a local nothing reads goes, with the plain read or constant that
/// fed it when that is the instruction just before (several reads and then their sets go pair
/// by pair); `local.set x; local.get x` goes when that is the only read of `x`; and the two
/// rewrites of a test. Whatever is left of such writes `inline::tee` turns into a `drop`.
fn sweep(body: &mut Body) {
    let Some(instructions) = decode(&body.code) else { return };
    let mut reads = vec![0u32; body.params.len() + body.locals.len()];
    for instruction in &instructions {
        if instruction.op == 0x20 { let Some(count) = reads.get_mut(instruction.index as usize) else { return }; *count += 1; }
        else if matches!(instruction.op, 0x21 | 0x22) && instruction.index as usize >= reads.len() { return; }
    }
    let code = &body.code;
    let mut out: Vec<u8> = Vec::with_capacity(code.len());
    // Where in `out` each of the plain reads and constants that end it begins.
    let mut pushes: Vec<usize> = Vec::new();
    // The comparison that ends `out`, if one does.
    let mut compared: Option<u8> = None;
    let mut skip = false;
    let is = |at: usize, op: u8| instructions.get(at).is_some_and(|i: &Instruction| i.op == op);
    // A plain read or constant set into a local that is written again before anything reads it
    // or any branch, block boundary or trap is met (within a few instructions): neither the
    // push nor the set is needed. (A checked `x + 1` sets the 1 into a scratch local the sum
    // then overwrites.)
    let mut dead = vec![false; instructions.len()];
    // (Looked for with the `tee`s of never-read locals, which go below, taken out from between.)
    let kept: Vec<usize> = (0..instructions.len()).filter(|at| !(instructions[*at].op == 0x22 && reads[instructions[*at].index as usize] == 0 && !is(at + 1, 0x00))).collect();
    for pair in kept.windows(2) {
        let (at, next) = (pair[0], pair[1]);
        let (first, second) = (&instructions[at], &instructions[next]);
        if dead[at] || is(next + 1, 0x00) || is(at + 1, 0x00) { continue; }
        // The same for `local.set x; local.get x`: the value stays on the stack, and the local
        // need not hold it.
        let local = if matches!(first.op, 0x20 | 0x41 | 0x42 | 0x44) && second.op == 0x21 { second.index }
            else if first.op == 0x21 && second.op == 0x20 && second.index == first.index { first.index }
            else { continue };
        for later in instructions[next + 1..].iter().take(12) {
            match later.op {
                0x21 | 0x22 if later.index == local => { dead[at] = true; dead[next] = true; break; }
                0x20 if later.index == local => break,
                0x00 | 0x02..=0x05 | 0x0b..=0x0f => break,
                _ => {}
            }
        }
    }
    for (at, instruction) in instructions.iter().enumerate() {
        if skip { skip = false; continue; }
        if dead[at] { continue; }
        let was_compared = compared.take();
        let bytes = &code[instruction.at..instruction.end];
        // Nothing next to an `unreachable` changes: what is before it may be its reason.
        if is(at + 1, 0x00) { pushes.clear(); out.extend(bytes); continue; }
        match instruction.op {
            0x20 | 0x41 | 0x42 | 0x44 => { pushes.push(out.len()); out.extend(bytes); }
            0x21 if reads[instruction.index as usize] == 0 && !pushes.is_empty() => out.truncate(pushes.pop().expect("a push")),
            0x21 if reads[instruction.index as usize] == 1 && instructions.get(at + 1).is_some_and(|next| next.op == 0x20 && next.index == instruction.index) && !dead[at + 1] && !is(at + 2, 0x00) => skip = true,
            0x22 if reads[instruction.index as usize] == 0 => {}
            // `i32.const 0; i32.eq` and `i64.const 0; i64.eq`: `eqz`.
            0x46 | 0x51 if pushes.last().is_some_and(|start| out[*start..] == [if instruction.op == 0x46 { 0x41 } else { 0x42 }, 0]) => {
                out.truncate(pushes.pop().expect("a push")); pushes.clear();
                out.push(if instruction.op == 0x46 { 0x45 } else { 0x50 });
            }
            // `i32.const 0; i32.add`, `i32.const 1; i32.mul` and the `i64` ones: the other operand, as it is.
            0x6a | 0x6c | 0x7c | 0x7e if pushes.last().is_some_and(|start| out[*start..] == [if instruction.op < 0x70 { 0x41 } else { 0x42 }, u8::from(matches!(instruction.op, 0x6c | 0x7e))]) => {
                out.truncate(pushes.pop().expect("a push"));
            }
            // A comparison and then `i32.eqz`: the opposite comparison.
            0x45 if was_compared.and_then(opposite).is_some() => {
                pushes.clear();
                let last = out.len() - 1;
                out[last] = was_compared.and_then(opposite).expect("an opposite");
                compared = Some(out[last]);
            }
            op => {
                pushes.clear();
                out.extend(bytes);
                if opposite(op).is_some() { compared = Some(op); }
            }
        }
    }
    body.code = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn forwarded(params: &[u8], locals: &[u8], code: &[u8]) -> Vec<u8> {
        let mut body = Body { params: params.to_vec(), results: vec![], locals: locals.to_vec(), code: code.to_vec() };
        run(&mut body);
        body.code
    }
    #[test]
    fn a_copy_is_read_from_its_source_until_either_is_written() {
        // l1 = p0; l1 + l1; p0 = 5; l1 (still read: p0 changed)
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x21, 1, 0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x42, 5, 0x21, 0, 0x20, 1, 0x1a, 0x0b]),
            [0x20, 0, 0x21, 1, 0x20, 0, 0x20, 0, 0x7c, 0x1a, 0x42, 5, 0x21, 0, 0x20, 1, 0x1a, 0x0b]);
        // l1 = p0; l1 = l1 + 1 (through a tee); l1 is no copy any more. (p0 is written at the end,
        // so nothing lasts past the straight line.)
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x21, 1, 0x20, 1, 0x42, 1, 0x7c, 0x22, 1, 0x20, 1, 0x7c, 0x21, 0, 0x0b]),
            [0x20, 0, 0x42, 1, 0x7c, 0x22, 1, 0x20, 1, 0x7c, 0x21, 0, 0x0b]);
        // A chain: l1 = p0; l2 = l1; l2 reads p0, and neither copy is written any more.
        assert_eq!(forwarded(&[0x7e], &[0x7e, 0x7e], &[0x20, 0, 0x21, 1, 0x20, 1, 0x21, 2, 0x20, 2, 0x20, 2, 0x7c, 0x1a, 0x0b]),
            [0x20, 0, 0x20, 0, 0x7c, 0x1a, 0x0b]);
    }
    #[test]
    fn arguments_read_and_then_set_are_copies_in_stack_order() {
        // p0, p1 read, then set into l3 and l2 (a written-in call's arguments); l2 + l3.
        assert_eq!(forwarded(&[0x7e, 0x7e], &[0x7e, 0x7e], &[0x20, 0, 0x20, 1, 0x21, 3, 0x21, 2, 0x20, 2, 0x20, 3, 0x7c, 0x1a, 0x0b]),
            [0x20, 0, 0x20, 1, 0x7c, 0x1a, 0x0b]);
        // p0 read, then written while its old value is on the stack: l1 gets the old value, and
        // is not read as p0, which is 5 now. (`local.set 1; local.get 1`, its only read, goes: the
        // value stays where it was.)
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x42, 5, 0x21, 0, 0x21, 1, 0x20, 1, 0x20, 0, 0x7c, 0x1a, 0x0b]),
            [0x20, 0, 0x42, 5, 0x21, 0, 0x42, 5, 0x7c, 0x1a, 0x0b]);
        // A tee copies, and what it leaves can be set again: l1 and l2 are both p0.
        assert_eq!(forwarded(&[0x7e], &[0x7e, 0x7e], &[0x20, 0, 0x22, 1, 0x21, 2, 0x20, 1, 0x20, 2, 0x7c, 0x1a, 0x0b]),
            [0x20, 0, 0x20, 0, 0x7c, 0x1a, 0x0b]);
    }
    #[test]
    fn a_small_constant_is_written_where_its_local_is_read() {
        assert_eq!(forwarded(&[], &[0x7e], &[0x42, 7, 0x21, 0, 0x20, 0, 0x20, 0, 0x7c, 0x1a, 0x0b]),
            [0x42, 7, 0x42, 7, 0x7c, 0x1a, 0x0b]);
        // A long constant stays in its local.
        let long = [0x42, 0xff, 0xff, 0xff, 0x0f, 0x21, 0, 0x20, 0, 0x20, 0, 0x7c, 0x1a, 0x0b];
        assert_eq!(forwarded(&[], &[0x7e], &long), long);
        // No trap gains a reason: `i32.const 6; drop; unreachable` would be read as one.
        let reason = [0x41, 6, 0x21, 0, 0x20, 0, 0x1a, 0x00, 0x0b];
        assert_eq!(forwarded(&[], &[0x7f], &reason), reason);
        // One that is there stays, and what is before it is left alone.
        let marked = [0x41, 1, 0x21, 0, 0x41, 6, 0x1a, 0x00, 0x0b];
        assert_eq!(forwarded(&[], &[0x7f], &marked), [0x41, 6, 0x1a, 0x00, 0x0b]);
    }
    #[test]
    fn what_is_known_on_a_line_enters_a_block_and_an_if_and_ends_at_a_join() {
        // p0 is written at the end of each, so only the straight line knows l1 is p0.
        // l1 = p0; block { l1 } ; l1  -- inside, p0; after the block's end, l1.
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x21, 1, 0x02, 0x40, 0x20, 1, 0x1a, 0x0b, 0x20, 1, 0x21, 0, 0x0b]),
            [0x20, 0, 0x21, 1, 0x02, 0x40, 0x20, 0, 0x1a, 0x0b, 0x20, 1, 0x21, 0, 0x0b]);
        // l1 = p0; if { l1 } else { l1 }: the first arm reads p0, the second l1.
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x21, 1, 0x41, 1, 0x04, 0x40, 0x20, 1, 0x1a, 0x05, 0x20, 1, 0x1a, 0x0b, 0x42, 1, 0x21, 0, 0x0b]),
            [0x20, 0, 0x21, 1, 0x41, 1, 0x04, 0x40, 0x20, 0, 0x1a, 0x05, 0x20, 1, 0x1a, 0x0b, 0x42, 1, 0x21, 0, 0x0b]);
        // l1 = p0; loop { l1; p0 = p0 + 1; br_if 0 }: the loop's top is reached from its own end.
        let looped = [0x20, 0, 0x21, 1, 0x03, 0x40, 0x20, 1, 0x1a, 0x20, 0, 0x42, 1, 0x7c, 0x21, 0, 0x41, 1, 0x0d, 0, 0x0b, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &looped), looped);
    }
    #[test]
    fn a_local_written_once_is_its_source_to_the_end_of_its_block() {
        // p0 is never written. l1 = p0; loop { l1; br_if 0 }; l1: every read is p0.
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x20, 0, 0x21, 1, 0x03, 0x40, 0x20, 1, 0x1a, 0x41, 1, 0x0d, 0, 0x0b, 0x20, 1, 0x1a, 0x0b]),
            [0x03, 0x40, 0x20, 0, 0x1a, 0x41, 1, 0x0d, 0, 0x0b, 0x20, 0, 0x1a, 0x0b]);
        // Written in an `if`, read after it: the write may not have run. Left alone.
        let skipped = [0x41, 1, 0x04, 0x40, 0x20, 0, 0x21, 1, 0x0b, 0x20, 1, 0x1a, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &skipped), skipped);
        // Read before its write in a loop (the value of the turn before): left alone; after it, p0.
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x03, 0x40, 0x20, 1, 0x1a, 0x20, 0, 0x21, 1, 0x20, 1, 0x1a, 0x41, 1, 0x0d, 0, 0x0b, 0x0b]),
            [0x03, 0x40, 0x20, 1, 0x1a, 0x20, 0, 0x21, 1, 0x20, 0, 0x1a, 0x41, 1, 0x0d, 0, 0x0b, 0x0b]);
        // Written twice: only the straight line. l1 = p0; loop { l1; l1 = 3; br_if 0 }.
        let twice = [0x20, 0, 0x21, 1, 0x03, 0x40, 0x20, 1, 0x1a, 0x42, 3, 0x21, 1, 0x41, 1, 0x0d, 0, 0x0b, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &twice), twice);
        // A copy of a once-written local whose own write is in a block that has closed does not last:
        // block { l1 = p0 + 1 }; l2 = l1; loop { l2; br_if 0 } keeps l2's reads... as l1's, on the line only.
        let closed = [0x02, 0x40, 0x20, 0, 0x42, 1, 0x7c, 0x21, 1, 0x0b, 0x20, 1, 0x21, 2, 0x03, 0x40, 0x20, 2, 0x1a, 0x41, 1, 0x0d, 0, 0x0b, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e, 0x7e], &closed), closed);
    }
    #[test]
    fn a_test_against_zero_and_an_inverted_comparison_are_one_instruction() {
        // p0 == 0 (i64) -> i64.eqz; p1 == 0 (i32) -> i32.eqz.
        assert_eq!(forwarded(&[0x7e, 0x7f], &[], &[0x20, 0, 0x42, 0, 0x51, 0x1a, 0x20, 1, 0x41, 0, 0x46, 0x1a, 0x0b]),
            [0x20, 0, 0x50, 0x1a, 0x20, 1, 0x45, 0x1a, 0x0b]);
        // !(p0 < p0) -> p0 >= p0; an inverted `i32.lt_u` has no opposite in the profile and stays.
        assert_eq!(forwarded(&[0x7e, 0x7f], &[], &[0x20, 0, 0x20, 0, 0x54, 0x45, 0x1a, 0x20, 1, 0x20, 1, 0x49, 0x45, 0x1a, 0x0b]),
            [0x20, 0, 0x20, 0, 0x5a, 0x1a, 0x20, 1, 0x20, 1, 0x49, 0x45, 0x1a, 0x0b]);
        // An f64 comparison is not inverted (a NaN compares false both ways).
        let float = [0x20, 0, 0x20, 0, 0x63, 0x45, 0x1a, 0x0b];
        assert_eq!(forwarded(&[0x7c], &[], &float), float);
    }
    #[test]
    fn results_set_only_to_be_returned_are_returned() {
        let returned = |code: &[u8]| {
            let mut body = Body { params: vec![0x7e], results: vec![0x7f, 0x7e], locals: vec![0x7f, 0x7e], code: code.to_vec() };
            run(&mut body);
            body.code
        };
        // block { if p0 == 0 { (0, 7) -> l1, l2; br 1 }; (1, p0) -> l1, l2 }; l1; l2; return
        assert_eq!(returned(&[0x02, 0x40, 0x20, 0, 0x50, 0x04, 0x40, 0x41, 0, 0x42, 7, 0x21, 2, 0x21, 1, 0x0c, 1, 0x0b,
                0x41, 1, 0x20, 0, 0x21, 2, 0x21, 1, 0x0b, 0x20, 1, 0x20, 2, 0x0f, 0x00, 0x0b]),
            [0x02, 0x40, 0x20, 0, 0x50, 0x04, 0x40, 0x41, 0, 0x42, 7, 0x0f, 0x0b,
                0x41, 1, 0x20, 0, 0x0f, 0x0b, 0x20, 1, 0x20, 2, 0x0f, 0x00, 0x0b]);
        // The sets in the other order hand the values over crossed: left alone.
        let crossed = [0x02, 0x40, 0x41, 1, 0x20, 0, 0x45, 0x04, 0x40, 0x00, 0x0b, 0x42, 9, 0x21, 1, 0x21, 2, 0x0b, 0x20, 1, 0x20, 2, 0x0f, 0x00, 0x0b];
        assert_eq!(returned(&crossed).len(), crossed.len());
        // A branch back to a loop's top is no way out of it.
        let looped = [0x03, 0x40, 0x41, 1, 0x20, 0, 0x21, 2, 0x21, 1, 0x20, 0, 0x50, 0x0d, 0, 0x0b, 0x20, 1, 0x20, 2, 0x0f, 0x00, 0x0b];
        assert_eq!(returned(&looped), looped);
        // Nor is an unconditional one: loop { if p0 != 0 { p0 = 0; (3, 4) -> l1, l2; br 1 } }; l1; l2; return
        let again = [0x03, 0x40, 0x20, 0, 0x50, 0x45, 0x04, 0x40, 0x42, 0, 0x21, 0, 0x41, 3, 0x42, 4, 0x21, 2, 0x21, 1, 0x0c, 1, 0x0b, 0x0b, 0x20, 1, 0x20, 2, 0x0f, 0x00, 0x0b];
        let kept = returned(&again);
        assert!(kept.windows(2).any(|w| w == [0x0c, 1]) && !kept[..kept.len() - 4].contains(&0x0f), "{kept:?}");
    }
    #[test]
    fn a_set_overwritten_before_any_read_goes_with_its_constant() {
        // l1 = 1; l1 = p0 + 1; l1  -- the first set is never read.
        assert_eq!(forwarded(&[0x7e], &[0x7e], &[0x42, 1, 0x21, 1, 0x20, 0, 0x42, 1, 0x7c, 0x21, 1, 0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x0b]),
            [0x20, 0, 0x42, 1, 0x7c, 0x21, 1, 0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x0b]);
        // Read in between (twice, so the first set is needed), or a branch in between: kept.
        let read = [0x42, 0xff, 0xff, 0x03, 0x21, 1, 0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x20, 0, 0x42, 1, 0x7c, 0x21, 1, 0x20, 1, 0x1a, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &read), read);
        let branched = [0x02, 0x40, 0x42, 0xff, 0xff, 0x03, 0x21, 1, 0x41, 1, 0x0d, 0, 0x20, 0, 0x21, 1, 0x0b, 0x20, 1, 0x20, 1, 0x7c, 0x1a, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &branched), branched);
    }
    #[test]
    fn a_copy_is_known_under_what_an_operation_left() {
        // p0; (p1 loaded) set l3; tee l2; l3 + ; l2 again -- a checked add's scratch: l2 is p0.
        assert_eq!(forwarded(&[0x7e, 0x7f], &[0x7e, 0x7e], &[0x20, 0, 0x20, 1, 0x31, 0, 0, 0x21, 3, 0x22, 2, 0x20, 3, 0x7c, 0x22, 3, 0x20, 2, 0x54, 0x1a, 0x20, 3, 0x1a, 0x0b]),
            [0x20, 0, 0x20, 1, 0x31, 0, 0, 0x7c, 0x22, 3, 0x20, 0, 0x54, 0x1a, 0x20, 3, 0x1a, 0x0b]);
        // Adding zero and multiplying by one leave the other operand.
        assert_eq!(forwarded(&[0x7e, 0x7f], &[], &[0x20, 1, 0x41, 0, 0x6a, 0x41, 1, 0x6c, 0x1a, 0x20, 0, 0x42, 1, 0x7e, 0x42, 0, 0x7c, 0x1a, 0x0b]),
            [0x20, 1, 0x1a, 0x20, 0, 0x1a, 0x0b]);
    }
    #[test]
    fn what_is_known_passes_an_if_that_only_traps() {
        // p0 is written at the end. l1 = p0; if p1 { trap 6 }; l1 -- still p0. With a set in the `if`, not.
        assert_eq!(forwarded(&[0x7e, 0x7f], &[0x7e], &[0x20, 0, 0x21, 2, 0x20, 1, 0x04, 0x40, 0x41, 6, 0x1a, 0x00, 0x0b, 0x20, 2, 0x20, 2, 0x7c, 0x21, 0, 0x0b]),
            [0x20, 1, 0x04, 0x40, 0x41, 6, 0x1a, 0x00, 0x0b, 0x20, 0, 0x20, 0, 0x7c, 0x21, 0, 0x0b]);
        let written = [0x20, 0, 0x21, 2, 0x20, 1, 0x04, 0x40, 0x42, 6, 0x21, 0, 0x0b, 0x20, 2, 0x20, 2, 0x7c, 0x21, 0, 0x0b];
        assert_eq!(forwarded(&[0x7e, 0x7f], &[0x7e], &written), written);
        // A set and then a trap in the `if`: what the set made known is not known after it.
        let set_then_trap = [0x20, 0, 0x21, 2, 0x20, 1, 0x04, 0x40, 0x42, 6, 0x21, 2, 0x00, 0x0b, 0x20, 2, 0x20, 2, 0x7c, 0x21, 0, 0x0b];
        assert_eq!(forwarded(&[0x7e, 0x7f], &[0x7e], &set_then_trap), set_then_trap);
    }
    /// A body of `i64` locals run on its arguments: the returned values, or `None` for a trap.
    /// Blocks have no results; every value is kept as a `u64` (an `i32` is 0 or 1 here).
    fn interpret(params: usize, locals: usize, code: &[u8], arguments: &[u64]) -> Option<Vec<u64>> {
        let instructions = decode(code).expect("a body the decoder knows");
        let value = |i: &Instruction| { // the signed LEB of a constant
            let (mut value, mut shift, mut at) = (0u64, 0u32, i.at + 1);
            loop {
                let byte = code[at]; at += 1;
                if shift < 64 { value |= u64::from(byte & 127) << shift; }
                shift += 7;
                if byte & 128 == 0 { if shift < 64 && byte & 64 != 0 { value |= u64::MAX << shift; } return value; }
            }
        };
        let (mut ends, mut elses, mut open) = (vec![0usize; instructions.len()], vec![0usize; instructions.len()], Vec::new());
        for (at, i) in instructions.iter().enumerate() {
            match i.op { 0x02..=0x04 => open.push(at), 0x05 => elses[*open.last().unwrap()] = at, 0x0b => if let Some(o) = open.pop() { ends[o] = at; }, _ => {} }
        }
        let mut local = arguments.to_vec(); local.resize(params + locals, 0);
        let (mut stack, mut labels, mut at, mut steps): (Vec<u64>, Vec<(usize, bool)>, usize, u32) = (Vec::new(), Vec::new(), 0, 0);
        loop {
            steps += 1; assert!(steps < 2_000_000, "a body that does not end");
            let i = &instructions[at]; at += 1;
            macro_rules! two { ($f:expr) => {{ let b = stack.pop().unwrap(); let a = stack.pop().unwrap(); let f: fn(u64, u64) -> u64 = $f; stack.push(f(a, b)); }}; }
            match i.op {
                0x00 => return None,
                0x02 => labels.push((ends[at - 1], false)),
                0x03 => labels.push((at - 1, true)),
                0x04 => {
                    let opened = at - 1;
                    if stack.pop().unwrap() != 0 { labels.push((ends[opened], false)); }
                    else if elses[opened] != 0 { at = elses[opened] + 1; labels.push((ends[opened], false)); }
                    else { at = ends[opened] + 1; }
                }
                0x05 => { let (end, _) = labels.pop().unwrap(); at = end + 1; }
                0x0b => if labels.pop().is_none() { return Some(stack); },
                0x0c | 0x0d => {
                    if i.op == 0x0d && stack.pop().unwrap() == 0 { continue; }
                    labels.truncate(labels.len() - i.index as usize);
                    let (target, looping) = *labels.last().unwrap();
                    at = target + 1;
                    if !looping { labels.pop(); }
                }
                0x0f => return Some(stack),
                0x1a => { stack.pop().unwrap(); }
                0x20 => stack.push(local[i.index as usize]),
                0x21 => local[i.index as usize] = stack.pop().unwrap(),
                0x22 => local[i.index as usize] = *stack.last().unwrap(),
                0x41 => stack.push(value(i) & 0xffff_ffff),
                0x42 => stack.push(value(i)),
                0x45 | 0x50 => { let a = stack.pop().unwrap(); stack.push(u64::from(a == 0)); }
                0x46 | 0x51 => two!(|a, b| u64::from(a == b)),
                0x47 | 0x52 => two!(|a, b| u64::from(a != b)),
                0x54 => two!(|a, b| u64::from(a < b)),
                0x56 => two!(|a, b| u64::from(a > b)),
                0x58 => two!(|a, b| u64::from(a <= b)),
                0x5a => two!(|a, b| u64::from(a >= b)),
                0x6a => two!(|a, b| a.wrapping_add(b) & 0xffff_ffff),
                0x6c => two!(|a, b| a.wrapping_mul(b) & 0xffff_ffff),
                0x7c => two!(|a, b| a.wrapping_add(b)),
                0x7d => two!(|a, b| a.wrapping_sub(b)),
                0x7e => two!(|a, b| a.wrapping_mul(b)),
                0x85 => two!(|a, b| a ^ b),
                0xa7 => { let a = stack.pop().unwrap(); stack.push(a & 0xffff_ffff); }
                0xad => {}
                other => panic!("an instruction the test does not write: {other:#x}"),
            }
        }
    }
    /// Random bodies in the shapes the emitter writes (copies, arguments read and then set, a
    /// set read back, constants small and long, checked arithmetic with a trap, `if`, `else`,
    /// blocks left by `br_if`, counted loops, results handed over in locals and returned):
    /// each runs the same before and after the pass, on several arguments: the same two
    /// results or a trap in both. 4,000 bodies; nine in ten are changed by the pass.
    #[test]
    fn random_bodies_run_the_same_after_the_pass() {
        const PARAMS: usize = 2;
        const LOCALS: usize = 9; // 2..9 free, 9 and 10 the loop counters (written by their loops only)
        let mut state = 0x853c_49e6_748f_ea9bu64;
        let mut random = move |below: u64| { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state % below };
        fn expr(out: &mut Vec<u8>, random: &mut impl FnMut(u64) -> u64, depth: u32) {
            match random(if depth == 0 { 3 } else { 8 }) {
                0 | 1 => out.extend([0x20, random(9) as u8]),
                2 => match random(5) {
                    0 => out.extend([0x42, 0]), 1 => out.extend([0x42, 1]), 2 => out.extend([0x42, 0x7f]),
                    3 => out.extend([0x42, 0xff, 0xff, 0xff, 0xff, 0x0f]), _ => out.extend([0x42, random(64) as u8]),
                },
                3 => { expr(out, random, depth - 1); out.extend([0x42, random(2) as u8, [0x7c, 0x7e][random(2) as usize]]); }
                4 => { cond(out, random, depth - 1); out.push(0xad); }
                _ => { expr(out, random, depth - 1); expr(out, random, depth - 1); out.push([0x7c, 0x7d, 0x7e, 0x85][random(4) as usize]); }
            }
        }
        fn cond(out: &mut Vec<u8>, random: &mut impl FnMut(u64) -> u64, depth: u32) {
            expr(out, random, depth);
            match random(6) {
                0 => out.push(0x50),
                1 => out.extend([0x42, 0, 0x51]),
                2 => out.extend([0x50, 0x41, 0, 0x46]),
                _ => {
                    expr(out, random, depth);
                    out.push([0x51, 0x52, 0x54, 0x56, 0x58, 0x5a][random(6) as usize]);
                    if random(2) == 0 { out.push(0x45); }
                }
            }
        }
        fn statements(out: &mut Vec<u8>, random: &mut impl FnMut(u64) -> u64, depth: u32, loops: u32) {
            for _ in 0..1 + random(5) {
                let target = 2 + random(7) as u8;
                match random(if depth == 0 { 9 } else { 14 }) {
                    0 | 1 => { out.extend([0x20, random(9) as u8, 0x21, target]); }
                    2 => { out.extend([0x20, random(9) as u8, 0x20, random(9) as u8, 0x21, target, 0x21, 2 + random(7) as u8]); }
                    3 => { expr(out, random, 2); out.extend([0x21, target, 0x20, target, 0x21, 2 + random(7) as u8]); }
                    4 => { expr(out, random, 2); out.extend([0x22, target, 0x21, 2 + random(7) as u8]); }
                    5 => { expr(out, random, 2); out.extend([0x21, target]); }
                    // A checked add as the emitter writes it: a; b; set t; tee s; get t; add; tee t; get s; lt_u; if trap; get t.
                    6 => {
                        let (s, t) = (2 + random(7) as u8, 2 + random(7) as u8);
                        if s == t { continue; }
                        expr(out, random, 1); expr(out, random, 1);
                        out.extend([0x21, t, 0x22, s, 0x20, t, 0x7c, 0x22, t, 0x20, s, 0x54, 0x04, 0x40, 0x41, 1, 0x1a, 0x00, 0x0b, 0x20, t, 0x21, target]);
                    }
                    7 => { cond(out, random, 1); out.extend([0x04, 0x40, 0x41, 6, 0x1a, 0x00, 0x0b]); }
                    8 => { out.extend([0x42, random(3) as u8, 0x21, target]); }
                    9 => { cond(out, random, 1); out.extend([0x04, 0x40]); statements(out, random, depth - 1, loops); if random(2) == 0 { out.push(0x05); statements(out, random, depth - 1, loops); } out.push(0x0b); }
                    10 => { out.extend([0x02, 0x40]); statements(out, random, depth - 1, loops); cond(out, random, 1); out.extend([0x0d, 0]); statements(out, random, depth - 1, loops); out.push(0x0b); }
                    11 if loops < 2 => {
                        // counter = k; block { loop { counter == 0 -> out; counter -= 1; body; again } }
                        let counter = 9 + loops as u8;
                        out.extend([0x42, 1 + random(4) as u8, 0x21, counter, 0x02, 0x40, 0x03, 0x40, 0x20, counter, 0x50, 0x0d, 1, 0x20, counter, 0x42, 1, 0x7d, 0x21, counter]);
                        statements(out, random, depth - 1, loops + 1);
                        out.extend([0x0c, 0, 0x0b, 0x0b]);
                    }
                    12 => { out.extend([0x02, 0x40]); statements(out, random, depth - 1, loops); out.push(0x0b); }
                    _ => { expr(out, random, 2); out.push(0x1a); }
                }
            }
        }
        let (mut changed, mut trapped, mut answered) = (0u32, 0u32, 0u32);
        for _ in 0..4_000 {
            let mut code = Vec::new();
            statements(&mut code, &mut random, 3, 0);
            // Two results, handed over in locals 7 and 8 from inside a block on some, plainly on others.
            if random(3) > 0 {
                code.extend([0x02, 0x40]);
                cond(&mut code, &mut random, 1);
                code.extend([0x04, 0x40]); expr(&mut code, &mut random, 1); expr(&mut code, &mut random, 1); code.extend([0x21, 8, 0x21, 7, 0x0c, 1, 0x0b]);
                expr(&mut code, &mut random, 1); expr(&mut code, &mut random, 1); code.extend([0x21, 8, 0x21, 7, 0x0b]);
            }
            code.extend([0x20, 7, 0x20, 8, 0x0f, 0x00, 0x0b]);
            let mut body = Body { params: vec![0x7e; PARAMS], results: vec![0x7e, 0x7e], locals: vec![0x7e; LOCALS], code: code.clone() };
            run(&mut body);
            if body.code != code { changed += 1; }
            for arguments in [[0u64, 0], [1, 2], [u64::MAX, 1], [u64::MAX - 3, u64::MAX], [5, 0x1_0000_0000]] {
                let before = interpret(PARAMS, LOCALS, &code, &arguments);
                assert_eq!(before, interpret(PARAMS, LOCALS, &body.code, &arguments), "{code:?} became {:?} on {arguments:?}", body.code);
                if before.is_some() { answered += 1; } else { trapped += 1; }
            }
        }
        assert!(changed > 3_600 && trapped > 500 && answered > 5_000, "{changed} changed, {trapped} traps, {answered} answers");
    }
    #[test]
    fn an_unknown_instruction_or_local_leaves_the_body_alone() {
        let unknown = [0x20, 0, 0x21, 1, 0x20, 1, 0xfc, 0x0a, 0, 0, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &unknown), unknown);
        let past = [0x20, 0, 0x21, 1, 0x20, 9, 0x1a, 0x0b];
        assert_eq!(forwarded(&[0x7e], &[0x7e], &past), past);
    }
}
