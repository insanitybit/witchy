//! Native buffer ABI. Calls are serialized on the guest thread and never
//! suspend. Forked workers have private heap state. The compiler owns the
//! lifetime and aliasing discipline for addresses returned to guest code.

use super::{heap_layout, heap_range, heap_slots::Slots};
use core::{ffi::c_void, mem::size_of, ptr, slice};

#[cfg(test)]
#[test]
fn allocated_buffers_transfer_and_release() {
    witchy_allow_allocator();
    unsafe {
        let first = witchy_allocate(2, 1, 1, 4);
        assert!(first > u64::from(u32::MAX));
        let second = witchy_allocate(2, 1, 1, 4);
        assert!(second > u64::from(u32::MAX));
        assert_eq!(witchy_cell(first, 1, 4, u64::from(READ)), 1);
        assert_eq!(witchy_cell(first, 1, 0, u64::from(READ)), 2);
        let value = 7u8;
        witchy_fill(first, 1, 4, &value);
        let address = witchy_cell(first, 1, 0, u64::from(READ));
        assert!(address > 2);
        assert_eq!(
            ptr::with_exposed_provenance::<u8>(address as usize).read(),
            value
        );
        assert_eq!(witchy_read_word(first, 1, 0, 4), 0x07070707);
        witchy_transfer(first, second, 1, 0, 4, 1);
        assert_eq!(witchy_release(first, 1), 0);
        assert_eq!(witchy_read_word(second, 1, 0, 4), 0x07070707);
        witchy_take_prefix(second, 1, 4);
        assert_eq!(witchy_release(second, 1), 0);

        // Exercise actual ABI operations across empty and partial ranges. The
        // independent word/mask model does not inspect the buffer metadata.
        for count in 0..=4usize {
            for offset in 0..=4usize {
                let source = witchy_allocate(2, 1, 1, 4);
                assert!(source > u64::from(u32::MAX));
                let target = witchy_allocate(2, 1, 1, 8);
                assert!(target > u64::from(u32::MAX));
                let word = 0x8877_6655u64;
                let expected = word & ((1u64 << (count * 8)) - 1);
                witchy_init_word(source, 1, 0, count, word);
                assert_eq!(witchy_read_word(source, 1, 0, count), expected);
                witchy_transfer(source, target, 1, offset, count, 1);
                assert_eq!(witchy_release(source, 1), 0);
                assert_eq!(witchy_read_word(target, 1, offset, count), expected);
                assert_eq!(witchy_release(target, 1), u64::from(count != 0));
                if count != 0 {
                    for index in offset..offset + count {
                        assert!(witchy_cell(target, 1, index, u64::from(TAKE)) > 2);
                    }
                    assert_eq!(witchy_release(target, 1), 0);
                }
            }
        }
        // A dense buffer named by slot and generation (a checked or observed build): after
        // its release, a buffer made at the same address is not reached through the old name.
        let mut old = witchy_allocate(2, 1 | PREFIX, 8, 4);
        let mut reused = false;
        for _ in 0..64 {
            let address = live(old) as usize;
            assert_eq!(witchy_release(old, 1), 0);
            let next = witchy_allocate(2, 1 | PREFIX, 8, 4);
            let again = live(next) as usize == address;
            if again {
                reused = true;
                assert_ne!(next, old);
                assert!((&*(&raw const HEAP)).slots.get(old).is_none(), "a stale handle names the new buffer");
                assert_eq!(witchy_observe(next, 1, 0, 0, u64::from(READ)) as usize, address + size_of::<Buffer>());
            }
            old = next;
            if again {
                break;
            }
        }
        assert!(reused, "the allocator gave no address again in 64 tries");
        assert_eq!(witchy_release(old, 1), 0);

        // The same buffer named by its header's address (a default build): no slot is used,
        // an error is still a small number, and the quota is given back.
        let before = (&*(&raw const HEAP)).bytes;
        let direct = witchy_allocate(2, 1 | PREFIX | DIRECT, 8, 4);
        assert!(direct >= 4096 && direct % 8 == 0);
        assert!((&*(&raw const HEAP)).bytes > before);
        assert_eq!(witchy_capacity(direct, 1 | DIRECT), 4);
        assert_eq!(witchy_observe(direct, 1 | DIRECT, 0, 0, u64::from(READ)), direct + size_of::<Buffer>() as u64);
        assert_eq!(witchy_release(direct, 1 | DIRECT), 0);
        assert_eq!((&*(&raw const HEAP)).bytes, before);
        assert_eq!(witchy_allocate(2, 1 | PREFIX | DIRECT, 8, u64::MAX), 1);
    }
}

unsafe extern "C" {
    fn malloc(bytes: usize) -> *mut c_void;
    fn free(address: *mut c_void);
}

const PREFIX: u64 = 1 << 29;
/// A dense buffer named by its header's address, not by a slot: what a `PrefixBuffer`'s owner
/// holds in a default build (src/compiler/native/body/storage.rs). The buffer has one owner,
/// which issues no key, link or weak reference (the checker refuses each), so nothing can name
/// it once it is released, as nothing names a Rust `Vec`'s storage after its drop. A checked or
/// observed build keeps the slot and generation for these buffers too.
const DIRECT: u64 = 1 << 31;
const INIT: u8 = 0;
const TAKE: u8 = 1;
const READ: u8 = 2;

#[repr(C)]
struct Buffer {
    capacity: usize,
    stride: usize,
    bytes: usize,
    occupied: u32,
    prefix: u32,
    kind: u64,
}

/// Compiled code reads the slot table in place at `witchy_heap` (its first field) for a proved
/// cell access; see src/compiler/native/body/cells.rs. The slot count is `SLOTS` there.
#[repr(C)]
struct Heap {
    slots: Slots<ptr::NonNull<Buffer>, { 1 << 18 }>,
    bytes: usize,
    granted: bool,
}

/// How many times the program asked for an allocation (granted or not). A run with
/// `WITCHY_COUNT_ALLOCATIONS` set in its environment says the number on standard output when
/// the program returns: the measure behind "a parse allocates a fixed number of times".
static mut ALLOCATIONS: u64 = 0;

pub(super) fn report_allocations() {
    unsafe extern "C" {
        fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char;
    }
    if !unsafe { getenv(c"WITCHY_COUNT_ALLOCATIONS".as_ptr()) }.is_null() {
        say(*b"allocations: 00000000000000000000\n", unsafe { ALLOCATIONS });
    }
    // `WITCHY_REPORT_LIVE`: the bytes of every allocation not released when the program
    // returns. A program that released each owner once says 0 (scripts/native-vec-fuzz.py).
    if !unsafe { getenv(c"WITCHY_REPORT_LIVE".as_ptr()) }.is_null() {
        say(*b"live bytes:  00000000000000000000\n", unsafe { (*(&raw const HEAP)).bytes } as u64);
    }
}

/// A 13-byte label and a number, on standard output (the stream a run always has; standard
/// error may be closed).
fn say(mut text: [u8; 34], number: u64) {
    unsafe extern "C" {
        fn write(fd: core::ffi::c_int, bytes: *const core::ffi::c_void, len: usize) -> isize;
    }
    let (mut left, mut first) = (number, 32);
    for at in (13..33).rev() {
        text[at] = b'0' + (left % 10) as u8;
        left /= 10;
        if text[at] != b'0' {
            first = at;
        }
    }
    // The number without its leading zeros, after the label.
    text.copy_within(first..34, 13);
    let length = 13 + 34 - first;
    unsafe { write(1, text.as_ptr().cast(), length) };
}

// The layout compiled code reads in place (src/compiler/native/body/cells.rs): a change here
// that does not change that file too fails to compile.
const _: () = {
    assert!(size_of::<Buffer>() == 40);
    assert!(core::mem::offset_of!(Buffer, capacity) == 0);
    assert!(core::mem::offset_of!(Buffer, stride) == 8);
    assert!(core::mem::offset_of!(Buffer, occupied) == 24);
    assert!(core::mem::offset_of!(Buffer, prefix) == 28);
    assert!(core::mem::offset_of!(Buffer, kind) == 32);
    assert!(PREFIX == 1 << 29);
    assert!(core::mem::offset_of!(Heap, slots) == 0);
    assert!(super::heap_slots::SLOT_BYTES == 16 && super::heap_slots::SLOT_GENERATION == 8);
    assert!(size_of::<Slots<ptr::NonNull<Buffer>, { 1 << 18 }>>() == (1 << 18) * 16 + 8);
};

#[unsafe(export_name = "witchy_heap")]
static mut HEAP: Heap = Heap {
    slots: Slots::new(),
    bytes: 0,
    granted: false,
};

#[cfg(not(kani))]
fn trap() -> ! {
    // The prior runtime used a hardware trap on ABI contract violations.
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[cfg(kani)]
fn trap() -> ! {
    panic!("native buffer contract violation")
}

impl Buffer {
    fn dense(&self) -> bool {
        self.kind & PREFIX != 0
    }

    // The metadata reference ends at the header. Derive payload and bitmap
    // pointers from the original malloc pointer to retain allocation provenance.
    unsafe fn payload(buffer: *mut Self) -> *mut u8 {
        unsafe { buffer.add(1).cast() }
    }

    unsafe fn permits(buffer: *mut Self, from: usize, end: usize, mode: u8) -> bool {
        let header = unsafe { &*buffer };
        let bitmap = if header.dense() {
            None
        } else {
            let start = unsafe { Self::payload(buffer).add(header.capacity * header.stride) };
            Some(unsafe { slice::from_raw_parts(start, header.capacity.div_ceil(8)) })
        };
        heap_range::permits(bitmap, header.prefix as usize, from, end, mode)
    }

    unsafe fn mark(buffer: *mut Self, from: usize, end: usize, mode: u8) {
        if from == end || mode == READ {
            return;
        }
        let header = unsafe { &*buffer };
        let bitmap = if header.dense() {
            None
        } else {
            let start = unsafe { Self::payload(buffer).add(header.capacity * header.stride) };
            Some(unsafe { slice::from_raw_parts_mut(start, header.capacity.div_ceil(8)) })
        };
        let Some((prefix, occupied)) = heap_range::mark(
            bitmap,
            header.prefix,
            header.occupied,
            from as u32,
            end as u32,
            mode,
        ) else {
            trap()
        };
        unsafe {
            (*buffer).prefix = prefix;
            (*buffer).occupied = occupied;
        }
    }

    unsafe fn range(buffer: *mut Self, from: usize, count: usize, mode: u8) -> *mut u8 {
        let Some(end) = from.checked_add(count) else {
            trap()
        };
        let header = unsafe { &*buffer };
        if end > header.capacity || !unsafe { Self::permits(buffer, from, end, mode) } {
            trap();
        }
        let offset = from * header.stride;
        if !header.dense()
            && mode == READ
            && from <= header.prefix as usize
            && end > header.prefix as usize
        {
            unsafe { (*buffer).prefix = end as u32 };
        }
        unsafe { Self::payload(buffer).add(offset) }
    }
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(12)]
fn admitted_sparse_read_stays_in_payload() {
    #[repr(C)]
    struct Fixture {
        header: Buffer,
        payload: [u8; 8],
        bitmap: u8,
    }
    let bitmap: u8 = kani::any();
    let from: u8 = kani::any();
    let end: u8 = kani::any();
    kani::assume(from <= end && end <= 8);
    let mut fixture = Fixture {
        header: Buffer {
            capacity: 8,
            stride: 1,
            bytes: size_of::<Fixture>(),
            occupied: bitmap.count_ones(),
            prefix: bitmap.trailing_ones(),
            kind: 1,
        },
        payload: [0; 8],
        bitmap,
    };
    kani::assume(heap_range::permits(
        Some(&[bitmap]),
        fixture.header.prefix as usize,
        from as usize,
        end as usize,
        READ,
    ));
    kani::cover(from == 0 && end == 8, "a full initialized buffer is read");
    let pointer = unsafe {
        Buffer::range(
            &raw mut fixture.header,
            from as usize,
            (end - from) as usize,
            READ,
        )
    };
    let expected = unsafe { fixture.payload.as_mut_ptr().add(from as usize) };
    assert_eq!(pointer, expected);
    assert!(fixture.header.prefix <= fixture.bitmap.trailing_ones());
}

/// With `WITCHY_HEAP_AUDIT` in the environment, a buffer's bookkeeping is recounted from its
/// bitmap whenever it is released: a count or prefix that disagrees with the cells traps. This
/// is how the inline cell accesses of proved code are checked against this runtime
/// (scripts/native-checked.py).
fn audit(buffer: *mut Buffer) {
    static mut CHECKED: u8 = 0;
    unsafe extern "C" {
        fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char;
    }
    unsafe {
        if CHECKED == 0 {
            CHECKED = if getenv(c"WITCHY_HEAP_AUDIT".as_ptr()).is_null() { 1 } else { 2 };
        }
        if CHECKED != 2 {
            return;
        }
    }
    let header = unsafe { &*buffer };
    if header.dense() {
        if header.prefix as usize > header.capacity || header.occupied != 0 {
            trap();
        }
        return;
    }
    let start = unsafe { Buffer::payload(buffer).add(header.capacity * header.stride) };
    let bitmap = unsafe { slice::from_raw_parts(start, header.capacity.div_ceil(8)) };
    let mut occupied = 0u32;
    let mut leading = 0usize;
    let mut run = true;
    for index in 0..header.capacity {
        let full = bitmap[index / 8] & (1 << (index % 8)) != 0;
        occupied += u32::from(full);
        if run && full { leading += 1 } else { run = false }
    }
    if occupied != header.occupied || header.prefix as usize > leading {
        trap();
    }
}

fn live(handle: u64) -> *mut Buffer {
    let Some(buffer) = (unsafe { (&*(&raw const HEAP)).slots.get(handle) }) else {
        trap()
    };
    buffer.as_ptr()
}

fn typed(handle: u64, kind: u64) -> *mut Buffer {
    let buffer = if kind & DIRECT != 0 {
        // Small numbers are allocation errors, never an address.
        if handle < 4096 {
            trap();
        }
        ptr::with_exposed_provenance_mut(handle as usize)
    } else {
        live(handle)
    };
    if unsafe { (*buffer).kind } & !PREFIX != kind {
        trap();
    }
    buffer
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_allow_allocator() {
    unsafe { HEAP.granted = true };
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_allocate(grant: u64, kind: u64, stride: u64, count: u64) -> u64 {
    unsafe { ALLOCATIONS += 1 };
    let heap = unsafe { &mut *(&raw mut HEAP) };
    let direct = kind & DIRECT != 0;
    if !heap.granted || grant != 2 || kind & !(PREFIX | DIRECT) == 0 || (direct && kind & PREFIX == 0) || stride == 0 || stride > 2048 {
        trap();
    }
    let Some(layout) = heap_layout::layout(
        stride,
        count,
        kind & PREFIX != 0,
        size_of::<Buffer>() as u64,
    ) else {
        return 1;
    };
    if !direct && !heap.slots.has_room() {
        return 3;
    }
    let bytes = layout.total as usize;
    if bytes > heap_layout::BYTE_LIMIT as usize - heap.bytes {
        return 2;
    }
    let Some(buffer) = ptr::NonNull::new(unsafe { malloc(bytes) }.cast::<Buffer>()) else {
        return 2;
    };
    unsafe {
        buffer.as_ptr().write(Buffer {
            capacity: count as usize,
            stride: stride as usize,
            bytes,
            occupied: 0,
            prefix: 0,
            kind,
        });
        Buffer::payload(buffer.as_ptr())
            .add(layout.payload as usize)
            .write_bytes(0, layout.bitmap as usize);
    }
    if direct {
        heap.bytes += bytes;
        return buffer.as_ptr().expose_provenance() as u64;
    }
    let Some(issued) = heap.slots.insert(buffer) else {
        unsafe { free(buffer.as_ptr().cast()) };
        return 3;
    };
    heap.bytes += bytes;
    issued
}

#[unsafe(no_mangle)]
extern "C" fn witchy_capacity(handle: u64, kind: u64) -> u64 {
    unsafe { (*typed(handle, kind)).capacity as u64 }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_release(handle: u64, kind: u64) -> u64 {
    let buffer = typed(handle, kind);
    audit(buffer);
    let header = unsafe { &*buffer };
    if header.prefix != 0 || header.occupied != 0 {
        return 1;
    }
    let heap = unsafe { &mut *(&raw mut HEAP) };
    if kind & DIRECT == 0 && heap.slots.remove(handle).is_none() {
        trap();
    }
    heap.bytes -= header.bytes;
    // A buffer named by its address has no slot to retire. Its type word is cleared, so a
    // second release of the block, or a runtime call on it, fails `typed` for as long as the
    // allocator leaves the block alone. This is a cheap second line and no more: the first
    // is that the owner is linear, and a checked or observed build keeps the generation.
    unsafe { (*buffer).kind = 0 };
    unsafe { free(buffer.cast()) };
    0
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_cell(handle: u64, kind: u64, index: usize, mode: u64) -> u64 {
    let buffer = typed(handle, kind);
    if mode > u64::from(READ) {
        trap();
    }
    if index >= unsafe { (*buffer).capacity } {
        return 1;
    }
    if !unsafe { Buffer::permits(buffer, index, index + 1, mode as u8) } {
        return 2;
    }
    unsafe {
        Buffer::mark(buffer, index, index + 1, mode as u8);
        // The address points past the header of a live allocation, so it
        // cannot equal the small integer error codes.
        Buffer::payload(buffer)
            .add(index * (*buffer).stride)
            .expose_provenance() as u64
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_read_range(handle: u64, kind: u64, from: usize, count: usize) -> u64 {
    unsafe { Buffer::range(typed(handle, kind), from, count, READ).expose_provenance() as u64 }
}

/// What a proved access written into compiled code must come to (an observed build,
/// src/compiler/native/body/cells.rs): the runtime's own judgement of the access, made before
/// the compiled code marks it, and the address the compiled code must have reached.
#[unsafe(no_mangle)]
extern "C" fn witchy_observe(handle: u64, kind: u64, from: usize, count: usize, mode: u64) -> u64 {
    let buffer = typed(handle, kind);
    let Some(end) = from.checked_add(count) else {
        trap()
    };
    let header = unsafe { &*buffer };
    if mode > u64::from(READ) || end > header.capacity || !unsafe { Buffer::permits(buffer, from, end, mode as u8) } {
        trap();
    }
    unsafe { Buffer::payload(buffer).add(from * header.stride).expose_provenance() as u64 }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_take_prefix(handle: u64, kind: u64, count: usize) {
    let buffer = typed(handle, kind);
    unsafe {
        Buffer::range(buffer, 0, count, TAKE);
        Buffer::mark(buffer, 0, count, TAKE);
    }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_transfer(
    source: u64,
    target: u64,
    kind: u64,
    at: usize,
    count: usize,
    take: u64,
) {
    let src = typed(source, kind);
    let dst = typed(target, kind);
    let stride = unsafe { (*src).stride };
    if stride != unsafe { (*dst).stride } || take > 1 || (take != 0 && source == target) {
        trap();
    }
    unsafe { transfer_buffers(src, dst, at, count, stride, take != 0) }
}

unsafe fn transfer_buffers(
    src: *mut Buffer,
    dst: *mut Buffer,
    at: usize,
    count: usize,
    stride: usize,
    take: bool,
) {
    unsafe {
        // A move must admit the removal as well as the read. Dense
        // storage cannot represent a hole before an initialized suffix.
        let from = Buffer::range(src, 0, count, if take { TAKE } else { READ });
        let to = Buffer::range(dst, at, count, INIT);
        ptr::copy(from, to, count * stride);
        Buffer::mark(dst, at, at + count, INIT);
        if take {
            Buffer::mark(src, 0, count, TAKE);
        }
    }
}

#[cfg(kani)]
#[kani::proof]
fn dense_transfer_admits_only_empty_or_whole_prefix_moves() {
    let prefix: u32 = kani::any();
    let count: u32 = kani::any();
    let capacity: u32 = kani::any();
    kani::assume(prefix <= capacity && count <= capacity);
    let mut header = Buffer {
        capacity: capacity as usize,
        stride: 1,
        bytes: 0,
        occupied: 0,
        prefix,
        kind: PREFIX | 1,
    };
    // Dense admission reads only the header. No payload pointer is formed.
    let allowed = unsafe { Buffer::permits(&raw mut header, 0, count as usize, TAKE) };
    assert_eq!(allowed, count == 0 || count == prefix);
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(6)]
fn transfer_moves_only_initialized_cells() {
    #[repr(C)]
    struct Fixture {
        header: Buffer,
        payload: [u8; 4],
        bitmap: u8,
    }
    let values: [u8; 4] = kani::any();
    let count: u8 = kani::any();
    let at: u8 = kani::any();
    let take: bool = kani::any();
    kani::assume(count <= 4 && at <= 4 - count);
    let initial = ((1u16 << count) - 1) as u8;
    let mut src = Fixture {
        header: Buffer {
            capacity: 4,
            stride: 1,
            bytes: size_of::<Fixture>(),
            occupied: u32::from(count),
            prefix: u32::from(count),
            kind: 1,
        },
        payload: values,
        bitmap: initial,
    };
    let mut dst = Fixture {
        header: Buffer {
            capacity: 4,
            stride: 1,
            bytes: size_of::<Fixture>(),
            occupied: 0,
            prefix: 0,
            kind: 1,
        },
        payload: [0; 4],
        bitmap: 0,
    };
    unsafe {
        transfer_buffers(
            &raw mut src.header,
            &raw mut dst.header,
            at as usize,
            count as usize,
            1,
            take,
        )
    };
    for i in 0..4 {
        let expected = if i >= at as usize && i < (at + count) as usize {
            values[i - at as usize]
        } else {
            0
        };
        assert_eq!(dst.payload[i], expected);
    }
    assert_eq!(src.payload, values);
    assert_eq!(src.bitmap, if take { 0 } else { initial });
    assert_eq!(src.header.occupied, if take { 0 } else { u32::from(count) });
    assert_eq!(dst.bitmap, initial << at);
    assert_eq!(dst.header.occupied, u32::from(count));
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_fill(handle: u64, kind: u64, count: usize, element: *const u8) {
    let buffer = typed(handle, kind);
    unsafe { fill_buffer(buffer, count, element) }
}

// The element may refer to the destination allocation. `copy` permits
// overlapping valid ranges, avoiding undefined behavior at the ABI boundary.
unsafe fn fill_buffer(buffer: *mut Buffer, count: usize, element: *const u8) {
    let stride = unsafe { (*buffer).stride };
    unsafe {
        let base = Buffer::range(buffer, 0, count, INIT);
        // A small element is copied out first (it may lie in the destination), so the cells
        // can be written without a call each: all zero bytes at once, a word as a word.
        let mut held = [0u8; 16];
        if stride <= held.len() {
            ptr::copy(element, held.as_mut_ptr(), stride);
            if held.iter().all(|byte| *byte == 0) {
                ptr::write_bytes(base, 0, count * stride);
            } else if stride == 8 {
                let word = u64::from_ne_bytes(held[..8].try_into().unwrap());
                for index in 0..count {
                    base.add(index * 8).cast::<u64>().write_unaligned(word);
                }
            } else {
                for index in 0..count {
                    ptr::copy_nonoverlapping(held.as_ptr(), base.add(index * stride), stride);
                }
            }
        } else {
            for index in 0..count {
                ptr::copy(element, base.add(index * stride), stride);
            }
        }
        Buffer::mark(buffer, 0, count, INIT);
    }
}

#[cfg(test)]
#[test]
fn fill_accepts_a_source_in_the_destination_allocation() {
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    let layout = Layout::from_size_align(size_of::<Buffer>() + 5, align_of::<Buffer>()).unwrap();
    unsafe {
        let allocation = alloc_zeroed(layout);
        assert!(!allocation.is_null());
        let buffer = allocation.cast::<Buffer>();
        buffer.write(Buffer {
            capacity: 4,
            stride: 1,
            bytes: layout.size(),
            occupied: 0,
            prefix: 0,
            kind: 1,
        });
        let payload = Buffer::payload(buffer);
        ptr::copy_nonoverlapping([8, 9, 3, 4].as_ptr(), payload, 4);
        fill_buffer(buffer, 4, payload.add(2));
        assert_eq!(slice::from_raw_parts(payload, 4), &[3; 4]);
        assert_eq!(*payload.add(4), 0b1111);
        dealloc(allocation, layout);
    }
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(10)]
fn filling_with_an_alias_stays_in_payload() {
    #[repr(C)]
    struct Fixture {
        header: Buffer,
        payload: [u8; 8],
        bitmap: u8,
    }
    let payload: [u8; 8] = kani::any();
    let count: u8 = kani::any();
    let source: u8 = kani::any();
    kani::assume(count <= 8 && source < 8);
    let mut fixture = Fixture {
        header: Buffer {
            capacity: 8,
            stride: 1,
            bytes: size_of::<Fixture>(),
            occupied: 0,
            prefix: 0,
            kind: 1,
        },
        payload,
        bitmap: 0,
    };
    let element = unsafe { fixture.payload.as_ptr().add(source as usize) };
    unsafe { fill_buffer(&raw mut fixture.header, count as usize, element) };
    for i in 0..8 {
        assert_eq!(
            fixture.payload[i],
            if i < count as usize {
                payload[source as usize]
            } else {
                payload[i]
            }
        );
    }
    assert_eq!(fixture.bitmap, ((1u16 << count) - 1) as u8);
    assert_eq!(fixture.header.occupied, u32::from(count));
    assert_eq!(fixture.header.prefix, u32::from(count));
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(10)]
fn filling_multibyte_cells_with_an_alias_stays_in_payload() {
    #[repr(C)]
    struct Fixture {
        header: Buffer,
        payload: [u8; 8],
        bitmap: u8,
    }
    let payload: [u8; 8] = kani::any();
    let count: u8 = kani::any();
    let source: u8 = kani::any();
    kani::assume(count <= 4 && source < 4);
    let mut fixture = Fixture {
        header: Buffer {
            capacity: 4,
            stride: 2,
            bytes: size_of::<Fixture>(),
            occupied: 0,
            prefix: 0,
            kind: 1,
        },
        payload,
        bitmap: 0,
    };
    let element = unsafe { fixture.payload.as_ptr().add(2 * source as usize) };
    unsafe { fill_buffer(&raw mut fixture.header, count as usize, element) };
    for i in 0..8 {
        let expected = if i < 2 * count as usize {
            payload[2 * source as usize + i % 2]
        } else {
            payload[i]
        };
        assert_eq!(fixture.payload[i], expected);
    }
    assert_eq!(fixture.bitmap, ((1u16 << count) - 1) as u8);
    assert_eq!(fixture.header.occupied, u32::from(count));
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_read_word(
    handle: u64,
    kind: u64,
    from: usize,
    count: usize,
) -> u64 {
    let buffer = typed(handle, kind);
    if unsafe { (*buffer).stride } != 1 || count > 8 {
        trap();
    }
    let mut word = [0; 8];
    unsafe {
        ptr::copy_nonoverlapping(
            Buffer::range(buffer, from, count, READ),
            word.as_mut_ptr(),
            count,
        );
    }
    u64::from_le_bytes(word)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_init_word(
    handle: u64,
    kind: u64,
    from: usize,
    count: usize,
    word: u64,
) {
    let buffer = typed(handle, kind);
    if unsafe { (*buffer).stride } != 1 || count > 8 {
        trap();
    }
    unsafe {
        let bytes = Buffer::range(buffer, from, count, INIT);
        ptr::copy_nonoverlapping(word.to_le_bytes().as_ptr(), bytes, count);
        Buffer::mark(buffer, from, from + count, INIT);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_init_slice(
    token: u64, kind: u64, at: usize, source: *const u8, count: usize,
) {
    let buffer = typed(token, kind);
    unsafe {
        let target = Buffer::range(buffer, at, count, INIT);
        if count != 0 {
            // The compiler supplies a live shared slice with this element layout.
            ptr::copy(source, target, count * (*buffer).stride);
        }
        Buffer::mark(buffer, at, at + count, INIT);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_copy_slice(target: *mut u8, source: *const u8, bytes: usize) {
    if bytes != 0 {
        unsafe { ptr::copy(source, target, bytes) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_box_loan(handle: u64, element_type: u64) -> u64 {
    let buffer = live(handle);
    let header = unsafe { &*buffer };
    if header.kind & !(PREFIX | (1 << 30)) != element_type
        || header.capacity == 0
        || !unsafe { Buffer::permits(buffer, 0, 1, READ) }
    {
        trap();
    }
    unsafe { Buffer::payload(buffer).expose_provenance() as u64 }
}

#[cfg(kani)]
#[kani::proof]
fn scalar_cells_end_before_occupancy_bitmap() {
    let shift: u8 = kani::any();
    kani::assume(shift <= 3);
    let stride = 1u64 << shift;
    let count: u32 = kani::any();
    let index: u32 = kani::any();
    let dense: bool = kani::any();
    kani::assume(index < count);
    let index = u64::from(index);
    let header = size_of::<Buffer>() as u64;
    if let Some(layout) = heap_layout::layout(stride, u64::from(count), dense, header) {
        let start = header + index * stride;
        assert!(start > 2);
        assert!(start + stride <= layout.total - layout.bitmap);
    }
}
