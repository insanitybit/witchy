//! Native byte budgets. The launcher serializes workers with waitpid.
//! Each worker gets a private copy of the per-connection budget.

use super::budget;
use core::ffi::c_char;

#[derive(Clone, Copy)]
struct Counters {
    output: u64,
    network: u64,
}

impl Counters {
    const UNLIMITED: Self = Self {
        output: u64::MAX,
        network: u64::MAX,
    };

    fn counter(&mut self, network: bool) -> &mut u64 {
        if network {
            &mut self.network
        } else {
            &mut self.output
        }
    }
}

static mut LOCAL: Counters = Counters::UNLIMITED;
static mut TOTAL: *mut Counters = &raw mut LOCAL;
static mut CONNECTION: Counters = Counters::UNLIMITED;

#[unsafe(no_mangle)]
extern "C" fn witchy_budget_share() -> bool {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            size: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
    }
    // A shared anonymous page carries the run-wide counters across workers.
    #[cfg(target_os = "macos")]
    const SHARED_ANON: c_int = 0x1001;
    #[cfg(target_os = "linux")]
    const SHARED_ANON: c_int = 0x21;
    let mapped = unsafe {
        mmap(
            core::ptr::null_mut(),
            size_of::<Counters>(),
            3,
            SHARED_ANON,
            -1,
            0,
        )
    };
    if mapped.addr() == usize::MAX {
        return false;
    }
    let shared = mapped.cast::<Counters>();
    unsafe {
        shared.write(TOTAL.read());
        TOTAL = shared;
    }
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_budget_configure(
    text: *const c_char,
    network: bool,
    per_connection: bool,
) -> bool {
    let input = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
    let Some(limit) = budget::parse_limit(input) else {
        return false;
    };
    let counters = if per_connection {
        &raw mut CONNECTION
    } else {
        unsafe { TOTAL }
    };
    unsafe { *(*counters).counter(network) = limit };
    true
}

fn charge(network: bool, bytes: u64) {
    unsafe extern "C" {
        safe fn exit(status: i32) -> !;
    }
    let total = unsafe { (*TOTAL).counter(network) };
    let connection = unsafe { (*(&raw mut CONNECTION)).counter(network) };
    let Some((new_total, new_connection)) = budget::debit(*total, *connection, bytes) else {
        exit(2)
    };
    *total = new_total;
    *connection = new_connection;
}

#[unsafe(no_mangle)]
extern "C" fn witchy_console_charge(bytes: usize) {
    charge(false, bytes as u64);
}

#[unsafe(no_mangle)]
extern "C" fn witchy_network_charge(bytes: usize) {
    charge(true, bytes as u64);
}

#[unsafe(no_mangle)]
extern "C" fn witchy_network_remaining() -> u64 {
    unsafe { (*TOTAL).network.min(CONNECTION.network) }
}

static mut FILESYSTEM: u64 = u64::MAX;
static mut FILESYSTEM_TOTAL: *mut u64 = &raw mut FILESYSTEM;
static mut FILESYSTEM_CONNECTION: u64 = u64::MAX;

pub(super) fn share_filesystem() -> bool {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            size: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
    }
    #[cfg(target_os = "macos")]
    const SHARED_ANON: c_int = 0x1001;
    #[cfg(target_os = "linux")]
    const SHARED_ANON: c_int = 0x21;
    let mapped = unsafe {
        mmap(
            core::ptr::null_mut(),
            size_of::<u64>(),
            3,
            SHARED_ANON,
            -1,
            0,
        )
    };
    if mapped.addr() == usize::MAX {
        return false;
    }
    let total = mapped.cast::<u64>();
    unsafe {
        total.write(*FILESYSTEM_TOTAL);
        FILESYSTEM_TOTAL = total;
    }
    true
}

pub(super) fn set_connection_filesystem(limit: u64) {
    unsafe {
        FILESYSTEM_CONNECTION = limit;
    }
}

pub(super) fn filesystem_total() -> u64 {
    unsafe { *FILESYSTEM_TOTAL }
}

pub(super) fn charge_snapshot(bytes: u64) -> bool {
    let Some((left, _)) = budget::debit(filesystem_total(), u64::MAX, bytes) else {
        return false;
    };
    unsafe {
        *FILESYSTEM_TOTAL = left;
    }
    true
}

pub(super) fn snapshot_read_length(requested: usize) -> usize {
    unsafe extern "C" {
        safe fn exit(status: core::ffi::c_int) -> !;
    }
    if requested == 0 {
        return 0;
    }
    let remaining = unsafe { FILESYSTEM_CONNECTION };
    if remaining == 0 {
        exit(2)
    }
    let amount = requested.min(remaining as usize);
    if remaining != u64::MAX {
        unsafe {
            FILESYSTEM_CONNECTION -= amount as u64;
        }
    }
    amount
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_filesystem_remaining() -> u64 {
    unsafe { (*FILESYSTEM_TOTAL).min(FILESYSTEM_CONNECTION) }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_filesystem_charge(bytes: u64) {
    unsafe extern "C" {
        safe fn exit(status: core::ffi::c_int) -> !;
    }
    let Some((left, connection)) =
        budget::debit(filesystem_total(), unsafe { FILESYSTEM_CONNECTION }, bytes)
    else {
        exit(2)
    };
    unsafe {
        *FILESYSTEM_TOTAL = left;
        FILESYSTEM_CONNECTION = connection;
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_filesystem_limit(text: *const c_char) -> bool {
    let bytes = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
    let Some(limit) = budget::parse_limit(bytes) else {
        return false;
    };
    unsafe { *FILESYSTEM_TOTAL = limit };
    true
}

/// Read up to the remaining filesystem quota, returning zero for EOF or
/// an empty request. I/O failure stops the run before it can accept a prefix.
pub(super) unsafe fn bounded_file_read(
    channel: core::ffi::c_int,
    slot: u32,
    bytes: *mut u8,
    count: usize,
) -> u64 {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn witchy_broker_file_read(channel: c_int, slot: u32, out: *mut u8, len: usize) -> c_int;
        fn write(fd: c_int, bytes: *const c_void, length: usize) -> isize;
        safe fn exit(status: c_int) -> !;
    }
    if count == 0 {
        return 0;
    }
    // At the exact limit check for EOF without writing a probe into guest storage.
    let limit = witchy_filesystem_remaining();
    let mut probe = 0;
    let (output, requested) = if limit == 0 {
        (&raw mut probe, 1)
    } else {
        (bytes, count.min(limit as usize))
    };
    let result = unsafe { witchy_broker_file_read(channel, slot, output, requested) };
    if result < 0 {
        let message = b"witchy: native file read failed; the run is stopped\n";
        unsafe {
            write(2, message.as_ptr().cast(), message.len());
            exit(2);
        }
    }
    let read = result as u64;
    if read != 0 {
        witchy_filesystem_charge(read);
    }
    read
}
