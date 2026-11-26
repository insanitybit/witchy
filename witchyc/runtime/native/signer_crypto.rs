//! Native ML-DSA signing through the pinned AWS-LC provider.

use super::global::{Global, Table};
use core::ffi::{CStr, c_char, c_int};

use super::{
    digest::fips_mode,
    signer_binding::{StoreSource, parse, witchy_native_secret_slot},
    sys,
    tls_signer::{self, Prehash, PrivateKey},
};
use provider::{Hmac, Seed};

/// The provider and C library calls made on secret bytes, behind signatures
/// that hold each call's contract.
mod provider {
    use core::{
        ffi::{CStr, c_char, c_int, c_void},
        ptr::{NonNull, null, null_mut},
        slice,
    };

    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn free(bytes: *mut c_void);
        fn read(fd: c_int, bytes: *mut c_void, len: usize) -> isize;
        fn getenv(name: *const c_char) -> *const c_char;
        fn unsetenv(name: *const c_char) -> c_int;
        fn strdup(s: *const c_char) -> *mut c_char;
        fn strnlen(text: *const c_char, maximum: usize) -> usize;
        #[link_name = "aws_lc_fips_0_14_4_OPENSSL_cleanse"]
        fn cleanse(bytes: *mut c_void, len: usize);
        #[link_name = "aws_lc_fips_0_14_4_HMAC_CTX_new"]
        fn hmac_new() -> *mut c_void;
        #[link_name = "aws_lc_fips_0_14_4_HMAC_CTX_free"]
        fn hmac_free(ctx: *mut c_void);
        #[link_name = "aws_lc_fips_0_14_4_HMAC_Init_ex"]
        fn hmac_init(
            ctx: *mut c_void,
            key: *const c_void,
            len: usize,
            digest: *const c_void,
            engine: *mut c_void,
        ) -> c_int;
        #[link_name = "aws_lc_fips_0_14_4_HMAC_Update"]
        fn hmac_update(ctx: *mut c_void, bytes: *const u8, len: usize) -> c_int;
        #[link_name = "aws_lc_fips_0_14_4_HMAC_Final"]
        fn hmac_final(ctx: *mut c_void, bytes: *mut u8, len: *mut u32) -> c_int;
        #[link_name = "aws_lc_fips_0_14_4_EVP_sha256"]
        fn sha256() -> *const c_void;
    }

    /// A keyed HMAC-SHA-256 computation the provider holds. `new` is the only
    /// way to get one and none is ever freed, so every copy names a live
    /// context for the life of the process.
    #[derive(Clone, Copy)]
    pub(super) struct Hmac(NonNull<c_void>);

    impl Hmac {
        /// A computation under `key`, with nothing fed to it yet.
        pub(super) fn new(key: &[u8]) -> Option<Self> {
            // SAFETY: the context is checked before it is used, and freeing a
            // null one does nothing. The provider reads `key` for its length
            // and keeps its own copy.
            unsafe {
                let context = hmac_new();
                if context.is_null()
                    || hmac_init(context, key.as_ptr().cast(), key.len(), sha256(), null_mut()) != 1
                {
                    hmac_free(context);
                    return None;
                }
                NonNull::new(context).map(Self)
            }
        }

        pub(super) fn update(self, bytes: &[u8]) -> bool {
            // SAFETY: the context is live, and `bytes` is read for its length.
            unsafe { hmac_update(self.0.as_ptr(), bytes.as_ptr(), bytes.len()) == 1 }
        }

        /// Writes the code of what was fed. The number of bytes written, or
        /// `None` when the provider refuses.
        pub(super) fn finish(self, out: &mut [u8; 32]) -> Option<u32> {
            let mut length = 0;
            // SAFETY: the context is live and was keyed for SHA-256, so the
            // provider writes that hash's 32 bytes, which `out` has.
            let done = unsafe { hmac_final(self.0.as_ptr(), out.as_mut_ptr(), &mut length) } == 1;
            done.then_some(length)
        }

        /// Starts the computation again under the same key.
        pub(super) fn restart(self) -> bool {
            // SAFETY: the context is live. A null key and a null hash ask the
            // provider to keep the ones it has.
            unsafe { hmac_init(self.0.as_ptr(), null(), 0, null(), null_mut()) == 1 }
        }
    }

    /// Room for a secret while it is loaded. The first `length` of the
    /// `capacity` allocated bytes hold values. Unless it is retained, all of
    /// it is cleansed and freed when this is dropped.
    pub(super) struct Seed {
        bytes: NonNull<u8>,
        capacity: usize,
        length: usize,
    }

    impl Seed {
        pub(super) fn new(capacity: usize) -> Option<Self> {
            // SAFETY: `malloc` takes any size, and gives null or that many bytes.
            let bytes = NonNull::new(unsafe { malloc(capacity) }.cast())?;
            Some(Self { bytes, capacity, length: 0 })
        }

        pub(super) fn capacity(&self) -> usize {
            self.capacity
        }

        /// The number of bytes that hold values.
        pub(super) fn len(&self) -> usize {
            self.length
        }

        pub(super) fn as_slice(&self) -> &[u8] {
            // SAFETY: the first `length` bytes of the allocation hold values,
            // and nothing writes them while `self` is borrowed.
            unsafe { slice::from_raw_parts(self.bytes.as_ptr(), self.length) }
        }

        /// Reads from `fd` into the bytes that hold no value yet, as `read`
        /// does, and counts what arrived.
        pub(super) fn read(&mut self, fd: c_int) -> isize {
            // SAFETY: `length` is at most `capacity`, so the call is given the
            // unused end of the allocation and its size. It writes no more,
            // and a positive result is the number of bytes it wrote.
            let count = unsafe {
                read(fd, self.bytes.as_ptr().add(self.length).cast(), self.capacity - self.length)
            };
            if count > 0 {
                self.length += count as usize;
            }
            count
        }

        /// Makes `bytes` the contents. False, with nothing changed, unless
        /// they are shorter than the capacity.
        pub(super) fn fill(&mut self, bytes: &[u8]) -> bool {
            if bytes.len() >= self.capacity {
                return false;
            }
            // SAFETY: the allocation has room for `bytes`, and is not part of
            // them because `self` is borrowed alone.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.bytes.as_ptr(), bytes.len()) };
            self.length = bytes.len();
            true
        }

        /// Keeps the contents for the life of the process.
        pub(super) fn retain(self) -> &'static [u8] {
            // SAFETY: as `as_slice`. Forgetting `self` leaves the allocation
            // with no owner, so nothing frees or writes it again.
            let kept = unsafe { slice::from_raw_parts(self.bytes.as_ptr(), self.length) };
            core::mem::forget(self);
            kept
        }
    }

    impl Drop for Seed {
        fn drop(&mut self) {
            // SAFETY: the allocation is `capacity` bytes from `malloc`, and
            // this is its owner's end.
            unsafe {
                cleanse(self.bytes.as_ptr().cast(), self.capacity);
                free(self.bytes.as_ptr().cast());
            }
        }
    }

    /// Moves the value of the environment variable `name` into `seed`, and
    /// takes the variable out of the environment with its value cleansed. The
    /// result is a copy of the name that is never freed. `None` when there is
    /// no such variable, its value does not fit, or the C library refuses.
    pub(super) fn take_environment(name: &CStr, seed: &mut Seed) -> Option<&'static CStr> {
        // SAFETY: `name` is a C string. `getenv` gives null or a C string that
        // stays where it is until the environment changes, and `unsetenv`
        // below is the first thing to change it; the borrow of the value has
        // ended by then, and the cleanse writes only the bytes `strnlen`
        // counted. `strdup` gives null or a C string of its own.
        unsafe {
            let value = getenv(name.as_ptr());
            if value.is_null() {
                return None;
            }
            let length = strnlen(value, seed.capacity);
            if length >= seed.capacity {
                return None;
            }
            let kept = strdup(name.as_ptr());
            if kept.is_null() {
                return None;
            }
            if !seed.fill(slice::from_raw_parts(value.cast(), length)) {
                return None;
            }
            cleanse(value.cast_mut().cast(), length);
            if unsetenv(name.as_ptr()) != 0 {
                return None;
            }
            Some(CStr::from_ptr(kept))
        }
    }
}

fn signer(handle: u64) -> (PrivateKey, [u8; 44], usize) {
    let slot = slot(witchy_native_secret_slot(handle, 2));
    match slot.key.get() {
        Some(key) if slot.key_kind.get() == 3 => (key, slot.domain.get(), slot.domain_len.get()),
        _ => trap(),
    }
}

/// The guest provides writable storage for the public key.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_signer_public(handle: u64, output: u64, capacity: u64) -> u64 {
    let (key, _, _) = signer(handle);
    let mut bytes = [0u8; 1952];
    if capacity != bytes.len() as u64 {
        return 2;
    }
    let length = match key.raw_public(&mut bytes) {
        Some(length) if length as u64 == capacity => length,
        _ => return 4,
    };
    // SAFETY: the guest passes `capacity` writable bytes at `output`, and
    // `length` is `capacity` here.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), output as *mut u8, length) };
    0
}

/// The guest provides readable message and writable signature spans.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_signer_sign(
    handle: u64,
    message: u64,
    length: u64,
    output: u64,
    capacity: u64,
) -> u64 {
    let (key, domain, domain_len) = signer(handle);
    let mut signature = [0u8; 3309];
    if capacity != signature.len() as u64 {
        return 2;
    }
    if length > 1024 * 1024 {
        return 3;
    }
    if length < domain_len as u64 {
        return 1;
    }
    let message = if length == 0 {
        &[]
    } else {
        // SAFETY: the guest passes `length` readable bytes at `message`, at
        // most 1 MiB here. The signature goes to a local, and to the guest
        // only after the last use of this borrow.
        unsafe { core::slice::from_raw_parts(message as *const u8, length as usize) }
    };
    if (0..domain_len).any(|offset| message.get(offset) != domain.get(offset)) {
        return 1;
    }
    let size = match key.sign(Prehash::None, message, &mut signature) {
        Some(size) if size as u64 == capacity => size,
        _ => return 4,
    };
    // SAFETY: the guest passes `capacity` writable bytes at `output`, and
    // `size` is `capacity` here.
    unsafe { core::ptr::copy_nonoverlapping(signature.as_ptr(), output as *mut u8, size) };
    0
}

// Serialized guest calls exclusively access one slot for the operation.
#[unsafe(no_mangle)]
extern "C" fn witchy_secret_seal(handle: u64, value: u64, mode: u64) -> u64 {
    if mode == 0 && matches!(handle, 8193 | 8194) {
        return handle - 8192;
    }
    let state = slot(witchy_native_secret_slot(handle, 1));
    let Some(seal) = state.seal.get() else { trap() };
    match mode {
        0 => 0,
        1 => {
            let Ok(byte) = u8::try_from(value) else {
                trap()
            };
            let length = state.sealed.get();
            if length >= 1024 * 1024 || !seal.update(&[byte]) {
                trap();
            }
            state.sealed.set(length + 1);
            0
        }
        2 => {
            let mut digest = [0; 32];
            if fips_mode() == 0
                || seal.finish(&mut digest).map(|length| length as usize) != Some(digest.len())
                || !seal.restart()
            {
                trap();
            }
            state.digest.set(digest);
            state.sealed.set(0);
            0
        }
        3 => state
            .digest
            .get()
            .get(value as usize)
            .map_or(256, |&byte| u64::from(byte)),
        _ => trap(),
    }
}

pub(super) fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

/// One named secret of the program, or one issued from the store. Each field
/// is read and written whole, by the guest thread alone.
pub(super) struct SignerSlot {
    pub(super) key: Global<Option<PrivateKey>>,
    pub(super) key_kind: Global<u32>,
    /// The bytes a `reveal` reads. They are never freed or written again.
    pub(super) revealed: Global<Option<&'static [u8]>>,
    seal: Global<Option<Hmac>>,
    /// The number of bytes fed to `seal` since its last code.
    sealed: Global<usize>,
    /// The last code `seal` gave.
    digest: Global<[u8; 32]>,
    device: Global<u64>,
    inode: Global<u64>,
    /// The environment variable the secret came from, in a copy that is never freed.
    environment: Global<Option<&'static CStr>>,
    domain: Global<[u8; 44]>,
    domain_len: Global<usize>,
}

impl SignerSlot {
    const EMPTY: Self = Self {
        key: Global::new(None),
        key_kind: Global::new(0),
        revealed: Global::new(None),
        seal: Global::new(None),
        sealed: Global::new(0),
        digest: Global::new([0; 32]),
        device: Global::new(0),
        inode: Global::new(0),
        environment: Global::new(None),
        domain: Global::new([0; 44]),
        domain_len: Global::new(0),
    };

    fn bound(&self) -> bool {
        self.key.get().is_some() || self.revealed.get().is_some() || self.seal.get().is_some()
    }

    /// Whether the slot holds a secret that was read from a descriptor.
    fn from_descriptor(&self) -> bool {
        self.environment.get().is_none() && self.bound()
    }
}

static SIGNER_SLOTS: [SignerSlot; 256] = [const { SignerSlot::EMPTY }; 256];

static SECRET_OBJECTS: Table<(u64, u64), 64> = Table::new((0, 0));

#[unsafe(no_mangle)]
extern "C" fn witchy_secret_add_object(device: u64, inode: u64) -> bool {
    SECRET_OBJECTS.push((device, inode))
}

fn secret_identities(mut visit: impl FnMut(u64, u64)) {
    for index in 0..128 {
        let slot = slot(index);
        if slot.from_descriptor() {
            visit(slot.device.get(), slot.inode.get());
        }
    }
    for (device, inode) in SECRET_OBJECTS.iter() {
        visit(device, inode);
    }
}

/// Writes the identity of each secret source to the same place of `devices`
/// and `inodes`, for as many as both have room for. The number written.
pub(super) fn witchy_secret_identities(devices: &mut [u64], inodes: &mut [u64]) -> u32 {
    let mut places = devices.iter_mut().zip(inodes.iter_mut());
    let mut count = 0;
    secret_identities(|device, inode| {
        if let Some((device_out, inode_out)) = places.next() {
            *device_out = device;
            *inode_out = inode;
            count += 1;
        }
    });
    count
}

#[unsafe(no_mangle)]
extern "C" fn witchy_secret_source(device: u64, inode: u64) -> bool {
    let mut found = false;
    secret_identities(|candidate_device, candidate_inode| {
        found |= candidate_device == device && candidate_inode == inode;
    });
    found
}

pub(super) fn slot(index: usize) -> &'static SignerSlot {
    SIGNER_SLOTS.get(index).unwrap_or_else(|| trap())
}

pub(super) fn witchy_secret_uses_env(name: &CStr) -> bool {
    (0..128).any(|index| slot(index).environment.get() == Some(name))
}

unsafe extern "C" {
    // Each entry is a tag followed by a NUL-terminated name. A zero tag ends the table.
    static witchy_secret_slots: u8;
}

/// The entries of the program's table of named secrets that are yet to be read.
struct SlotNames(*const u8);

/// The tag and the name of each named secret of the program, in slot order.
fn slot_names() -> SlotNames {
    SlotNames(&raw const witchy_secret_slots)
}

impl Iterator for SlotNames {
    type Item = (u8, &'static CStr);

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the emitter writes the table in the form described where it
        // is declared, and it is never written afterwards. The cursor starts
        // at the first tag and moves only past a whole entry, never past the
        // zero tag, so it is always at a tag of the table.
        unsafe {
            let kind = self.0.read();
            if kind == 0 {
                return None;
            }
            let name = CStr::from_ptr(self.0.add(1).cast());
            self.0 = self.0.add(1 + name.to_bytes_with_nul().len());
            Some((kind, name))
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_signers_bound() -> bool {
    let mut names = slot_names();
    for index in 0..64 {
        let Some((kind, _)) = names.next() else {
            return true;
        };
        let slot = slot(index);
        let bound = match kind {
            1 => slot.revealed.get().is_some(),
            2 => slot.seal.get().is_some(),
            3 | 4 => slot.key_kind.get() == u32::from(kind) && slot.key.get().is_some(),
            _ => false,
        };
        if !bound {
            return false;
        }
    }
    names.next().is_none()
}

/// The name the guest is spelling for the store, a byte for each call.
static STORE_NAME: Global<[u8; 65]> = Global::new([0; 65]);
static STORE_NAME_LEN: Global<usize> = Global::new(0);
/// The slot the next secret issued from the store takes.
static STORE_NEXT: Global<usize> = Global::new(128);

// Guest calls cannot suspend or reenter while manipulating this request.
#[unsafe(no_mangle)]
extern "C" fn witchy_secret_store(handle: u64, value: u64, mode: u64) -> u64 {
    if handle != 65536 {
        trap();
    }
    match mode {
        0 => {
            let Ok(byte) = u8::try_from(value) else {
                trap()
            };
            let mut name = STORE_NAME.get();
            let len = STORE_NAME_LEN.get();
            if let Some(out) = name.get_mut(len) {
                *out = byte;
                STORE_NAME.set(name);
                STORE_NAME_LEN.set(len + 1);
            }
            return 0;
        }
        3 => return 0,
        1 | 2 => {}
        _ => trap(),
    }
    let len = STORE_NAME_LEN.get();
    STORE_NAME_LEN.set(0);
    let name = STORE_NAME.get();
    let Some(name) = name.get(..len) else {
        trap()
    };
    let source = match super::signer_binding::store_lookup(name, mode == 1) {
        -1 => return 8193,
        -2 => return 8194,
        index @ 0..=63 => index as usize + 64,
        _ => trap(),
    };
    let next = STORE_NEXT.get();
    if next >= 256 || fips_mode() == 0 {
        trap();
    }
    let secret = slot(source).revealed.get();
    let issued = slot(next);
    let Some(seal) = Hmac::new(secret.unwrap_or(&[])) else {
        trap()
    };
    issued.seal.set(Some(seal));
    if mode == 1 {
        issued.revealed.set(secret);
    }
    STORE_NEXT.set(next + 1);
    16 * (next + 1) as u64 + u64::from(mode == 2)
}

static SEED_DEADLINE: Global<u64> = Global::new(0);

fn descriptor_number(bytes: &[u8]) -> Option<c_int> {
    if bytes.is_empty() || bytes.first() == Some(&b'0') {
        return None;
    }
    let fd = bytes.iter().try_fold(0, |fd, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        let value = fd * 10 + c_int::from(byte - b'0');
        (value <= 1023).then_some(value)
    })?;
    (fd >= 3).then_some(fd)
}

/// Reads the whole of the secret behind the descriptor that `descriptor`
/// numbers into `seed`, closes it, and records its identity in `slot`. False
/// when it is not a secret this launch may take, or it does not end in time
/// and within the capacity of `seed`, which must hold nothing yet.
fn read_secret_descriptor(slot: &SignerSlot, descriptor: &[u8], seed: &mut Seed) -> bool {
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
    }
    #[cfg(target_os = "macos")]
    const NONBLOCK: c_int = 4;
    #[cfg(target_os = "linux")]
    const NONBLOCK: c_int = 0o4000;
    const INTERRUPTED: c_int = 4;
    #[cfg(target_os = "macos")]
    const AGAIN: c_int = 35;
    #[cfg(target_os = "linux")]
    const AGAIN: c_int = 11;
    let Some(fd) = descriptor_number(descriptor) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    let identity = super::seatbelt::stat(fd);
    #[cfg(target_os = "linux")]
    let identity = super::linux_stat::stat(fd);
    let Some(identity) = identity else {
        return false;
    };
    for index in 0..128 {
        let bound = self::slot(index);
        if bound.from_descriptor()
            && bound.device.get() == identity.device as u64
            && bound.inode.get() == identity.inode
        {
            return false;
        }
    }
    let flags = sys::status_flags(fd);
    if flags < 0 || sys::set_status_flags(fd, flags | NONBLOCK) < 0 {
        return false;
    }
    let Some(now) = super::deadline::monotonic_ms() else {
        sys::set_status_flags(fd, flags);
        close(fd);
        return false;
    };
    let deadline = {
        if SEED_DEADLINE.get() == 0 {
            SEED_DEADLINE.set(now.saturating_add(1000));
        }
        SEED_DEADLINE.get()
    };
    let mut eof = false;
    while seed.len() < seed.capacity() {
        let Some(now) = super::deadline::monotonic_ms() else {
            break;
        };
        if now >= deadline {
            break;
        }
        let timeout = (deadline - now).min(c_int::MAX as u64) as c_int;
        let mut ready = [sys::PollFd {
            fd,
            events: 1,
            returned: 0,
        }];
        let result = sys::poll(&mut ready, timeout);
        if result < 0 && super::errno::get() == INTERRUPTED {
            continue;
        }
        if result != 1 {
            break;
        }
        let count = seed.read(fd);
        if count < 0 && matches!(super::errno::get(), INTERRUPTED | AGAIN) {
            continue;
        }
        if count <= 0 {
            eof = count == 0;
            break;
        }
    }
    let restored = sys::set_status_flags(fd, flags) == 0;
    close(fd);
    if !restored || !eof {
        return false;
    }
    slot.device.set(identity.device as u64);
    slot.inode.set(identity.inode);
    true
}

/// Loads the secret that `source` names in the way `kind` says into `seed`,
/// which must hold nothing yet, and records in `slot` where it came from. The
/// secret's length.
fn load_seed(slot: &SignerSlot, seed: &mut Seed, source: &CStr, kind: StoreSource) -> Option<usize> {
    match kind {
        StoreSource::Descriptor => {
            if !read_secret_descriptor(slot, source.to_bytes(), seed) {
                return None;
            }
        }
        StoreSource::Literal => {
            if !seed.fill(source.to_bytes()) {
                return None;
            }
        }
        StoreSource::Environment => {
            if witchy_secret_uses_env(source) {
                return None;
            }
            // The slot keeps the name for the life of the process, and the caller's
            // text may be a buffer it reuses, so the slot holds its own copy.
            let kept = provider::take_environment(source, seed)?;
            slot.environment.set(Some(kept));
        }
    }
    Some(seed.len())
}

/// Loads the store's secret `index`. `name` is the variable an
/// `Environment` secret is read from; `source` is the text or the descriptor
/// number of the other kinds.
pub(super) fn load_store_secret(
    index: usize,
    name: &CStr,
    source: &CStr,
    kind: StoreSource,
) -> bool {
    if index >= 64 {
        return false;
    }
    let Some(mut seed) = Seed::new(65_537) else {
        return false;
    };
    let slot = slot(64 + index);
    let input = if kind == StoreSource::Environment {
        name
    } else {
        source
    };
    if load_seed(slot, &mut seed, input, kind).is_none() {
        return false;
    }
    slot.revealed.set(Some(seed.retain()));
    true
}

/// `text` is a C string that stays as it is through the call.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_signer(text: *const c_char) -> bool {
    // SAFETY: the caller's promise.
    let text = unsafe { CStr::from_ptr(text) };
    let bytes = text.to_bytes();
    let Some(binding) = parse(bytes) else {
        return false;
    };
    let selected = slot_names().take(64).position(|(kind, name)| {
        u32::from(kind) == binding.kind && bytes.get(..binding.name_len) == Some(name.to_bytes())
    });
    let Some(index) = selected else { return false };
    let slot = slot(index);
    if slot.bound() || (binding.kind != 1 && fips_mode() == 0) {
        return false;
    }
    let capacity = match binding.kind {
        3 => 33,
        4 => tls_signer::MAX_KEY_BYTES + 1,
        _ => 65_537,
    };
    let Some(mut seed) = Seed::new(capacity) else {
        return false;
    };
    // The source starts at or before the terminating NUL.
    let source = text.to_bytes_with_nul().get(binding.source_start..);
    let Some(source) = source.and_then(|source| CStr::from_bytes_with_nul(source).ok()) else {
        return false;
    };
    let kind = if binding.environment {
        StoreSource::Environment
    } else {
        StoreSource::Descriptor
    };
    let Some(length) = load_seed(slot, &mut seed, source, kind) else {
        return false;
    };
    match binding.kind {
        1 => slot.revealed.set(Some(seed.retain())),
        2 => {
            let Some(seal) = Hmac::new(seed.as_slice()) else {
                return false;
            };
            slot.seal.set(Some(seal));
        }
        3 if length == 32 => {
            let Ok(seed) = <&[u8; 32]>::try_from(seed.as_slice()) else {
                return false;
            };
            let Some(key) = PrivateKey::mldsa65(seed) else {
                return false;
            };
            let role = bytes.iter().skip(binding.role_start).take(binding.role_len);
            let mut domain = slot.domain.get();
            for (out, &byte) in domain.iter_mut().zip(b"witchy/v2/".iter().chain(role)) {
                *out = byte;
            }
            slot.domain.set(domain);
            slot.domain_len.set(b"witchy/v2/".len() + binding.role_len + 1);
            slot.key.set(Some(key));
            slot.key_kind.set(3);
        }
        4 if (1..=tls_signer::MAX_KEY_BYTES).contains(&length) => {
            let Some(key) = PrivateKey::from_pkcs8(seed.as_slice()) else {
                return false;
            };
            slot.key.set(Some(key));
            slot.key_kind.set(4);
        }
        _ => return false,
    }
    true
}
