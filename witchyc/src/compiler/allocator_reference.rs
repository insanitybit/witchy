//! The allocator's helpers as they were at de6ed12b (2026-11-22, before the audited-token memory
//! and the straight-through proved init and take), kept word for word as the other side of
//! `allocator::tests::the_helpers_answer_and_write_as_the_reference_does`. Test code only: no
//! module is built from it.
#![allow(dead_code)]
use super::*;

/// `type_info` written in place: the stride and alignment of type `type_`, or a trap for an
/// unknown one. A call to the helper on every allocation and release cost measurable time.
pub(super) fn type_lookup(c: &mut Code, types: &[(u64, u64)], type_: u32, stride: u32, align: u32) {
    c.i64(0); c.set(stride); c.i64(0); c.set(align);
    for (id, (size, alignment)) in types.iter().enumerate() {
        c.get(type_); c.i64(id as u64 + 1); c.op(EQ);
        c.if_(|c| { c.i64(*size); c.set(stride); c.i64(*alignment); c.set(align); });
    }
    // Every known type has an alignment of at least one: zero is an unknown ID.
    c.get(align); c.op(EQZ); c.trap_if();
}
/// What every buffer operation checks: the token names a live entry of this generation and type,
/// and its count of full cells is within its capacity. Leaves the entry's address in `entry`.
/// The entry's own equations were established by try_alloc and are audited again by try_release;
/// nothing between the two writes an entry, so an access does not re-derive them.
pub(super) fn validated(c: &mut Code, h: Heap, token: u32, type_: u32, entry: u32, decoded: u32) {
    c.get(token); c.op(EQZ); c.trap_if(); // zero rejected before subtraction
    c.get(token); c.i64(1); c.op(SUB); c.set(decoded);
    c.get(decoded); c.i64(h.quantum_bits()); c.op(SHR); c.i64(h.slots - 1); c.op(AND); c.i64(6); c.op(SHL); c.set(entry);
    c.load(entry, LIVE, 0x35, 2); c.i64(1); c.op(NE); c.trap_if();
    // Full width: a generation beyond the stored bits equals no stored one.
    c.load(entry, GENERATION, 0x35, 2); c.get(decoded); c.i64(h.quantum_bits() + h.slot_bits()); c.op(SHR); c.i64(1); c.op(ADD); c.op(NE); c.trap_if();
    // The place the token names is the place the entry holds: emitted code forms the address
    // of a proved cell from the token alone.
    c.load(entry, FIRST, 0x29, 3); c.get(decoded); c.i64((1u64 << h.quantum_bits()) - 1); c.op(AND); c.op(NE); c.trap_if();
    c.load(entry, TYPE, 0x35, 2); c.get(type_); c.op(NE); c.trap_if();
    c.load(entry, COUNT, 0x29, 3); c.load(entry, CAPACITY, 0x29, 3); c.op(GT); c.trap_if();
}
pub(super) fn validate(h: Heap) -> Vec<u8> {
    let mut c = Code::new(2); // params token0,type1; entry2,decoded3
    validated(&mut c, h, 0, 1, 2, 3);
    c.get(2); // ENTRY ADDRESS0 IS VALID SUCCESS, never a null sentinel
    c.finish()
}
/// `remaining` bytes from `address`, eight at a time while eight remain. Both locals are consumed.
pub(super) fn span(c: &mut Code, address: u32, remaining: u32, word: impl Fn(&mut Code), byte: impl Fn(&mut Code)) {
    for (step, body) in [(8u64, &word as &dyn Fn(&mut Code)), (1, &byte)] {
        c.loop_(|c| {
            c.get(remaining); c.i64(step); c.op(LT); c.branch_if(1);
            body(c);
            c.get(address); c.i64(step); c.op(ADD); c.set(address);
            c.get(remaining); c.i64(step); c.op(SUB); c.set(remaining);
            c.branch(0);
        });
    }
}
/// The full audit of a validated entry, before release writes anything: every equation between
/// its fields, its place in the heap, and the occupancy of its whole extent.
/// `type_` is the type parameter, `entry` the validated entry; `l` is nine free i64 locals.
pub(super) fn audit(c: &mut Code, h: Heap, types: &[(u64, u64)], type_: u32, entry: u32, l: u32) {
    let (stride, align, capacity, requested, quanta, base, first, remaining, address) = (l, l + 1, l + 2, l + 3, l + 4, l + 5, l + 6, l + 7, l + 8);
    type_lookup(c, types, type_, stride, align);
    c.load(entry, STRIDE, 0x29, 3); c.get(stride); c.op(NE); c.trap_if();
    c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
    c.get(capacity); c.i64(h.heap_bytes()); c.get(stride); c.op(DIV); c.op(GT); c.trap_if();
    c.load(entry, REQUESTED, 0x35, 2); c.set(requested);
    c.get(capacity); c.get(stride); c.op(MUL); c.get(requested); c.op(NE); c.trap_if();
    c.load(entry, EXTENT, 0x35, 2); c.set(quanta);
    c.get(requested); c.i64(QUANTUM - 1); c.op(ADD); c.i64(5); c.op(SHR);
    c.get(quanta); c.op(NE); c.trap_if();
    c.load(entry, BASE, 0x35, 2); c.set(base);
    c.get(capacity); c.op(EQZ);
    c.if_else(|c| {
        c.get(base); c.i64(0); c.op(NE); c.trap_if();
        c.get(quanta); c.i64(0); c.op(NE); c.trap_if();
        c.load(entry, FIRST, 0x29, 3); c.i64(0); c.op(NE); c.trap_if();
    }, |c| {
        c.get(base); c.i64(HEAP_LOW); c.op(LT); c.trap_if();
        c.get(base); c.i64(h.heap_high()); c.op(GE); c.trap_if();
        c.get(base); c.i64(HEAP_LOW); c.op(SUB); c.i64(QUANTUM - 1); c.op(AND);
        c.i64(0); c.op(NE); c.trap_if();
        c.get(base); c.get(align); c.op(REM); c.i64(0); c.op(NE); c.trap_if();
        c.get(quanta); c.i64(h.quanta()); c.op(GT); c.trap_if();
        c.get(base); c.get(quanta); c.i64(5); c.op(SHL); c.op(ADD);
        c.i64(h.heap_high()); c.op(GT); c.trap_if();
        c.get(base); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
        c.load(entry, FIRST, 0x29, 3); c.get(first); c.op(NE); c.trap_if();
        c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address);
        c.get(quanta); c.set(remaining);
        span(c, address, remaining,
            |c| { c.load(address, 0, 0x29, 0); c.i64(OCCUPIED); c.op(NE); c.trap_if(); },
            |c| { c.load(address, 0, 0x31, 0); c.i64(1); c.op(NE); c.trap_if(); });
    });
}
pub(super) fn allocation_error(c: &mut Code, error: u64) {
    c.i32(1); c.i64(0); c.i64(error); c.op(RETURN);
}
pub(super) fn try_alloc(h: Heap, types: &[(u64, u64)], boxed: bool) -> Vec<u8> {
    // params handle0,type1,capacity2; locals stride3,align4,requested5,quanta6,slot7,entry8,
    // oldGeneration9,first10,start11,cursor12,base13,newGeneration14,address15,remaining16,word17,lowest18.
    let (stride, align, requested, quanta, slot, entry, old, first, start, cursor, base, new, address, remaining, word, lowest, head) =
        (3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19);
    let mut c = Code::new(17);
    c.get(0); c.i32(HANDLE); c.op(0x47); c.trap_if();
    if boxed { c.get(1); c.i64(!BOX_TYPE); c.op(AND); c.set(word); }
    type_lookup(&mut c, types, if boxed {word} else {1}, stride, align);
    c.get(2); c.i64(h.heap_bytes()); c.get(stride); c.op(DIV); c.op(GT);
    c.if_(|c| allocation_error(c, 1)); // before multiplication, widened capacity arithmetic
    c.get(2); c.get(stride); c.op(MUL); c.set(requested);
    c.get(requested); c.i64(QUANTUM - 1); c.op(ADD); c.i64(5); c.op(SHR); c.set(quanta);
    // READ-ONLY plan. A released slot if there is one, else the next never-used slot. Only
    // try_release puts a slot on the list, and only one whose generation can still advance;
    // an entry that is live or terminal here means the metadata is not the allocator's own.
    c.load_at(h.hints() + FREE_SLOT, 0x29, 3); c.set(head);
    c.get(head); c.op(EQZ);
    c.if_else(|c| {
        c.load_at(h.hints() + FRESH_SLOT, 0x29, 3); c.set(slot);
        c.get(slot); c.i64(h.slots); c.op(GE);
        c.if_(|c| allocation_error(c, 3)); // metadata failure precedes fragmentation
    }, |c| {
        c.get(head); c.i64(h.slots); c.op(GT); c.trap_if();
        c.get(head); c.i64(1); c.op(SUB); c.set(slot);
    });
    c.get(slot); c.i64(6); c.op(SHL); c.set(entry);
    c.load(entry, LIVE, 0x35, 2); c.i64(0); c.op(NE); c.trap_if();
    c.load(entry, GENERATION, 0x35, 2); c.set(old);
    c.get(old); c.i64(h.generation_max()); c.op(GE); c.trap_if();
    c.get(quanta); c.op(EQZ);
    c.if_else(|c| { c.i64(0); c.set(base); c.i64(0); c.set(first); }, |c| {
        let ordinary = |c: &mut Code| {
        // First fit. No quantum below the hint is free, so no run starts below it.
        // `start` is where the current free run began; `lowest` the first free quantum seen.
        c.i64(h.quanta()); c.set(first); c.i64(h.quanta()); c.set(lowest);
        c.load_at(h.hints() + FREE_QUANTUM, 0x29, 3); c.set(cursor);
        c.get(cursor); c.set(start);
        c.loop_(|c| {
            c.get(cursor); c.i64(h.quanta()); c.op(GE); c.branch_if(1);
            // Eight quanta at once when the cursor is word-aligned and eight remain.
            c.get(cursor); c.i64(7); c.op(AND); c.op(EQZ);
            c.get(cursor); c.i64(8); c.op(ADD); c.i64(h.quanta()); c.op(LE); c.op(0x71);
            c.if_(|c| {
                c.i64(h.occupancy()); c.get(cursor); c.op(ADD); c.set(address);
                c.load(address, 0, 0x29, 0); c.set(word);
                c.get(word); c.i64(OCCUPIED); c.op(EQ);
                c.if_(|c| {
                    c.get(cursor); c.i64(8); c.op(ADD); c.set(cursor); c.get(cursor); c.set(start);
                    c.branch(2); // if/if/loop
                });
                c.get(word); c.op(EQZ);
                c.if_(|c| {
                    c.get(lowest); c.i64(h.quanta()); c.op(EQ); c.if_(|c| { c.get(cursor); c.set(lowest); });
                    c.get(cursor); c.i64(8); c.op(ADD); c.set(cursor);
                    c.get(cursor); c.get(start); c.op(SUB); c.get(quanta); c.op(GE);
                    c.if_(|c| { c.get(start); c.set(first); c.branch(4); }); // if/if/if/loop/exit block
                    c.branch(2);
                });
            });
            c.i64(h.occupancy()); c.get(cursor); c.op(ADD); c.set(address);
            c.load(address, 0, 0x31, 0); c.op(EQZ);
            c.if_else(|c| {
                c.get(lowest); c.i64(h.quanta()); c.op(EQ); c.if_(|c| { c.get(cursor); c.set(lowest); });
                c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor);
                c.get(cursor); c.get(start); c.op(SUB); c.get(quanta); c.op(GE);
                c.if_(|c| { c.get(start); c.set(first); c.branch(3); }); // if/if-else/loop/exit block
            }, |c| { c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor); c.get(cursor); c.set(start); });
            c.branch(0);
        });
        };
        if boxed {
            c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ);
            c.if_else(ordinary, |c| {
                // Congruence makes the metadata slot recoverable from a live owning
                // address at release, without adding a back-pointer to each allocation.
                c.get(slot); c.set(start);
                c.i64(h.quanta()); c.set(first);
                c.loop_(|c| {
                    c.get(start); c.get(quanta); c.op(ADD); c.i64(h.quanta()); c.op(GT); c.branch_if(1);
                    c.i64(0); c.set(cursor);
                    c.loop_(|c| {
                        c.get(cursor); c.get(quanta); c.op(EQ); c.branch_if(1);
                        c.i64(h.occupancy()); c.get(start); c.op(ADD); c.get(cursor); c.op(ADD); c.set(address);
                        c.load(address, 0, 0x31, 0); c.i64(0); c.op(NE); c.branch_if(1);
                        c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor); c.branch(0);
                    });
                    c.get(cursor); c.get(quanta); c.op(EQ);
                    c.if_(|c| { c.get(start); c.set(first); c.branch(2); });
                    c.get(start); c.i64(h.slots); c.op(ADD); c.set(start); c.branch(0);
                });
            });
        } else { ordinary(c); }
        c.get(first); c.i64(h.quanta()); c.op(EQ);
        c.if_(|c| allocation_error(c, 2));
        c.i64(HEAP_LOW); c.get(first); c.i64(5); c.op(SHL); c.op(ADD); c.set(base);
    });
    c.get(slot); c.i64(6); c.op(SHL); c.set(entry);
    c.get(old); c.i64(1); c.op(ADD); c.set(new);
    // COMMIT: all addresses/counts proven bounded above; no Err, call or callback below.
    c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address); c.get(quanta); c.set(remaining);
    span(&mut c, address, remaining,
        |c| c.store(address, 0, |c| c.i64(OCCUPIED), 0x37, 0),
        |c| c.store(address, 0, |c| c.i64(1), 0x3c, 0));
    // Clear the COMPLETE allocated bitmap extent, not payload bytes. One quantum's 32 cells
    // have four bitmap bytes, so quanta isolate bitmap bytes.
    c.i64(h.bitmap_low()); c.get(first); c.i64(2); c.op(SHL); c.op(ADD); c.set(address);
    c.get(quanta); c.i64(2); c.op(SHL); c.set(remaining);
    span(&mut c, address, remaining,
        |c| c.store(address, 0, |c| c.i64(0), 0x37, 0),
        |c| c.store(address, 0, |c| c.i64(0), 0x3c, 0));
    // Take the slot off the list it came from.
    c.get(head); c.op(EQZ);
    c.if_else(|c| c.store_at(h.hints() + FRESH_SLOT, |c| { c.get(slot); c.i64(1); c.op(ADD); }, 0x37, 3),
        |c| c.store_at(h.hints() + FREE_SLOT, |c| c.load(entry, NEXT, 0x29, 3), 0x37, 3));
    c.get(quanta); c.op(EQZ); c.op(0x45);
    if boxed { c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x71); }
    c.if_(|c| {
        // If the run we took began at the first free quantum seen, nothing below its end is free.
        c.get(lowest); c.get(first); c.op(EQ);
        c.if_else(|c| c.store_at(h.hints() + FREE_QUANTUM, |c| { c.get(first); c.get(quanta); c.op(ADD); }, 0x37, 3),
            |c| c.store_at(h.hints() + FREE_QUANTUM, |c| c.get(lowest), 0x37, 3));
    });
    for (offset, local) in [(GENERATION, new), (TYPE, 1), (BASE, base), (REQUESTED, requested), (EXTENT, quanta)] {
        c.store(entry, offset, |c| c.get(local), 0x3e, 2);
    }
    c.store(entry, CAPACITY, |c| c.get(2), 0x37, 3);
    c.store(entry, STRIDE, |c| c.get(stride), 0x37, 3);
    c.store(entry, COUNT, |c| c.i64(0), 0x37, 3);
    c.store(entry, FIRST, |c| c.get(first), 0x37, 3);
    c.store(entry, LIVE, |c| c.i64(1), 0x3e, 2);
    if boxed {
        c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x45);
        c.if_(|c| { c.i32(0); c.get(base); c.i64(0); c.op(RETURN); });
    }
    // Generation, slot, first quantum; plus one, so that no token is zero.
    c.i32(0);
    c.get(old); c.i64(h.slot_bits()); c.op(SHL); c.get(slot); c.op(ADD);
    c.i64(h.quantum_bits()); c.op(SHL); c.get(first); c.op(ADD);
    c.i64(1); c.op(ADD); c.i64(0);
    c.finish()
}
pub(super) fn capacity(h: Heap) -> Vec<u8> {
    let mut c = Code::new(2); // params token0/type1; entry2,decoded3
    validated(&mut c, h, 0, 1, 2, 3);
    c.load(2, CAPACITY, 0x29, 3);
    c.finish()
}
pub(super) fn try_release(h: Heap, types: &[(u64, u64)], boxed: bool) -> Vec<u8> {
    // params token0/type1; entry2,quanta3,first4,remaining5,address6,decoded7; audit locals 8..17
    let (entry, quanta, first, remaining, address) = (2, 3, 4, 5, 6);
    let mut c = Code::new(15);
    if boxed {
        c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x45);
        c.if_(|c| {
            c.get(0); c.i64(HEAP_LOW); c.op(LT); c.trap_if();
            c.get(0); c.i64(h.heap_high()); c.op(GE); c.trap_if();
            c.get(0); c.i64(QUANTUM - 1); c.op(AND); c.i64(0); c.op(NE); c.trap_if();
            c.get(0); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
            c.get(first); c.i64(h.slots - 1); c.op(AND); c.set(7);
            c.get(7); c.i64(6); c.op(SHL); c.set(entry);
            c.load(entry, GENERATION, 0x35, 2); c.i64(1); c.op(SUB);
            c.i64(h.slot_bits()); c.op(SHL); c.get(7); c.op(ADD);
            c.i64(h.quantum_bits()); c.op(SHL); c.get(first); c.op(ADD); c.i64(1); c.op(ADD); c.set(0);
        });
    }
    validated(&mut c, h, 0, 1, entry, 7);
    if boxed { c.get(1); c.i64(!BOX_TYPE); c.op(AND); c.set(1); }
    audit(&mut c, h, types, 1, entry, 8); // all validation BEFORE mutation
    c.load(entry, COUNT, 0x29, 3); c.i64(0); c.op(GT);
    c.if_(|c| { c.i32(1); c.get(0); c.op(RETURN); }); // exact owner, no writes on Err
    c.load(entry, EXTENT, 0x35, 2); c.set(quanta);
    c.get(quanta); c.i64(0); c.op(GT);
    c.if_(|c| {
        c.load(entry, BASE, 0x35, 2); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
        c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address); c.get(quanta); c.set(remaining);
        span(c, address, remaining,
            |c| c.store(address, 0, |c| c.i64(0), 0x37, 0),
            |c| c.store(address, 0, |c| c.i64(0), 0x3c, 0));
        c.get(first); c.load_at(h.hints() + FREE_QUANTUM, 0x29, 3); c.op(LT);
        c.if_(|c| c.store_at(h.hints() + FREE_QUANTUM, |c| c.get(first), 0x37, 3));
    });
    // A slot at its last generation is never used again, so it stays off the list.
    c.load(entry, GENERATION, 0x35, 2); c.i64(h.generation_max()); c.op(LT);
    c.if_(|c| {
        c.store(entry, NEXT, |c| c.load_at(h.hints() + FREE_SLOT, 0x29, 3), 0x37, 3);
        c.store_at(h.hints() + FREE_SLOT, |c| { c.get(entry); c.i64(6); c.op(SHR); c.i64(1); c.op(ADD); }, 0x37, 3);
    });
    c.store(entry, LIVE, |c| c.i64(0), 0x3e, 2); // preserves generation (including terminal MAX)
    c.i32(0); c.i64(0); // Unit Ok, inactive Err token only
    c.finish()
}

/// The one way emitted code reaches a cell it must test or change the state of: token, type, index and what the operation needs.
/// Returns (error, cell address in program memory). The occupancy bitmap and the full-cell count
/// live in the allocator's memory and are read and written only here.
///
/// Modes 0..2 test the index and the cell's state: 0 wants an empty cell and marks it full,
/// 1 wants a full cell and marks it empty, 2 wants a full cell and leaves it. Errors are
/// 1 (out of range) and 2 (wrong state), with nothing written. Modes 3..5 are the same three
/// without the tests, for operations whose bound and state a proof has established.
pub(super) fn cell_error(c: &mut Code, error: u64) {
    c.i64(error); c.i64(0); c.op(RETURN);
}
/// One pass of a range mode over `cells` (param 4) cells from `element`, mode in param 3: the
/// check (`commit` false: each start bit clear for mode 6, set for 7, else trap) or the commit
/// (set for 6, clear for 7). Where the stride is one byte the run's bits are consecutive, so each
/// step takes as many as remain in the bitmap word holding the next one, under a mask, aligned or
/// not (an 8-byte append at an odd length was 16 single-bit steps: `str`'s `push_text`); other
/// strides go a bit at a time.
pub(super) fn range_walk(c: &mut Code, h: Heap, commit: bool, (element, step, stride, bit, address, mask, byte): (u32, u32, u32, u32, u32, u32, u32)) {
    const SELECT: u8 = 0x1b;
    c.i64(0); c.set(step);
    c.loop_(|c| {
        c.get(step); c.get(4); c.op(GE); c.branch_if(1);
        c.get(element); c.get(step); c.get(stride); c.op(MUL); c.op(ADD); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.get(stride); c.i64(1); c.op(EQ);
        c.if_else(|c| {
            // At a word's first bit with 64 cells or more left: whole words, each compared with
            // (the check) or written as (the commit) one value, all ones or zero, as long as 64
            // cells remain. The masked step below does the same to one word with a mask of all
            // ones, and builds the mask each time: 1.75 instructions a cell against 0.6, for
            // every range of a long String (2026-11-22). What is left under a word, and a run
            // that starts inside one, go through the masked step.
            c.get(bit); c.i64(63); c.op(AND); c.op(EQZ);
            c.get(4); c.get(step); c.op(SUB); c.i64(64); c.op(GE); c.op(0x71); // i32.and
            c.if_(|c| {
                c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
                // Check: clear (6) or set (7). Commit: set (6) or clear (7).
                c.i64(if commit { u64::MAX } else { 0 }); c.i64(if commit { 0 } else { u64::MAX }); c.get(3); c.i64(6); c.op(EQ); c.op(SELECT); c.set(mask);
                c.loop_(|c| {
                    c.get(4); c.get(step); c.op(SUB); c.i64(64); c.op(LT); c.branch_if(1);
                    if commit { c.store(address, 0, |c| c.get(mask), 0x37, 0); }
                    else { c.load(address, 0, 0x29, 0); c.get(mask); c.op(NE); c.trap_if(); }
                    c.get(address); c.i64(8); c.op(ADD); c.set(address);
                    c.get(step); c.i64(64); c.op(ADD); c.set(step);
                    c.branch(0);
                });
                // Back to the head: it ends the walk, or takes what is left under a word.
                c.branch(2);
            });
            // The word holding `bit`; `byte` = n, the cells taken: those left, at most to the word's end.
            c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
            c.get(4); c.get(step); c.op(SUB); c.set(byte);
            c.i64(64); c.get(bit); c.i64(63); c.op(AND); c.op(SUB); c.set(mask);
            c.get(byte); c.get(mask); c.get(byte); c.get(mask); c.op(LT); c.op(SELECT); c.set(byte);
            // n ones from the bit's place in the word (n = 64 only at place 0; a shift by 64 is by 0).
            c.i64(u64::MAX); c.i64(1); c.get(byte); c.op(SHL); c.i64(1); c.op(SUB); c.get(byte); c.i64(64); c.op(EQ); c.op(SELECT);
            c.get(bit); c.i64(63); c.op(AND); c.op(SHL); c.set(mask);
            // The word, in `bit` (not needed again this step).
            c.load(address, 0, 0x29, 0); c.set(bit);
            if commit {
                // Set (6) or clear (7) the masked bits.
                c.store(address, 0, |c| {
                    c.get(bit); c.get(mask); c.op(OR);
                    c.get(bit); c.get(mask); c.i64(u64::MAX); c.op(XOR); c.op(AND);
                    c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
                }, 0x37, 0);
            } else {
                // All clear (6) or all set (7) under the mask, else a trap.
                c.get(bit); c.get(mask); c.op(AND);
                c.i64(0); c.get(mask); c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
                c.op(NE); c.trap_if();
            }
            c.get(step); c.get(byte); c.op(ADD); c.set(step);
        }, |c| {
            c.i64(h.bitmap_low()); c.get(bit); c.i64(3); c.op(SHR); c.op(ADD); c.set(address);
            c.i64(1); c.get(bit); c.i64(7); c.op(AND); c.op(SHL); c.set(mask);
            if commit {
                c.load(address, 0, 0x31, 0); c.set(byte);
                c.get(3); c.i64(6); c.op(EQ);
                c.if_else(|c| c.store(address, 0, |c| { c.get(byte); c.get(mask); c.op(OR); }, 0x3c, 0),
                    |c| c.store(address, 0, |c| { c.get(byte); c.get(mask); c.i64(255); c.op(XOR); c.op(AND); }, 0x3c, 0));
            } else {
                c.load(address, 0, 0x31, 0); c.get(mask); c.op(AND); c.op(EQZ);
                c.get(3); c.i64(6); c.op(EQ); c.op(0x47); c.trap_if(); // i32.ne: wrong state for the mode
            }
            c.get(step); c.i64(1); c.op(ADD); c.set(step);
        });
        c.branch(0);
    });
}
/// The range modes, 6 and 7, of the `cell` helper's interface: the same parameters, the cells
/// `index..index + cells` all empty and marked full (6) or all full and marked empty (7).
pub(super) fn range(h: Heap) -> Vec<u8> {
    let (entry, capacity, element, bit, address, mask, byte, count) = (5, 7, 8, 9, 10, 11, 12, 13);
    let (step, stride) = (14, 15);
    let _ = (bit, address, mask, byte);
    let mut c = Code::new(11);
    c.get(3); c.i64(6); c.op(LT); c.trap_if();
    c.get(3); c.i64(7); c.op(GT); c.trap_if();
    // Modes 6 and 7: `cells` cells from `index` on, all empty and marked full (6) or all full and
    // marked empty (7), for a typed range operation whose payloads the emitted code moves in
    // memory 0 (docs/proposal-cell-ranges.md). The token is always audited. Every bound and
    // every cell's state is checked before anything is written; a contradiction is a trap, as
    // the operation's proof says none can occur. Returns the first cell's address.
    c.get(3); c.i64(6); c.op(GE);
    c.if_(|c| {
        validated(c, h, 0, 1, entry, 6);
        c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
        // index <= capacity and cells <= capacity - index: no wrap.
        c.get(2); c.get(capacity); c.op(GT); c.trap_if();
        c.get(4); c.get(capacity); c.get(2); c.op(SUB); c.op(GT); c.trap_if();
        c.load(entry, STRIDE, 0x29, 3); c.set(stride);
        c.load(entry, BASE, 0x35, 2); c.get(2); c.get(stride); c.op(MUL); c.op(ADD); c.set(element);
        c.load(entry, COUNT, 0x29, 3); c.set(count); // validated: count <= capacity
        // The full-cell count moves by `cells` without leaving 0..capacity.
        c.get(3); c.i64(6); c.op(EQ);
        c.if_else(|c| { c.get(4); c.get(capacity); c.get(count); c.op(SUB); c.op(GT); c.trap_if(); },
            |c| { c.get(4); c.get(count); c.op(GT); c.trap_if(); });
        // Check: each cell's start bit is clear (6) or set (7). Then COMMIT: set (6) or clear (7)
        // each, then the count once. For one-byte cells the run's bits are consecutive, so an
        // aligned bitmap word (64 cells) or byte (8) is checked and written whole: the same test
        // as cell by cell, fewer steps (the 10 MB String's growth and close were a bit at a time).
        // A run of one-byte cells that lies in one bitmap word (an append of at most eight bytes
        // nearly always does): the walk's one step done once, the same test before the same
        // write. The two passes below each found the word and built the mask again, about 200
        // instructions of every such append (2026-11-22). No cell, no word: the walk does nothing.
        c.get(element); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.get(stride); c.i64(1); c.op(EQ);
        c.get(4); c.i64(0); c.op(NE); c.op(0x71); // i32.and
        c.get(bit); c.i64(63); c.op(AND); c.get(4); c.op(ADD); c.i64(64); c.op(LE); c.op(0x71);
        c.if_else(|c| {
            const SELECT: u8 = 0x1b;
            c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
            // `cells` ones from the bit's place in the word (64 only at place 0; a shift by 64 is by 0).
            c.i64(u64::MAX); c.i64(1); c.get(4); c.op(SHL); c.i64(1); c.op(SUB); c.get(4); c.i64(64); c.op(EQ); c.op(SELECT);
            c.get(bit); c.i64(63); c.op(AND); c.op(SHL); c.set(mask);
            c.load(address, 0, 0x29, 0); c.set(byte);
            // All clear (6) or all set (7) under the mask, else a trap, before the write.
            c.get(byte); c.get(mask); c.op(AND);
            c.i64(0); c.get(mask); c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
            c.op(NE); c.trap_if();
            // Set (6) or clear (7) the masked bits.
            c.store(address, 0, |c| {
                c.get(byte); c.get(mask); c.op(OR);
                c.get(byte); c.get(mask); c.i64(u64::MAX); c.op(XOR); c.op(AND);
                c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
            }, 0x37, 0);
        }, |c| {
            for commit in [false, true] {
                range_walk(c, h, commit, (element, step, stride, bit, address, mask, byte));
            }
        });
        c.get(3); c.i64(6); c.op(EQ);
        c.if_else(|c| c.store(entry, COUNT, |c| { c.get(count); c.get(4); c.op(ADD); }, 0x37, 3),
            |c| c.store(entry, COUNT, |c| { c.get(count); c.get(4); c.op(SUB); }, 0x37, 3));
        c.i64(0); c.get(element); c.op(RETURN);
    });
    c.op(0x00); // the range block returned
    c.finish()
}
pub(super) fn cell(h: Heap) -> Vec<u8> {
    // params token0,type1,index2,mode3,cells4; locals entry5,decoded6,capacity7,element8,bit9,
    // byteAddress10,mask11,byte12,count13 (and two the range modes used, kept so the local count
    // stays). `cells` is read by the range modes only.
    let (entry, capacity, element, bit, address, mask, byte, count) = (5, 7, 8, 9, 10, 11, 12, 13);
    let mut c = Code::new(11);
    // Modes 0..5; the range modes are the `range` helper's, a function of their own so that this
    // one, which every push and take calls, stays as small as it was (binary-trees ran 6-25%
    // slower while the range code shared it).
    c.get(3); c.i64(5); c.op(GT); c.trap_if();
    c.get(3); c.i64(6); c.op(LT);
    c.if_(|c| {
        // Modes 3..5 rest, as a proved read does in emitted code, on the borrow checker and the
        // operation's proof for the token being live and of this type: the entry is decoded from it
        // without the audit (decisions.md, "Proved writes skip the token audit"). Modes 0..2 audit it.
        // Mode 2, a checked reference (a lookup: `memory.ref`, the hash map's probes), writes nothing
        // and keeps the zero-token, bounds and occupancy checks below; the token audit's facts
        // (live, generation, place, type, count within capacity) rest on the borrow checker for the
        // owner it is lent from, as modes 3..5 do (decisions.md, "Lookup-only checked reference").
        c.get(3); c.i64(2); c.op(LT);
        c.if_else(|c| validated(c, h, 0, 1, entry, 6), |c| {
            c.get(3); c.i64(2); c.op(EQ); c.if_(|c| { c.get(0); c.op(EQZ); c.trap_if(); });
            c.get(0); c.i64(1); c.op(SUB); c.i64(h.quantum_bits()); c.op(SHR); c.i64(h.slots - 1); c.op(AND); c.i64(6); c.op(SHL); c.set(entry);
        });
        c.get(3); c.i64(3); c.op(LT);
        c.if_(|c| {
            c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
            c.get(2); c.get(capacity); c.op(GE); c.if_(|c| cell_error(c, 1)); // full-width before multiply/wrap
        });
        c.load(entry, BASE, 0x35, 2); c.get(2); c.load(entry, STRIDE, 0x29, 3); c.op(MUL); c.op(ADD); c.set(element);
        c.get(3); c.i64(5); c.op(EQ);
        c.if_(|c| { c.i64(0); c.get(element); c.op(RETURN); }); // a proved read touches no cell state
        c.get(element); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.i64(h.bitmap_low()); c.get(bit); c.i64(3); c.op(SHR); c.op(ADD); c.set(address);
        c.i64(1); c.get(bit); c.i64(7); c.op(AND); c.op(SHL); c.set(mask);
        c.load(address, 0, 0x31, 0); c.set(byte);
        c.load(entry, COUNT, 0x29, 3); c.set(count); // validated: count <= capacity
        c.get(3); c.i64(3); c.op(LT);
        c.if_(|c| {
            // Selected bit/count contradictions are corruption, never a normal Empty/Full error.
            c.get(byte); c.get(mask); c.op(AND); c.op(EQZ);
            c.if_else(|c| { c.get(count); c.get(capacity); c.op(EQ); c.trap_if(); },
                |c| { c.get(count); c.op(EQZ); c.trap_if(); });
            // Mode 0 wants the cell empty; modes 1 and 2 want it full.
            c.get(byte); c.get(mask); c.op(AND); c.op(EQZ); c.get(3); c.op(EQZ); c.op(0x47); c.if_(|c| cell_error(c, 2));
        });
        // COMMIT: no Err below. Modes 0 and 3 mark the cell full, 1 and 4 mark it empty.
        c.get(3); c.i64(3); c.op(REM); c.op(EQZ);
        c.if_(|c| {
            c.store(address, 0, |c| { c.get(byte); c.get(mask); c.op(OR); }, 0x3c, 0);
            c.store(entry, COUNT, |c| { c.get(count); c.i64(1); c.op(ADD); }, 0x37, 3);
        });
        c.get(3); c.i64(3); c.op(REM); c.i64(1); c.op(EQ);
        c.if_(|c| {
            c.store(address, 0, |c| { c.get(byte); c.get(mask); c.i64(255); c.op(XOR); c.op(AND); }, 0x3c, 0);
            c.store(entry, COUNT, |c| { c.get(count); c.i64(1); c.op(SUB); }, 0x37, 3);
        });
        c.i64(0); c.get(element);
        c.op(RETURN);
    });
    c.op(0x00); // every mode returned above: unreachable
    c.finish()
}

