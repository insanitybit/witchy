//! Native ML-DSA signing through the pinned AWS-LC provider.

use super::global::{Global, Table};
use core::{
    ffi::{c_int, c_void},
    ptr::{null, null_mut},
};

use super::{
    pkix::ffi::{free_digest_context, new_digest_context},
    signer_binding::witchy_native_secret_slot,
    tls_signer::{self, sign, sign_init},
};

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_get_raw_public_key"]
    fn public_key(key: *const c_void, out: *mut u8, len: *mut usize) -> c_int;
}

fn signer(handle: u64) -> (*mut c_void, *const u8, usize) {
    let index = witchy_native_secret_slot(handle, 2);
    let slot = unsafe { &*slot_ptr(index) };
    if slot.key_kind != 3 || slot.key.is_null() {
        trap();
    }
    (slot.key, slot.domain.as_ptr(), slot.domain_len)
}

/// The guest provides writable storage for the public key.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_signer_public(handle: u64, output: u64, capacity: u64) -> u64 {
    let (key, _, _) = signer(handle);
    let mut bytes = [0u8; 1952];
    let mut length = bytes.len();
    if capacity != length as u64 {
        return 2;
    }
    if unsafe { public_key(key, bytes.as_mut_ptr(), &mut length) } != 1 || length as u64 != capacity
    {
        return 4;
    }
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
    let mut size = signature.len();
    if capacity != size as u64 {
        return 2;
    }
    if length > 1024 * 1024 {
        return 3;
    }
    if length < domain_len as u64 {
        return 1;
    }
    let input = message as *const u8;
    for offset in 0..domain_len {
        if unsafe { input.add(offset).read() != domain.add(offset).read() } {
            return 1;
        }
    }
    let ctx = unsafe { new_digest_context() };
    if ctx.is_null() {
        return 4;
    }
    let ok = unsafe {
        let ok = sign_init(ctx, null_mut(), null(), null_mut(), key.cast()) == 1
            && sign(
                ctx,
                signature.as_mut_ptr(),
                &mut size,
                input,
                length as usize,
            ) == 1
            && size as u64 == capacity;
        free_digest_context(ctx);
        ok
    };
    if !ok {
        return 4;
    }
    unsafe { core::ptr::copy_nonoverlapping(signature.as_ptr(), output as *mut u8, size) };
    0
}

#[derive(Clone, Copy)]
struct SealState {
    context: *mut c_void,
    length: usize,
    digest: [u8; 32],
}

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_HMAC_Update"]
    fn hmac_update(ctx: *mut c_void, bytes: *const u8, len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_HMAC_Final"]
    fn hmac_final(ctx: *mut c_void, bytes: *mut u8, len: *mut u32) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_HMAC_Init_ex"]
    fn hmac_init(
        ctx: *mut c_void,
        key: *const c_void,
        len: usize,
        digest: *const c_void,
        engine: *mut c_void,
    ) -> c_int;
}

// Serialized guest calls exclusively access one slot for the operation.
#[unsafe(no_mangle)]
extern "C" fn witchy_secret_seal(handle: u64, value: u64, mode: u64) -> u64 {
    if mode == 0 && matches!(handle, 8193 | 8194) {
        return handle - 8192;
    }
    let index = witchy_native_secret_slot(handle, 1);
    let state = unsafe { &mut (*slot_ptr(index)).seal };
    let ctx = state.context;
    if ctx.is_null() {
        trap();
    }
    match mode {
        0 => 0,
        1 => {
            let Ok(byte) = u8::try_from(value) else {
                trap()
            };
            if state.length >= 1024 * 1024 || unsafe { hmac_update(ctx, &byte, 1) } != 1 {
                trap();
            }
            state.length += 1;
            0
        }
        2 => {
            let mut length = 0;
            if unsafe { super::digest::fips_mode() } == 0
                || unsafe { hmac_final(ctx, state.digest.as_mut_ptr(), &mut length) } != 1
                || length as usize != state.digest.len()
                || unsafe { hmac_init(ctx, null(), 0, null(), null_mut()) } != 1
            {
                trap();
            }
            state.length = 0;
            0
        }
        3 => state
            .digest
            .get(value as usize)
            .map_or(256, |&byte| u64::from(byte)),
        _ => trap(),
    }
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[derive(Clone, Copy)]
pub(super) struct SignerSlot {
    pub(super) key: *mut c_void,
    pub(super) key_kind: u32,
    pub(super) revealed: *mut u8,
    pub(super) length: usize,
    seal: SealState,
    device: u64,
    inode: u64,
    environment: *const u8,
    domain: [u8; 44],
    domain_len: usize,
}

static mut SIGNER_SLOTS: [SignerSlot; 256] = [SignerSlot {
    key: null_mut(),
    key_kind: 0,
    revealed: null_mut(),
    length: 0,
    seal: SealState {
        context: null_mut(),
        length: 0,
        digest: [0; 32],
    },
    device: 0,
    inode: 0,
    environment: null(),
    domain: [0; 44],
    domain_len: 0,
}; 256];

static SECRET_OBJECTS: Table<(u64, u64), 64> = Table::new((0, 0));

#[unsafe(no_mangle)]
extern "C" fn witchy_secret_add_object(device: u64, inode: u64) -> bool {
    SECRET_OBJECTS.push((device, inode))
}

fn secret_identities(mut visit: impl FnMut(u64, u64)) {
    for index in 0..128 {
        let slot = unsafe { &*slot_ptr(index) };
        if slot.environment.is_null()
            && (!slot.key.is_null() || !slot.revealed.is_null() || !slot.seal.context.is_null())
        {
            visit(slot.device as u64, slot.inode);
        }
    }
    for (device, inode) in SECRET_OBJECTS.iter() {
        visit(device, inode);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_secret_identities(
    devices: *mut u64,
    inodes: *mut u64,
    capacity: u32,
) -> u32 {
    let mut count = 0;
    secret_identities(|device, inode| {
        if count < capacity {
            unsafe {
                devices.add(count as usize).write(device);
                inodes.add(count as usize).write(inode);
            }
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

pub(super) fn slot_ptr(index: usize) -> *mut SignerSlot {
    if index >= 256 {
        trap();
    }
    // Calls on the guest thread are serialized, including startup loading.
    unsafe { (&raw mut SIGNER_SLOTS).cast::<SignerSlot>().add(index) }
}

/// Startup caller provides a NUL-terminated environment name.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_secret_uses_env(name: *const core::ffi::c_char) -> bool {
    use core::ffi::CStr;
    let requested = unsafe { CStr::from_ptr(name) }.to_bytes();
    for index in 0..128 {
        let stored = unsafe { (*slot_ptr(index)).environment };
        if !stored.is_null() && unsafe { CStr::from_ptr(stored.cast()) }.to_bytes() == requested {
            return true;
        }
    }
    false
}

#[unsafe(no_mangle)]
extern "C" fn witchy_signers_bound() -> bool {
    use core::ffi::CStr;
    let mut entry = &raw const witchy_secret_slots;
    for index in 0..64 {
        let kind = unsafe { entry.read() };
        if kind == 0 {
            return true;
        }
        let slot = unsafe { &*slot_ptr(index) };
        let bound = match kind {
            1 => !slot.revealed.is_null(),
            2 => !slot.seal.context.is_null(),
            3 | 4 => slot.key_kind == u32::from(kind) && !slot.key.is_null(),
            _ => false,
        };
        if !bound {
            return false;
        }
        let name = unsafe { CStr::from_ptr(entry.add(1).cast()) };
        entry = unsafe { entry.add(1 + name.to_bytes_with_nul().len()) };
    }
    unsafe { entry.read() == 0 }
}

struct StoreRequest {
    name: [u8; 65],
    len: usize,
    next: usize,
}

static mut STORE_REQUEST: StoreRequest = StoreRequest {
    name: [0; 65],
    len: 0,
    next: 128,
};

#[unsafe(no_mangle)]
extern "C" fn witchy_secret_store(handle: u64, value: u64, mode: u64) -> u64 {
    if handle != 65536 {
        trap();
    }
    // Guest calls cannot suspend or reenter while manipulating this request.
    let request = unsafe { &mut *(&raw mut STORE_REQUEST) };
    match mode {
        0 => {
            let Ok(byte) = u8::try_from(value) else {
                trap()
            };
            if let Some(out) = request.name.get_mut(request.len) {
                *out = byte;
                request.len += 1;
            }
            return 0;
        }
        3 => return 0,
        1 | 2 => {}
        _ => trap(),
    }
    let len = core::mem::take(&mut request.len);
    let Some(name) = request.name.get(..len) else {
        trap()
    };
    let source = match super::signer_binding::store_lookup(name, mode == 1) {
        -1 => return 8193,
        -2 => return 8194,
        index @ 0..=63 => index as usize + 64,
        _ => trap(),
    };
    if request.next >= 256 || unsafe { super::digest::fips_mode() } == 0 {
        trap();
    }
    let (bytes, len) = unsafe {
        let source = &*slot_ptr(source);
        (source.revealed, source.length)
    };
    let issued = unsafe { &mut *slot_ptr(request.next) };
    let context = unsafe { hmac_new() };
    if context.is_null()
        || unsafe { hmac_init(context, bytes.cast(), len, sha256(), null_mut()) } != 1
    {
        trap();
    }
    issued.seal.context = context;
    if mode == 1 {
        issued.revealed = bytes;
        issued.length = len;
    }
    request.next += 1;
    16 * request.next as u64 + u64::from(mode == 2)
}

static SEED_DEADLINE: Global<u64> = Global::new(0);

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    ready: i16,
}

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

/// The caller owns the destination slot and writable seed buffer during launch.
unsafe fn read_secret_descriptor(
    slot: *mut SignerSlot,
    descriptor: *const core::ffi::c_char,
    seed: *mut u8,
    capacity: usize,
    size: *mut usize,
) -> bool {
    use core::ffi::CStr;
    unsafe extern "C" {
        fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
        #[cfg(target_os = "macos")]
        fn poll(ready: *mut PollFd, count: u32, timeout: c_int) -> c_int;
        #[cfg(target_os = "linux")]
        fn poll(ready: *mut PollFd, count: usize, timeout: c_int) -> c_int;
        fn read(fd: c_int, bytes: *mut c_void, len: usize) -> isize;
        safe fn close(fd: c_int) -> c_int;
    }
    const GET_FLAGS: c_int = 3;
    const SET_FLAGS: c_int = 4;
    #[cfg(target_os = "macos")]
    const NONBLOCK: c_int = 4;
    #[cfg(target_os = "linux")]
    const NONBLOCK: c_int = 0o4000;
    const INTERRUPTED: c_int = 4;
    #[cfg(target_os = "macos")]
    const AGAIN: c_int = 35;
    #[cfg(target_os = "linux")]
    const AGAIN: c_int = 11;
    let Some(fd) = descriptor_number(unsafe { CStr::from_ptr(descriptor) }.to_bytes()) else {
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
        let bound = unsafe { &*slot_ptr(index) };
        if bound.environment.is_null()
            && (!bound.key.is_null() || !bound.revealed.is_null() || !bound.seal.context.is_null())
            && bound.device == identity.device as u64
            && bound.inode == identity.inode
        {
            return false;
        }
    }
    let flags = unsafe { fcntl(fd, GET_FLAGS) };
    if flags < 0 || unsafe { fcntl(fd, SET_FLAGS, flags | NONBLOCK) } < 0 {
        return false;
    }
    let Some(now) = super::deadline::monotonic_ms() else {
        unsafe {
            fcntl(fd, SET_FLAGS, flags);
            close(fd);
        }
        return false;
    };
    let deadline = {
        if SEED_DEADLINE.get() == 0 {
            SEED_DEADLINE.set(now.saturating_add(1000));
        }
        SEED_DEADLINE.get()
    };
    let mut length = 0;
    let mut eof = false;
    while length < capacity {
        let Some(now) = super::deadline::monotonic_ms() else {
            break;
        };
        if now >= deadline {
            break;
        }
        let timeout = (deadline - now).min(c_int::MAX as u64) as c_int;
        let mut ready = PollFd {
            fd,
            events: 1,
            ready: 0,
        };
        let result = unsafe { poll(&mut ready, 1, timeout) };
        if result < 0 && super::errno::get() == INTERRUPTED {
            continue;
        }
        if result != 1 {
            break;
        }
        let count = unsafe { read(fd, seed.add(length).cast(), capacity - length) };
        if count < 0 && matches!(super::errno::get(), INTERRUPTED | AGAIN) {
            continue;
        }
        if count <= 0 {
            eof = count == 0;
            break;
        }
        length += count as usize;
    }
    let restored = unsafe { fcntl(fd, SET_FLAGS, flags) } == 0;
    close(fd);
    if !restored || !eof {
        return false;
    }
    unsafe {
        (*slot).device = identity.device as u64;
        (*slot).inode = identity.inode;
        size.write(length);
    }
    true
}

use super::signer_binding::{StoreSource, parse};
use core::ffi::{CStr, c_char};

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(bytes: *mut c_void);
    fn getenv(name: *const c_char) -> *const c_char;
    fn unsetenv(name: *const c_char) -> c_int;
    fn strnlen(text: *const c_char, maximum: usize) -> usize;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_cleanse"]
    fn cleanse(bytes: *mut c_void, len: usize);
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_pqdsa_new_raw_private_key"]
    fn private_key(nid: c_int, bytes: *const u8, len: usize) -> *mut c_void;
    #[link_name = "aws_lc_fips_0_14_4_HMAC_CTX_new"]
    fn hmac_new() -> *mut c_void;
    #[link_name = "aws_lc_fips_0_14_4_HMAC_CTX_free"]
    fn hmac_free(ctx: *mut c_void);
    #[link_name = "aws_lc_fips_0_14_4_EVP_sha256"]
    fn sha256() -> *const c_void;
    // Each entry is a tag followed by a NUL-terminated name. A zero tag ends the table.
    static witchy_secret_slots: u8;
}

struct Seed {
    bytes: *mut u8,
    capacity: usize,
}

impl Seed {
    fn new(capacity: usize) -> Option<Self> {
        let bytes = unsafe { malloc(capacity) }.cast();
        if bytes == null_mut() {
            None
        } else {
            Some(Self { bytes, capacity })
        }
    }

    fn retain(mut self) -> *mut u8 {
        core::mem::replace(&mut self.bytes, null_mut())
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        if !self.bytes.is_null() {
            unsafe {
                cleanse(self.bytes.cast(), self.capacity);
                free(self.bytes.cast());
            }
        }
    }
}

unsafe fn load_seed(
    slot: *mut SignerSlot,
    seed: &Seed,
    source: *const c_char,
    kind: StoreSource,
) -> Option<usize> {
    if kind == StoreSource::Descriptor {
        let mut length = 0;
        return unsafe {
            read_secret_descriptor(slot, source, seed.bytes, seed.capacity, &mut length)
        }
        .then_some(length);
    }
    let environment = kind == StoreSource::Environment;
    if environment && unsafe { witchy_secret_uses_env(source) } {
        return None;
    }
    let value = if environment {
        unsafe { getenv(source) }
    } else {
        source
    };
    if value.is_null() {
        return None;
    }
    let length = unsafe { strnlen(value, seed.capacity) };
    if length >= seed.capacity {
        return None;
    }
    unsafe { core::ptr::copy_nonoverlapping(value.cast(), seed.bytes, length) };
    if environment {
        unsafe { cleanse(value.cast_mut().cast(), length) };
        if unsafe { unsetenv(source) } != 0 {
            return None;
        }
        unsafe { (*slot).environment = source.cast() };
    }
    Some(length)
}

pub(super) unsafe fn load_store_secret(
    index: usize,
    name: *const c_char,
    source: *const c_char,
    kind: StoreSource,
) -> bool {
    if index >= 64 {
        return false;
    }
    let Some(seed) = Seed::new(65_537) else {
        return false;
    };
    let slot = slot_ptr(64 + index);
    let input = if kind == StoreSource::Environment {
        name
    } else {
        source
    };
    let Some(length) = (unsafe { load_seed(slot, &seed, input, kind) }) else {
        return false;
    };
    unsafe {
        (*slot).revealed = seed.retain();
        (*slot).length = length;
    }
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_signer(text: *const c_char) -> bool {
    let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
    let Some(binding) = parse(bytes) else {
        return false;
    };
    let mut entry = &raw const witchy_secret_slots;
    let mut selected = None;
    for index in 0..64 {
        let kind = unsafe { entry.read() };
        if kind == 0 {
            break;
        }
        let name = unsafe { CStr::from_ptr(entry.add(1).cast()) };
        if u32::from(kind) == binding.kind && bytes.get(..binding.name_len) == Some(name.to_bytes())
        {
            selected = Some(slot_ptr(index));
            break;
        }
        entry = unsafe { entry.add(1 + name.to_bytes_with_nul().len()) };
    }
    let Some(slot) = selected else { return false };
    let available = unsafe {
        (*slot).key.is_null() && (*slot).revealed.is_null() && (*slot).seal.context.is_null()
    };
    if !available || (binding.kind != 1 && unsafe { super::digest::fips_mode() } == 0) {
        return false;
    }
    let capacity = match binding.kind {
        3 => 33,
        4 => tls_signer::MAX_KEY_BYTES + 1,
        _ => 65_537,
    };
    let Some(seed) = Seed::new(capacity) else {
        return false;
    };
    let source = unsafe { text.add(binding.source_start) };
    let kind = if binding.environment {
        StoreSource::Environment
    } else {
        StoreSource::Descriptor
    };
    let Some(length) = (unsafe { load_seed(slot, &seed, source, kind) }) else {
        return false;
    };
    match binding.kind {
        1 => unsafe {
            (*slot).revealed = seed.retain();
            (*slot).length = length;
        },
        2 => unsafe {
            let ctx = hmac_new();
            if ctx.is_null() || hmac_init(ctx, seed.bytes.cast(), length, sha256(), null_mut()) != 1
            {
                hmac_free(ctx);
                return false;
            }
            (*slot).seal.context = ctx;
        },
        3 if length == 32 => {
            let key = unsafe { private_key(995, seed.bytes, 32) };
            if key.is_null() {
                return false;
            }
            let role = bytes.iter().skip(binding.role_start).take(binding.role_len);
            let slot = unsafe { &mut *slot };
            for (out, &byte) in slot.domain.iter_mut().zip(b"witchy/v2/".iter().chain(role)) {
                *out = byte;
            }
            slot.domain_len = b"witchy/v2/".len() + binding.role_len + 1;
            slot.key = key;
            slot.key_kind = 3;
        }
        4 if (1..=tls_signer::MAX_KEY_BYTES).contains(&length) => {
            let bytes = unsafe { core::slice::from_raw_parts(seed.bytes, length) };
            let Some(key) = tls_signer::from_pkcs8(bytes) else { return false };
            let slot = unsafe { &mut *slot };
            slot.key = key.as_ptr().cast();
            slot.key_kind = 4;
        }
        _ => return false,
    }
    true
}
