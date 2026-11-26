//! Linux AArch64 Landlock and seccomp adapter.

use super::global::Global;
use super::sys::{self, Stream, perror};
use core::ffi::{c_char, c_int, c_long, c_void};

#[cfg(not(target_endian = "little"))]
compile_error!("native seccomp argument offsets require little-endian AArch64");

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    safe fn personality(value: u64) -> c_int;
    fn prctl(option: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn setvbuf(stream: *mut c_void, buffer: *mut c_char, mode: c_int, size: usize) -> c_int;
    static mut stdout: *mut c_void;
    static mut stdin: *mut c_void;
}

/// `close_range(first, last, 0)`: closes every descriptor numbered from
/// `first` to `last`. Zero on success.
fn close_range(first: u32, last: u32) -> c_long {
    // SAFETY: the call takes three integers and closes descriptors, which
    // touches no memory.
    unsafe { syscall(CLOSE_RANGE, first, last, 0u32) }
}

/// `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)`. Zero on success.
fn set_no_new_privs() -> c_int {
    // SAFETY: the option takes integers only.
    unsafe { prctl(38, 1 as c_long, 0 as c_long, 0 as c_long, 0 as c_long) }
}

/// `prctl(PR_SET_DUMPABLE, 0, 0, 0, 0)`. Zero on success.
fn clear_dumpable() -> c_int {
    // SAFETY: the option takes integers only.
    unsafe { prctl(4, 0 as c_long, 0 as c_long, 0 as c_long, 0 as c_long) }
}

/// The Landlock ABI the kernel offers, or a negative value.
fn landlock_abi() -> c_long {
    const ABI_QUERY: u32 = 1;
    // SAFETY: a version query passes no attribute for the kernel to read.
    unsafe {
        syscall(
            LANDLOCK_CREATE_RULESET,
            core::ptr::null::<u64>(),
            0usize,
            ABI_QUERY,
        )
    }
}

/// `landlock_restrict_self(rules, 0)`. Zero on success.
fn landlock_restrict_self(rules: c_int) -> c_long {
    // SAFETY: the call takes a descriptor and a flags word.
    unsafe { syscall(LANDLOCK_RESTRICT_SELF, rules, 0u32) }
}

const LANDLOCK_CREATE_RULESET: c_long = 444;
const LANDLOCK_ADD_RULE: c_long = 445;
const LANDLOCK_RESTRICT_SELF: c_long = 446;
const CLOSE_RANGE: c_long = 436;
const SECCOMP: c_long = 277;

/// Close every inherited descriptor except granted streams and explicitly
/// retained descriptors, even when descriptor numbers are sparse.
pub(super) fn witchy_close_inherited_keeping(
    output: bool,
    input: bool,
    keep: &[c_int],
) -> bool {
    if keep.len() > 64 {
        return false;
    }
    let mut kept = [0u32; 67];
    let mut n = 0usize;
    for fd in [Some(2), output.then_some(1), input.then_some(0)]
        .into_iter()
        .flatten()
    {
        kept[n] = fd;
        n += 1;
    }
    for &fd in keep {
        if fd >= 0 {
            kept[n] = fd as u32;
            n += 1;
        }
    }
    kept[..n].sort_unstable();
    let mut from = 0u32;
    for &fd in &kept[..n] {
        if fd > from && close_range(from, fd - 1) != 0 {
            return false;
        }
        from = fd.saturating_add(1);
    }
    close_range(from, u32::MAX) == 0
}

fn witchy_close_inherited_descriptors(output: bool, input: bool, relay: c_int) -> bool {
    witchy_close_inherited_keeping(output, input, &[relay])
}

static SIGNING: Global<bool> = Global::new(false);

/// Establish process-wide restrictions before the supervisor opens grants or
/// creates children, so the broker and Exec descendants inherit them.
#[unsafe(no_mangle)]
extern "C" fn witchy_linux_prepare_supervisor() -> bool {
    if set_no_new_privs() != 0 || clear_dumpable() != 0 {
        perror(c"witchy: no_new_privs");
        return false;
    }
    true
}

#[unsafe(no_mangle)]
extern "C" fn witchy_sandbox_allow_signing() {
    SIGNING.set(true);
}

fn restrict_filesystem() -> bool {
    let abi = landlock_abi();
    if abi < 0 {
        perror(c"native sandbox Landlock ABI query");
        return false;
    }
    if abi < 3 {
        Stream::stderr().put(c"native sandbox requires Landlock ABI 3 or newer\n");
        return false;
    }
    // The first fifteen filesystem rights run from EXECUTE through TRUNCATE.
    // This is the original one-field Landlock ruleset attribute.
    let rights: u64 = (1 << 15) - 1;
    let rules = unsafe {
        syscall(
            LANDLOCK_CREATE_RULESET,
            &raw const rights,
            size_of::<u64>(),
            0u32,
        )
    };
    if rules < 0 {
        perror(c"native sandbox Landlock ruleset");
        return false;
    }
    let rules = rules as c_int;
    let ok = landlock_restrict_self(rules) == 0;
    if !ok {
        perror(c"native sandbox Landlock installation");
    }
    close(rules);
    ok
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Filter {
    code: u16,
    yes: u8,
    no: u8,
    value: u32,
}

const fn load(offset: u32) -> Filter {
    Filter {
        code: 0x20,
        yes: 0,
        no: 0,
        value: offset,
    }
}

const fn equal(value: u32, yes: u8, no: u8) -> Filter {
    Filter {
        code: 0x15,
        yes,
        no,
        value,
    }
}

const fn result(value: u32) -> Filter {
    Filter {
        code: 0x06,
        yes: 0,
        no: 0,
        value,
    }
}

// mprotect's protection argument must be exactly PROT_NONE.
const fn remove_memory_access(on_success: u32, deny: u32) -> [Filter; 7] {
    [
        load(36), // args[2] high word
        equal(0, 1, 0),
        result(deny),
        load(32),
        equal(0, 1, 0),
        result(deny),
        result(on_success),
    ]
}

fn restrict_syscalls(output: bool, allocator: bool) -> bool {
    const ALLOW: u32 = 0x7fff_0000;
    const DENY: u32 = 0x0005_0001; // SECCOMP_RET_ERRNO | EPERM
    const KILL: u32 = 0x8000_0000;
    let write = if output { ALLOW } else { DENY };
    let allocate = if allocator { ALLOW } else { DENY };
    let mut code = FilterBuilder::new();
    code.add(&[
        load(4),                  // seccomp_data.arch
        equal(0xc000_00b7, 1, 0), // AUDIT_ARCH_AARCH64
        result(KILL),
        load(0),         // seccomp_data.nr
        equal(93, 0, 1), // exit
        result(ALLOW),
        equal(94, 0, 1), // exit_group
        result(ALLOW),
        equal(64, 0, 4), // write
        load(16),        // args[0] low word
        equal(1, 1, 0),  // stdout
        result(DENY),
        result(write),
        equal(278, 0, 1), // getrandom, for the signing provider
        result(if SIGNING.get() { ALLOW } else { DENY }),
        equal(98, 0, 1), // futex, for the signing provider
        result(if SIGNING.get() { ALLOW } else { DENY }),
        equal(214, 0, 1), // brk
        result(allocate),
        equal(215, 0, 1), // munmap
        result(allocate),
    ]);
    code.syscall(226, &remove_memory_access(allocate, DENY)); // mprotect
    code.add(&[
        equal(222, 0, 13), // mmap
        load(36),          // args[2] high word
        equal(0, 1, 0),
        result(DENY),
        load(32),
        equal(3, 1, 0), // PROT_READ | PROT_WRITE
        result(DENY),
        load(44), // args[3] high word
        equal(0, 1, 0),
        result(DENY),
        load(40),
        equal(0x22, 1, 0), // MAP_PRIVATE | MAP_ANONYMOUS
        result(DENY),
        result(allocate),
        result(DENY),
    ]);
    if code.install() {
        return true;
    }
    perror(c"native sandbox seccomp installation");
    false
}

// Linux AArch64's brokered guest is confined to the granted streams and relay;
// the relay owns filesystem and networking access in a separate process.
fn restrict_relay_syscalls(output: bool, allocator: bool, input: bool, relay: c_int) -> bool {
    const ALLOW: u32 = 0x7fff_0000;
    const DENY: u32 = 0x0005_0001;
    const KILL: u32 = 0x8000_0000;
    let mut code = FilterBuilder::new();
    code.add(&[
        load(4),
        equal(0xc000_00b7, 1, 0),
        result(KILL),
        load(0),
        equal(93, 0, 1),
        result(ALLOW), // exit
        equal(94, 0, 1),
        result(ALLOW), // exit_group
        equal(113, 0, 1),
        result(ALLOW), // clock_gettime
        equal(115, 0, 1),
        result(ALLOW), // clock_nanosleep
        equal(101, 0, 1),
        result(ALLOW), // nanosleep
        equal(278, 0, 1),
        result(ALLOW), // getrandom
        equal(98, 0, 1),
        result(if SIGNING.get() { ALLOW } else { DENY }),
        equal(139, 0, 1),
        result(ALLOW),   // rt_sigreturn
        equal(64, 0, 9), // write: only stdout or relay
        load(20),
        equal(0, 1, 0),
        result(DENY),
        load(16),
        equal(1, 0, 1),
        result(if output { ALLOW } else { DENY }),
        equal(relay as u32, 0, 1),
        result(ALLOW),
        result(DENY),
        equal(206, 0, 7), // sendto: relay only
        load(20),
        equal(0, 1, 0),
        result(DENY),
        load(16),
        equal(relay as u32, 1, 0),
        result(DENY),
        result(ALLOW),
        equal(63, 0, 9), // read: only stdin or relay
        load(20),
        equal(0, 1, 0),
        result(DENY),
        load(16),
        equal(0, 0, 1),
        result(if input { ALLOW } else { DENY }),
        equal(relay as u32, 0, 1),
        result(ALLOW),
        result(DENY),
        equal(214, 0, 1),
        result(ALLOW), // brk
        equal(215, 0, 1),
        result(ALLOW), // munmap
        equal(233, 0, 1),
        result(ALLOW), // madvise
        equal(216, 0, 1),
        result(ALLOW),     // mremap
    ]);
    code.syscall(226, &remove_memory_access(if allocator { ALLOW } else { DENY }, DENY)); // mprotect
    code.add(&[
        equal(222, 0, 13), // mmap: anonymous private read/write only
        load(36),
        equal(0, 1, 0),
        result(DENY),
        load(32),
        equal(3, 1, 0),
        result(DENY),
        load(44),
        equal(0, 1, 0),
        result(DENY),
        load(40),
        equal(0x22, 1, 0),
        result(DENY),
        result(ALLOW),
        result(DENY),
    ]);
    if code.install() {
        return true;
    }
    perror(c"native sandbox seccomp installation");
    false
}

// Each rule starts with a syscall comparison that skips its whole body on a
// mismatch. Keeping the body with the comparison avoids hand-counted offsets.
struct FilterBuilder {
    code: [Filter; 256],
    len: usize,
}

impl FilterBuilder {
    fn new() -> Self {
        Self {
            code: [result(0); 256],
            len: 0,
        }
    }

    fn add(&mut self, instructions: &[Filter]) {
        let end = self.len + instructions.len();
        self.code[self.len..end].copy_from_slice(instructions);
        self.len = end;
    }

    fn syscall(&mut self, number: u32, body: &[Filter]) {
        #[cfg(kani)]
        assert!(body.len() <= usize::from(u8::MAX));
        self.add(&[equal(number, 0, body.len() as u8)]);
        self.add(body);
    }

    /// Installs the instructions added so far as this thread's seccomp
    /// filter: `seccomp(SECCOMP_SET_MODE_FILTER, 0, program)`. False when the
    /// kernel refuses it.
    #[cfg(not(kani))]
    fn install(&self) -> bool {
        #[repr(C)]
        struct Program {
            len: u16,
            filter: *const Filter,
        }
        let program = Program {
            len: self.len as u16,
            filter: self.code.as_ptr(),
        };
        // SAFETY: the program names `len` instructions of `code`, which holds
        // at least that many, and the kernel copies them during the call.
        unsafe { syscall(SECCOMP, 1u32, 0u32, &raw const program) == 0 }
    }

    /// A proof checks the program where the runtime would install it.
    #[cfg(kani)]
    fn install(&self) -> bool {
        proofs::well_formed(self);
        true
    }
}

fn restrict_broker_syscalls(dials: bool, signal_fd: c_int, message_fd: c_int) -> bool {
    const ALLOW: u32 = 0x7fff_0000;
    const DENY: u32 = 0x0005_0001;
    const KILL: u32 = 0x8000_0000;
    let mut code = FilterBuilder::new();
    code.add(&[load(4), equal(0xc000_00b7, 1, 0), result(KILL), load(0)]);
    // Filesystem operations are further restricted by the Landlock ruleset.
    for number in [
        63, 64, 65, 66, 67, 68, 62, 57, 56, 437, 79, 80, 291, 61, 34, 35, 38, 276, 82, 83, 46, 25,
        23, 24,
    ] {
        code.syscall(number, &[result(ALLOW)]);
    }
    code.syscall(
        424,
        &[
            load(16),
            equal(signal_fd as u32, 0, 1),
            result(if signal_fd >= 0 { ALLOW } else { DENY }),
            result(DENY),
        ],
    );
    code.syscall(95, &[result(if signal_fd >= 0 { ALLOW } else { DENY })]);
    code.syscall(
        211,
        &[
            load(16),
            equal(message_fd as u32, 0, 1),
            result(if message_fd >= 0 { ALLOW } else { DENY }),
            result(DENY),
        ],
    );
    code.syscall(
        198,
        &[
            load(16),
            equal(2, 1, 0),
            equal(10, 0, 7),
            load(24),
            Filter {
                code: 0x54,
                yes: 0,
                no: 0,
                value: !(0x800 | 0x80000),
            },
            equal(1, 0, 4),
            load(32),
            equal(0, 1, 0),
            equal(6, 0, 1),
            result(if dials { ALLOW } else { DENY }),
            result(DENY),
        ],
    );
    // sendto may only use an already connected socket, with no destination.
    code.syscall(
        206,
        &[
            load(52),
            equal(0, 1, 0),
            result(DENY),
            load(48),
            equal(0, 1, 0),
            result(DENY),
            result(ALLOW),
        ],
    );
    for number in [
        203, 202, 242, 208, 209, 204, 205, 207, 210, 73, 72, 113, 115, 101, 278, 214, 215, 216,
        233, 134, 135, 139, 172, 178, 174, 93, 94,
    ] {
        code.syscall(number, &[result(ALLOW)]);
    }
    for number in [222, 226] {
        code.syscall(
            number,
            &[
                load(32),
                Filter {
                    code: 0x45,
                    yes: 0,
                    no: 1,
                    value: 4,
                },
                result(DENY),
                result(ALLOW),
            ],
        );
    }
    code.add(&[result(DENY)]);
    code.install()
}

/// Confine the broker to already-opened directory roots and declared TCP ports.
/// `ports` must reference `port_count` initialized port numbers.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_confine_broker(
    read_root: c_int,
    write_root: c_int,
    output_parent: c_int,
    ports: *const u16,
    port_count: u32,
    signal_fd: c_int,
    message_fd: c_int,
) -> bool {
    if port_count != 0 && ports.is_null() {
        return false;
    }
    let abi = landlock_abi();
    if abi < 3 || (port_count != 0 && abi < 4) {
        return false;
    }
    const READ: u64 = (1 << 2) | (1 << 3);
    const WRITE: u64 =
        READ | (1 << 1) | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8) | (1 << 13) | (1 << 14);
    const OUTPUT: u64 = (1 << 1) | (1 << 5) | (1 << 8);
    #[repr(C)]
    struct Ruleset {
        fs: u64,
        net: u64,
        scoped: u64,
    }
    #[repr(C, packed)]
    struct PathRule {
        rights: u64,
        parent: c_int,
    }
    #[repr(C)]
    struct PortRule {
        rights: u64,
        port: u64,
    }
    let attr = Ruleset {
        fs: (1 << 15) - 1,
        net: if abi >= 4 { 3 } else { 0 },
        scoped: if abi >= 6 { 2 } else { 0 },
    };
    let size = if abi >= 4 {
        size_of::<Ruleset>()
    } else {
        size_of::<u64>()
    };
    let rules = unsafe { syscall(LANDLOCK_CREATE_RULESET, &raw const attr, size, 0u32) };
    if rules < 0 {
        return false;
    }
    let rules = rules as c_int;
    let mut ok = true;
    for (parent, rights) in [
        (read_root, READ),
        (write_root, WRITE),
        (output_parent, OUTPUT),
    ] {
        if parent >= 0 {
            let rule = PathRule { rights, parent };
            ok = ok
                && unsafe { syscall(LANDLOCK_ADD_RULE, rules, 1u32, &raw const rule, 0u32) == 0 };
        }
    }
    for i in 0..port_count as usize {
        if !ok {
            break;
        }
        let rule = PortRule {
            rights: 2,
            port: u64::from(unsafe { *ports.add(i) }),
        };
        ok = unsafe { syscall(LANDLOCK_ADD_RULE, rules, 2u32, &raw const rule, 0u32) == 0 };
    }
    ok = ok
        && set_no_new_privs() == 0
        && clear_dumpable() == 0
        && landlock_restrict_self(rules) == 0;
    close(rules);
    ok && restrict_broker_syscalls(port_count != 0, signal_fd, message_fd)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_install_sandbox_with_relay(
    output: bool,
    allocator: bool,
    input: bool,
    relay: c_int,
) -> bool {
    if relay >= 0 && (relay <= 2 || sys::descriptor_flags(relay) < 0) {
        return false;
    }
    if relay < 0 {
        return witchy_install_sandbox(output, allocator);
    }
    if set_no_new_privs() != 0 || clear_dumpable() != 0 {
        perror(c"native sandbox process setup");
        return false;
    }
    // Prepare stdio before seccomp, since glibc otherwise probes the descriptor.
    static mut OUTPUT_BUFFER: [u8; 8192] = [0; 8192];
    static mut INPUT_BUFFER: [u8; 8192] = [0; 8192];
    if (output && unsafe { setvbuf(stdout, (&raw mut OUTPUT_BUFFER).cast(), 0, 8192) } != 0)
        || (input && unsafe { setvbuf(stdin, (&raw mut INPUT_BUFFER).cast(), 0, 8192) } != 0)
        || !restrict_filesystem()
        || !witchy_close_inherited_descriptors(output, input, relay)
    {
        return false;
    }
    restrict_relay_syscalls(output, allocator, input, relay)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_install_sandbox(output: bool, allocator: bool) -> bool {
    if allocator {
        let domain = personality(u32::MAX.into());
        if domain < 0 {
            perror(c"native sandbox process personality");
            return false;
        }
        if domain & 0x040_0000 != 0 {
            Stream::stderr().put(c"native sandbox requires non-executable data mappings\n");
            return false;
        }
    }
    if set_no_new_privs() != 0 || clear_dumpable() != 0 {
        perror(c"native sandbox process setup");
        return false;
    }
    if !restrict_filesystem() {
        return false;
    }
    if close_range(3, u32::MAX) != 0 {
        perror(c"native sandbox descriptor cleanup");
        return false;
    }
    close(0);
    if !output {
        close(1);
    }
    restrict_syscalls(output, allocator)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// What the kernel asks of a program, checked where the runtime would
    /// install it: it is not empty, it ends by returning, and every jump
    /// lands inside it.
    pub(super) fn well_formed(filter: &FilterBuilder) {
        assert!(0 < filter.len && filter.len <= filter.code.len());
        assert!(filter.code[filter.len - 1].code == 0x06);
        let at: usize = kani::any();
        kani::assume(at < filter.len);
        let instruction = &filter.code[at];
        if matches!(instruction.code, 0x15 | 0x45) {
            kani::cover!(
                instruction.yes > 8 || instruction.no > 8,
                "a long jump is checked"
            );
            assert!(at + 1 + usize::from(instruction.yes) < filter.len);
            assert!(at + 1 + usize::from(instruction.no) < filter.len);
        } else {
            assert!(matches!(instruction.code, 0x20 | 0x54 | 0x06));
            assert!(instruction.yes == 0 && instruction.no == 0);
        }
    }

    /// The unbrokered guest's filter is well formed whatever it is granted.
    #[kani::proof]
    fn the_guest_filter_is_well_formed() {
        SIGNING.set(kani::any());
        assert!(restrict_syscalls(kani::any(), kani::any()));
    }

    /// The brokered guest's filter is well formed for every relay descriptor.
    #[kani::proof]
    fn the_relay_filter_is_well_formed() {
        SIGNING.set(kani::any());
        assert!(restrict_relay_syscalls(
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any()
        ));
    }

    /// The broker's filter is well formed for every descriptor it may keep,
    /// and no rule's body is longer than its one-byte skip can pass.
    #[kani::proof]
    #[kani::unwind(29)]
    fn the_broker_filter_is_well_formed() {
        assert!(restrict_broker_syscalls(
            kani::any(),
            kani::any(),
            kani::any()
        ));
    }
}
