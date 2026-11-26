//! Native byte budgets. The launcher serializes workers with waitpid.
//! Each worker gets a private copy of the per-connection budget.

use super::budget;
use super::global::Global;
use core::ptr::NonNull;

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

/// The run-wide counters, until the workers share a page for them.
static LOCAL: Global<Counters> = Global::new(Counters::UNLIMITED);
/// The page the workers share for the run-wide counters, once there is one.
static SHARED: Global<Option<NonNull<Counters>>> = Global::new(None);
static CONNECTION: Global<Counters> = Global::new(Counters::UNLIMITED);

/// The run-wide counters.
fn totals() -> Counters {
    match SHARED.get() {
        // SAFETY: `SHARED` is only ever a mapped page that
        // `witchy_budget_share` wrote the counters to and that is never
        // unmapped. This process is one thread, and the launcher runs one
        // worker at a time.
        Some(shared) => unsafe { shared.read() },
        None => LOCAL.get(),
    }
}

/// One run-wide counter.
fn total(network: bool) -> u64 {
    *totals().counter(network)
}

/// Replaces one run-wide counter and leaves the other as it is.
fn set_total(network: bool, value: u64) {
    match SHARED.get() {
        // SAFETY: as `totals`.
        Some(shared) => unsafe { *(*shared.as_ptr()).counter(network) = value },
        None => {
            let mut local = LOCAL.get();
            *local.counter(network) = value;
            LOCAL.set(local);
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_budget_share() -> bool {
    // A shared anonymous page carries the run-wide counters across workers.
    let Some(mapped) = super::sys::map_shared(size_of::<Counters>()) else {
        return false;
    };
    let shared = mapped.cast::<Counters>();
    // SAFETY: the page is new, writable, aligned for anything and larger
    // than the counters.
    unsafe { shared.write(totals()) };
    SHARED.set(Some(shared));
    true
}

pub(super) fn witchy_budget_configure(
    text: &core::ffi::CStr,
    network: bool,
    per_connection: bool,
) -> bool {
    let Some(limit) = budget::parse_limit(text.to_bytes()) else {
        return false;
    };
    if per_connection {
        let mut connection = CONNECTION.get();
        *connection.counter(network) = limit;
        CONNECTION.set(connection);
    } else {
        set_total(network, limit);
    }
    true
}

fn charge(network: bool, bytes: u64) {
    unsafe extern "C" {
        safe fn exit(status: i32) -> !;
    }
    let mut connection = CONNECTION.get();
    let Some((new_total, new_connection)) =
        budget::debit(total(network), *connection.counter(network), bytes)
    else {
        exit(2)
    };
    set_total(network, new_total);
    *connection.counter(network) = new_connection;
    CONNECTION.set(connection);
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
    total(true).min(CONNECTION.get().network)
}

/// The run-wide filesystem budget, until the workers share a page for it.
static FILESYSTEM: Global<u64> = Global::new(u64::MAX);
/// The page the workers share for the run-wide filesystem budget, once
/// there is one.
static FILESYSTEM_SHARED: Global<Option<NonNull<u64>>> = Global::new(None);
static FILESYSTEM_CONNECTION: Global<u64> = Global::new(u64::MAX);

fn set_filesystem_total(value: u64) {
    match FILESYSTEM_SHARED.get() {
        // SAFETY: as `filesystem_total`.
        Some(shared) => unsafe { shared.write(value) },
        None => FILESYSTEM.set(value),
    }
}

pub(super) fn share_filesystem() -> bool {
    let Some(mapped) = super::sys::map_shared(size_of::<u64>()) else {
        return false;
    };
    let total = mapped.cast::<u64>();
    // SAFETY: the page is new, writable, aligned for anything and larger
    // than the budget.
    unsafe { total.write(filesystem_total()) };
    FILESYSTEM_SHARED.set(Some(total));
    true
}

pub(super) fn set_connection_filesystem(limit: u64) {
    FILESYSTEM_CONNECTION.set(limit);
}

pub(super) fn filesystem_total() -> u64 {
    match FILESYSTEM_SHARED.get() {
        // SAFETY: `FILESYSTEM_SHARED` is only ever a mapped page that
        // `share_filesystem` wrote the budget to and that is never unmapped.
        // This process is one thread, and the launcher runs one worker at a
        // time.
        Some(shared) => unsafe { shared.read() },
        None => FILESYSTEM.get(),
    }
}

pub(super) fn charge_snapshot(bytes: u64) -> bool {
    let Some((left, _)) = budget::debit(filesystem_total(), u64::MAX, bytes) else {
        return false;
    };
    set_filesystem_total(left);
    true
}

pub(super) fn snapshot_read_length(requested: usize) -> usize {
    unsafe extern "C" {
        safe fn exit(status: core::ffi::c_int) -> !;
    }
    if requested == 0 {
        return 0;
    }
    let remaining = FILESYSTEM_CONNECTION.get();
    if remaining == 0 {
        exit(2)
    }
    let amount = requested.min(remaining as usize);
    if remaining != u64::MAX {
        FILESYSTEM_CONNECTION.set(remaining - amount as u64);
    }
    amount
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_filesystem_remaining() -> u64 {
    filesystem_total().min(FILESYSTEM_CONNECTION.get())
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_filesystem_charge(bytes: u64) {
    unsafe extern "C" {
        safe fn exit(status: core::ffi::c_int) -> !;
    }
    let Some((left, connection)) =
        budget::debit(filesystem_total(), FILESYSTEM_CONNECTION.get(), bytes)
    else {
        exit(2)
    };
    set_filesystem_total(left);
    FILESYSTEM_CONNECTION.set(connection);
}

pub(super) fn witchy_filesystem_limit(text: &core::ffi::CStr) -> bool {
    let Some(limit) = budget::parse_limit(text.to_bytes()) else {
        return false;
    };
    set_filesystem_total(limit);
    true
}

/// Read up to the remaining filesystem quota into `bytes`, returning zero for
/// EOF or an empty request. `None` is a request with nowhere to put its
/// bytes. I/O failure stops the run before it can accept a prefix.
pub(super) fn bounded_file_read(
    channel: core::ffi::c_int,
    slot: u32,
    bytes: Option<&mut [u8]>,
) -> u64 {
    use core::ffi::c_int;
    unsafe extern "C" {
        safe fn exit(status: c_int) -> !;
    }
    if bytes.as_ref().is_some_and(|bytes| bytes.is_empty()) {
        return 0;
    }
    // At the exact limit check for EOF without writing a probe into guest storage.
    let limit = witchy_filesystem_remaining();
    let mut probe = [0];
    let stop = || -> ! {
        super::sys::write(2, b"witchy: native file read failed; the run is stopped\n");
        exit(2)
    };
    let output = if limit == 0 {
        &mut probe[..]
    } else if let Some(bytes) = bytes {
        let count = bytes.len().min(limit as usize);
        &mut bytes[..count]
    } else {
        stop()
    };
    let result = super::broker_transport::witchy_broker_file_read(channel, slot, output);
    if result < 0 {
        stop();
    }
    let read = result as u64;
    if read != 0 {
        witchy_filesystem_charge(read);
    }
    read
}
