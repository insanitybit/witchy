//! Strict profile preflight, on the exact bytes that run, before Wasmtime compiles them: a
//! hostile module is refused before any of its code (a start function above all) could
//! execute. It complements Wasmtime's full validation and the store-confinement scan in
//! main.rs. Ported from the former Node launcher (scripts/wasm-host.mjs `inspect`), check for
//! check; the messages are the same, so fixtures that expect them still match.

/// What the run grants, as the preflight needs it.
#[derive(Default)]
pub struct Granted {
    pub output: bool,
    pub allocator: bool,
    pub input: bool,
    pub dir: bool,
    pub clock: bool,
    pub rand: bool,
    pub env: bool,
    pub net: bool,
    pub exec: bool,
    pub secret: bool,
    pub grant: bool,
    pub args: bool,
    pub sqlite: bool,
    pub local_service: bool,
    pub memory_pages: u64,
    pub module_bytes: u64,
}

const FUNCTIONS: u32 = 4096;
const LOCALS: u32 = 4096;
const INSTRUCTIONS: u64 = 65_536;
/// The frame profile's stack top (src/compiler/layout: the stack ends here, below the heap).
pub(crate) const STACK_HIGH: i64 = 66_560;
/// The literal segment ends before the allocator's first payload address.
pub(crate) const LITERAL_HIGH: i64 = 98_304;

// The two-page frame profile has no allocator payloads above the stack.
pub(crate) fn literal_high(heap: bool) -> i64 {
    if heap { LITERAL_HIGH } else { 131_072 }
}
/// The last functions of a heap module are the allocator's helpers (src/compiler/allocator.rs).
const ALLOCATOR_HELPERS: u32 = 7;

/// The file operations, in the order a module imports them after `dir_authorize` (src/compiler/host.rs).
const FILE_IMPORTS: &[&str] = &["fs_path_byte", "fs_open_file", "fs_open_dir", "fs_make_dir", "fs_status", "fs_read_u8", "fs_write_u8", "fs_close", "fs_abandon",
    "fs_probe", "fs_open_list", "fs_remove", "fs_open_append", "fs_open_new", "fs_rename_from", "fs_rename", "fs_read", "fs_write"];
const SNAPSHOT_REMAINING: &str = "fs_snapshot_remaining";
/// Each host family (src/compiler/host.rs `Family::ALL`): authorizer, operations, and the grant it needs.
const HOST_FAMILIES: &[(&str, &[&str], fn(&Granted) -> bool)] = &[
    ("dir_authorize", FILE_IMPORTS, |g| g.dir),
    ("clock_authorize", &["clock_now_ms", "clock_timer", "clock_timer_end"], |g| g.clock),
    ("rand_authorize", &["rand_u64", "rand_p384_agree", "rand_p384_start", "rand_p384_public", "rand_p384_finish", "rand_p384_close"], |g| g.rand),
    ("env_authorize", &["env_name_byte", "env_lookup", "env_value_byte"], |g| g.env),
    ("net_authorize", &["net_addr_byte", "net_only", "net_connect", "net_status", "net_send_u8", "net_recv_u8", "net_close", "net_accept", "net_send", "net_recv",
        "net_group", "net_group_add", "net_group_take", "net_wait", "net_try_recv", "net_try_send", "net_try_accept",
        "net_handle", "net_poll", "net_accepted_on"], |g| g.net),
    ("exec_authorize", &["exec_name_byte", "exec_only", "exec_arg_byte", "exec_arg_end", "exec_run", "exec_status", "exec_out_u8", "exec_code", "exec_input_u8", "exec_deadline_ms", "exec_output_limit", "exec_err_u8", "exec_cwd", "exec_env_name_u8", "exec_env_value_u8", "exec_env_end"], |g| g.exec),
    ("secret_authorize", &["secret_name_byte", "secret_get", "secret_status", "secret_byte", "secret_message_byte", "secret_hmac", "secret_digest_byte",
        "signer_public_key", "signer_sign", "tls_signer_sign", "secret_hmac_sha384", "mldsa87_signer_public_key", "mldsa87_signer_sign"], |g| g.secret),
    // The values of grantable capabilities: policy data, never authority.
    ("grant_authorize", &["grant_name_byte", "grant_lookup"], |g| g.grant),
    // The words of the launch (`--args`, `--arg=`): read-only.
    ("args_authorize", &["args_count", "args_select", "args_byte"], |g| g.args),
    ("sqlite_authorize", &["sqlite_run"], |g| g.sqlite),
    ("local_service_authorize", &["local_service_exchange"], |g| g.local_service),
];
/// Where the signer's operations start in the secret family's list: a module built before
/// them imports only the operations before.
const SIGNER_IMPORTS: usize = 7;
const TLS_SIGNER_IMPORT: usize = 9;
const SECRET_SHA384_IMPORT: usize = 10;
const MLDSA87_SIGNER_IMPORT: usize = 11;
/// Where `net_accepted_on` is in the network family's list: a module built before it
/// (2026-11-21) imports only the operations before.
const ACCEPTED_ON_IMPORT: usize = 19;
/// The public crypto operations (src/compiler/host.rs `Family::Crypto`), after every family above:
/// no authorizer and no grant, since each computes only on the slices it is given.
const PUBLIC_IMPORTS: &[&str] = &[
    "crypto_sha256", "crypto_sha384", "crypto_mldsa65_verify", "crypto_sha512", "crypto_mldsa87_verify",
    "crypto_tls13_aes128gcm_seal", "crypto_tls13_aes128gcm_open",
    "crypto_tls13_aes256gcm_seal", "crypto_tls13_aes256gcm_open", "crypto_verify_tag48", "crypto_x509_p384_verify",
    "crypto_p256_verify", "crypto_rsa_pkcs1_sha256_verify",
];
const PUBLIC_IMPORT_COUNTS: &[usize] = &[PUBLIC_IMPORTS.len(), 11, 10, 5, 3];
/// The imports of type `(i64, i64, i64) -> (i64)` (`HostOp::three`); every other operation is
/// `(i64, i64) -> (i64)`.
const THREE_IMPORTS: &[&str] = &["signer_sign", "tls_signer_sign", "crypto_mldsa65_verify", "crypto_mldsa87_verify",
    "crypto_p256_verify", "rand_p384_finish", "secret_hmac_sha384", "mldsa87_signer_sign", "local_service_exchange"];

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
/// F64 (docs/wasm-f64.md): a value type of the profile. A NaN's payload may differ between
/// machines; the compiler makes `F64.bits` give one canonical NaN, the only place it shows.
const F64: u8 = 0x7c;

struct Reader<'a> { bytes: &'a [u8], at: usize }

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.at).ok_or("truncated module")?;
        self.at += 1;
        Ok(byte)
    }
    fn uint(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for i in 0..5 {
            let byte = self.byte()?;
            if i == 4 && byte & 0xf0 != 0 { return Err("invalid u32 LEB".into()); }
            value |= u32::from(byte & 127) << (7 * i);
            if byte & 128 == 0 { return Ok(value); }
        }
        Err("invalid u32 LEB".into())
    }
    fn signed(&mut self, max: usize) -> Result<(), String> {
        for _ in 0..max { if self.byte()? & 128 == 0 { return Ok(()); } }
        Err("invalid signed LEB".into())
    }
    fn signed32(&mut self) -> Result<i64, String> {
        let mut value = 0i64;
        for i in 0..5 {
            let byte = self.byte()?;
            value |= i64::from(byte & 127) << (7 * i);
            if byte & 128 == 0 {
                if byte & 64 != 0 { value -= 1i64 << (7 * (i + 1)); }
                if !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&value) { return Err("invalid i32 LEB".into()); }
                return Ok(value);
            }
        }
        Err("invalid i32 LEB".into())
    }
    fn part(&mut self, size: u32) -> Result<Reader<'a>, String> {
        let size = size as usize;
        if size > self.bytes.len() - self.at { return Err("truncated section".into()); }
        let part = Reader { bytes: &self.bytes[self.at..self.at + size], at: 0 };
        self.at += size;
        Ok(part)
    }
    fn text(&mut self) -> Result<String, String> {
        let size = self.uint()?;
        let part = self.part(size)?;
        String::from_utf8(part.bytes.to_vec()).map_err(|_| "invalid UTF-8 name".into())
    }
    fn end(&self) -> Result<(), String> { if self.at != self.bytes.len() { Err("trailing section data".into()) } else { Ok(()) } }
    fn count(&mut self, max: u32) -> Result<u32, String> {
        let n = self.uint()?;
        if n > max { Err("profile count limit".into()) } else { Ok(n) }
    }
    fn done(&self) -> bool { self.at >= self.bytes.len() }
}

struct Type { params: Vec<u8>, results: Vec<u8> }

fn same(types: &[Type], index: u32, params: &[u8], results: &[u8]) -> bool {
    types.get(index as usize).is_some_and(|t| t.params == params && t.results == results)
}

/// One of `main`'s fixed secrets, from the module's `witchy.secret-slots` section: the
/// parameter's name and kind, 0 `Secret<Reveal>`, 1 `Secret<Seal>`, 2 `Signer`. A slot's
/// place in the list is its number (`secret_authorize` takes `16 * (place + 1) + kind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot { pub name: String, pub kind: u8 }

/// The name of the section listing `main`'s fixed secrets.
pub const SLOTS: &str = "witchy.secret-slots";

/// `inspect_slots` without the slots, for the tests.
#[cfg(test)]
pub fn inspect(bytes: &[u8], granted: &Granted) -> Result<(), String> { inspect_slots(bytes, granted).map(drop) }

/// Refuses a module outside the closed profile, or one whose imports the run does not grant;
/// gives the module's fixed secrets, which the run must bind each by name.
pub fn inspect_slots(bytes: &[u8], granted: &Granted) -> Result<Vec<Slot>, String> {
    let instruction_limit = INSTRUCTIONS.max(granted.module_bytes);
    if bytes.len() as u64 > granted.module_bytes { return Err("module size/type limit".into()); }
    let mut r = Reader { bytes, at: 0 };
    for expected in [0, 97, 115, 109, 1, 0, 0, 0] { if r.byte()? != expected { return Err("invalid Wasm header".into()); } }
    let (mut types, mut functions, mut imports) = (Vec::<Type>::new(), Vec::<u32>::new(), Vec::<(String, String, u32)>::new());
    let (mut imported, mut last_section, mut bodies, mut export_count, mut instruction_count) = (0u32, 0u8, None::<u32>, 0u32, 0u64);
    let (mut memory, mut metadata, mut stack_global) = (0u32, false, false);
    let mut slots = Vec::new();
    // The section state after `witchy.secret-slots`: only the `name` section may follow.
    const AFTER_SLOTS: u8 = 254;
    // A program with tasks has four more private i32 globals after the stack pointer: where a
    // pause is, the running task's block, and the bounds of the stack frames are on.
    let mut globals = 0u32;
    // Output (3), input (2), allocator and snapshot query, plus the canonical families.
    let import_limit = 7 + HOST_FAMILIES.iter().map(|(_, ops, _)| 1 + ops.len() as u32).sum::<u32>() + PUBLIC_IMPORTS.len() as u32;
    while !r.done() {
        let id = r.byte()?;
        // Two custom sections are admitted, after the code, each once and in this order:
        // `witchy.secret-slots`, `main`'s fixed secrets, which the run binds by name before it
        // starts; and the standard `name` section, last. It holds function names only, which a
        // trap's backtrace shows; nothing reads it to run.
        let previous = last_section;
        let custom = id == 0 && (matches!(previous, 10 | 11 | AFTER_SLOTS));
        if !custom && (![1, 2, 3, 5, 6, 7, 10, 11].contains(&id) || id <= last_section) { return Err("section forbidden by host profile".into()); }
        last_section = if custom { u8::MAX } else { id };
        let size = r.uint()?;
        let mut s = r.part(size)?;
        match id {
            11 => {
                // No passive segments, arbitrary initializer, or data for allocator memory.
                if previous != 10 || memory < 2 || s.uint()? != 1 || s.uint()? != 0
                    || s.byte()? != 0x41 || s.signed32()? != STACK_HIGH || s.byte()? != 0x0b
                {
                    return Err("literal data outside the Wasm literal region".into());
                }
                let size = s.uint()?;
                if size == 0 || i64::from(size) > literal_high(metadata) - STACK_HIGH {
                    return Err("literal data exceeds the Wasm literal region".into());
                }
                let data = s.part(size)?;
                std::str::from_utf8(data.bytes).map_err(|_| "invalid UTF-8 literal data")?;
            }
            0 => {
                let title = s.text()?;
                if title == SLOTS && matches!(previous, 10 | 11) {
                    last_section = AFTER_SLOTS;
                    // At most twelve (main's parameters), each a right (0 Reveal, 1 Seal) and a
                    // plain name of its own.
                    let count = s.count(12)?;
                    if count == 0 { return Err("empty secret slot section".into()); }
                    for _ in 0..count {
                        let kind = s.byte()?;
                        if kind > 4 { return Err("invalid secret slot right".into()); }
                        let name = s.text()?;
                        if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') { return Err("invalid secret slot name".into()); }
                        if slots.iter().any(|slot: &Slot| slot.name == name) { return Err("secret slot named twice".into()); }
                        slots.push(Slot { name, kind });
                    }
                    s.end()?;
                    continue;
                }
                if title != "name" { return Err("section forbidden by host profile".into()); }
                // Subsection 1, function names: strictly increasing indices of functions the
                // module has, each a short UTF-8 name. No other subsection.
                if s.byte()? != 1 { return Err("only function names in the name section".into()); }
                let size = s.uint()?;
                let mut names = s.part(size)?;
                // `functions` holds the imported functions too (review: adding `imported` again
                // admitted names for two functions the module does not have).
                let total = functions.len() as u32;
                let mut previous: Option<u32> = None;
                for _ in 0..names.count(total)? {
                    let index = names.uint()?;
                    if index >= total || previous.is_some_and(|p| index <= p) { return Err("invalid function name index".into()); }
                    previous = Some(index);
                    let text = names.text()?;
                    if text.len() > 256 { return Err("function name too long".into()); }
                    // The names reach stderr (Wasmtime's backtrace, the host's report): printable
                    // ASCII without a backtick, so a module writes no line breaks, terminal escapes
                    // or quoting of its own there (review: a name forged a `rejected:` line).
                    if !text.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'`') { return Err("function name outside printable ASCII".into()); }
                }
                names.end()?;
            }
            1 => {
                for _ in 0..s.count(FUNCTIONS + 2)? {
                    if s.byte()? != 0x60 { return Err("only function types allowed".into()); }
                    let (mut params, mut results) = (Vec::new(), Vec::new());
                    for _ in 0..s.count(32)? { params.push(s.byte()?); }
                    for _ in 0..s.count(32)? { results.push(s.byte()?); }
                    if params.iter().chain(&results).any(|t| *t != I32 && *t != I64 && *t != F64) { return Err("only i32/i64/f64 values allowed".into()); }
                    types.push(Type { params, results });
                }
            }
            2 => {
                imported = s.count(import_limit)?;
                for _ in 0..imported {
                    let (module, name, kind, ty) = (s.text()?, s.text()?, s.byte()?, s.uint()?);
                    if kind != 0 || types.get(ty as usize).is_none() { return Err("only typed function imports allowed".into()); }
                    imports.push((module, name, ty));
                    functions.push(ty);
                }
            }
            3 => {
                for _ in 0..s.count(FUNCTIONS.saturating_sub(imported))? {
                    let ty = s.uint()?;
                    if types.get(ty as usize).is_none() { return Err("unknown function type".into()); }
                    functions.push(ty);
                }
            }
            5 => {
                let memories = s.count(2)?;
                if memories < 1 || s.uint()? != 1 { return Err("private unshared memory32 with maximum required".into()); }
                memory = s.uint()?;
                if memory < 2 || s.uint()? != memory { return Err("private memory must have a fixed size of two or more pages".into()); }
                // A heap has a second memory, the allocator's own. Nothing else may have one.
                metadata = memories == 2;
                if metadata != (memory >= 3) { return Err("allocator memory required exactly with a heap".into()); }
                if metadata {
                    if s.uint()? != 1 { return Err("allocator memory must have a fixed size".into()); }
                    let metadata_pages = s.uint()?;
                    if metadata_pages < 1 || s.uint()? != metadata_pages { return Err("allocator memory must have a fixed size".into()); }
                    if u64::from(memory) + u64::from(metadata_pages) > granted.memory_pages { return Err("memory grant exceeded".into()); }
                }
            }
            6 => {
                let count = s.count(5)?;
                let shaped = memory != 0 && (count == 1 || (count == 5 && metadata)) && s.byte()? == I32 && s.byte()? == 1
                    && s.byte()? == 0x41 && s.signed32()? == STACK_HIGH && s.byte()? == 0x0b;
                if !shaped { return Err("frame stack global shape".into()); }
                for wanted in [Some(0), Some(0), None, Some(STACK_HIGH)].into_iter().take(count as usize - 1) {
                    let initial = (s.byte()? == I32 && s.byte()? == 1 && s.byte()? == 0x41).then(|| s.signed32()).transpose()?;
                    let shaped = initial.is_some_and(|v| wanted.is_none_or(|w| v == w) && (0..=STACK_HIGH).contains(&v)) && s.byte()? == 0x0b;
                    if !shaped { return Err("task global shape".into()); }
                }
                stack_global = true;
                globals = count;
            }
            7 => {
                // `run`, and, in a module with memory, that memory (0) as `memory`, which bulk
                // host operations read and write within its bounds. The allocator's memory is
                // never exported.
                export_count = s.count(2)?;
                let mut run = None;
                for _ in 0..export_count {
                    let (name, kind, index) = (s.text()?, s.byte()?, s.uint()?);
                    match (name.as_str(), kind) {
                        ("run", 0) if run.is_none() => run = Some(index),
                        ("memory", 2) if memory != 0 && index == 0 && export_count == 2 => {}
                        _ => return Err("only run function may be exported".into()),
                    }
                }
                let Some(run) = run else { return Err("only run function may be exported".into()) };
                export_count = 1;
                let ty = functions.get(run as usize).copied();
                if run < imported || !ty.is_some_and(|ty| same(&types, ty, &[], &[])) { return Err("run must be a defined () -> () function".into()); }
            }
            _ => {
                let count = s.count(FUNCTIONS)?;
                if count as usize != functions.len() - imported as usize { return Err("function/code count mismatch".into()); }
                bodies = Some(count);
                for i in 0..count {
                    let size = s.uint()?;
                    let mut body = s.part(size)?;
                    // Store confinement: the last functions of a heap module are the allocator's
                    // helpers. They may name only the allocator's memory; every other function
                    // only the program's.
                    let owned = if metadata && i + ALLOCATOR_HELPERS >= count { 1 } else { 0 };
                    let mut locals = types[functions[(i + imported) as usize] as usize].params.len() as u32;
                    for _ in 0..body.count(LOCALS)? {
                        locals = locals.saturating_add(body.count(LOCALS)?);
                        let ty = body.byte()?;
                        if locals > LOCALS || (ty != I32 && ty != I64 && ty != F64) { return Err("local limit/type".into()); }
                    }
                    let mut depth = 1u32;
                    while !body.done() {
                        instruction_count += 1;
                        if instruction_count > instruction_limit { return Err("instruction limit".into()); }
                        let op = body.byte()?;
                        match op {
                            0x02..=0x04 => {
                                depth += 1;
                                let block = body.byte()?;
                                if depth > 128 || ![0x40, I32, I64, F64].contains(&block) { return Err("block limit/type".into()); }
                            }
                            0x0b => {
                                depth -= 1;
                                if depth == 0 && !body.done() { return Err("code after function end".into()); }
                            }
                            0x0c | 0x0d | 0x10 | 0x20 | 0x21 | 0x22 => { body.uint()?; }
                            0x23 | 0x24 if memory != 0 => { if body.uint()? >= globals.max(1) { return Err("only private stack global allowed".into()); } }
                            0x28 | 0x29 | 0x2b | 0x2d | 0x31 | 0x33 | 0x35 | 0x36 | 0x37 | 0x39 | 0x3a | 0x3c | 0x3d | 0x3e if memory != 0 => {
                                let alignment = match op { 0x28 | 0x35 | 0x36 | 0x3e => 2, 0x29 | 0x37 | 0x2b | 0x39 => 3, 0x33 | 0x3d => 1, _ => 0 };
                                let flags = body.uint()?;
                                if flags & !0x40 > alignment { return Err("invalid memory alignment".into()); }
                                let named = if flags & 0x40 != 0 { body.uint()? } else { 0 };
                                if named != owned { return Err("memory access outside the function's own memory".into()); }
                                if body.uint()? != 0 { return Err("frame memory offset must be zero".into()); }
                            }
                            // The packed integer kernels only manipulate stack values. SIMD
                            // loads/stores have different immediates and are outside this profile.
                            0xfd => match body.uint()? {
                                0x0c => { for _ in 0..16 { body.byte()?; } } // v128.const
                                0x0d => { for _ in 0..16 { if body.byte()? >= 32 { return Err("invalid SIMD shuffle lane".into()); } } }
                                0x1d | 0x1e => { if body.byte()? >= 2 { return Err("invalid SIMD word lane".into()); } }
                                // The emitter widens i8x16.bitmask's i32 immediately. Count both instructions.
                                0x64 => {
                                    if body.byte()? != 0xad { return Err("expected widened SIMD mask".into()); }
                                    instruction_count += 1;
                                    if instruction_count > instruction_limit { return Err("instruction limit".into()); }
                                }
                                0x12 | 0x23 | 0x28 | 0x2d | 0x32 | 0x37 | 0x3c
                                | 0x4e | 0x50 | 0x51 | 0x6e | 0x71 | 0x8e | 0x91 | 0x95 | 0x9e | 0x9f
                                | 0xae | 0xb1 | 0xb5 | 0xce | 0xd1 | 0xd5 | 0xd6 | 0xd9 => {}
                                _ => return Err("SIMD instruction outside integer profile".into()),
                            },
                            // Closed frame address arithmetic; indexed U64 bounds precede wrap/multiply.
                            // i64.extend_i32_u widens an address for a bulk host operation.
                            // i32.or joins the conditions a resumed task's statements are guarded by.
                            0x49 | 0x4b | 0x6a | 0x6b | 0x6c | 0x71 | 0x72 | 0xa7 | 0xad if memory != 0 => {}
                            0x41 => body.signed(5)?,
                            0x42 => body.signed(10)?,
                            // f64.const: eight bytes.
                            0x44 => { for _ in 0..8 { body.byte()?; } }
                            // F64: comparisons, abs, neg, floor, sqrt, + - * /, i64.trunc_f64_s (traps),
                            // f64.convert_i64_s/u, and the two reinterpretations.
                            0x61..=0x66 | 0x99 | 0x9a | 0x9c | 0x9f | 0xa0..=0xa3 | 0xb0 | 0xb9 | 0xba | 0xbd | 0xbf => {}
                            0x00 | 0x01 | 0x05 | 0x0f | 0x1a | 0x1b | 0x45 | 0x46 | 0x47
                            | 0x50 | 0x51 | 0x52 | 0x54 | 0x56 | 0x58 | 0x5a | 0x7c | 0x7d | 0x7e | 0x80 | 0x82
                            | 0x83 | 0x84 | 0x85 | 0x86 | 0x88 // i64 and, or, xor, shl, shr_u
                            | 0x7a | 0x7b // i64 ctz, popcnt
                            | 0x53 | 0x55 | 0x57 | 0x59 | 0x7f | 0x81 => {} // i64 lt_s, gt_s, le_s, ge_s, div_s, rem_s
                            _ => return Err("instruction forbidden by scalar profile".into()),
                        }
                    }
                    if depth != 0 { return Err("unclosed function".into()); }
                }
            }
        }
        s.end()?;
    }
    // A module with fixed secrets needs each bound (main.rs `Secrets::bind`): a run that
    // grants no secrets at all is told which.
    if !slots.is_empty() && !granted.secret { return Err(format!("main takes fixed secrets the run does not bind: {}", slots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "))); }
    let Some(bodies) = bodies.filter(|_| export_count == 1) else { return Err("missing run/code".into()) };
    if (memory != 0) != stack_global { return Err("frame memory and stack global required together".into()); }
    if metadata && bodies < ALLOCATOR_HELPERS { return Err("heap module without allocator helpers".into()); }
    let profile = if metadata { "witchy.buffer.v2" } else if memory == 2 { "witchy.frame.v1" } else { "witchy.scalar.v1" };
    let names: Vec<&str> = imports.iter().map(|(_, name, _)| name.as_str()).collect();
    // Output is two or three imports together, or none: a number with a newline, one byte, and
    // (from 2026-11-19) eight bytes of a word; then input, one import; then the allocator's,
    // exactly when the module has a heap.
    let printing = names.len() >= 2 && names[0] == "print_u64" && names[1] == "print_u8";
    let output_imports = if !printing { 0 } else if names.get(2) == Some(&"print_word") { 3 } else { 2 };
    let reading = names.get(output_imports) == Some(&"read_u8");
    // Input is one import, or (from 2026-11-19) two: a byte, and a run of up to seven.
    let input_imports = if !reading { 0 } else if names.get(output_imports + 1) == Some(&"read_run") { 2 } else { 1 };
    // Then each host family's: its authorizer and one import per operation, all together or none.
    let mut before = output_imports + input_imports;
    for (authorize, operations, _) in HOST_FAMILIES {
        if names.get(before) != Some(authorize) { continue; }
        // A module built before the signer (2026-11-20) imports the secret family without its
        // last two operations: still canonical, and it can only do what it could then.
        let operations = if *authorize == "secret_authorize" && names.get(before + 1 + SIGNER_IMPORTS) != Some(&operations[SIGNER_IMPORTS]) { &operations[..SIGNER_IMPORTS] }
            else if *authorize == "secret_authorize" && names.get(before + 1 + TLS_SIGNER_IMPORT) != Some(&"tls_signer_sign") { &operations[..TLS_SIGNER_IMPORT] }
            else if *authorize == "secret_authorize" && names.get(before + 1 + SECRET_SHA384_IMPORT) != Some(&"secret_hmac_sha384") { &operations[..SECRET_SHA384_IMPORT] }
            else if *authorize == "secret_authorize" && names.get(before + 1 + MLDSA87_SIGNER_IMPORT) != Some(&"mldsa87_signer_public_key") { &operations[..MLDSA87_SIGNER_IMPORT] }
            // Likewise one built before a socket could say which listener accepted it.
            else if *authorize == "net_authorize" && names.get(before + 1 + ACCEPTED_ON_IMPORT) != Some(&operations[ACCEPTED_ON_IMPORT]) { &operations[..ACCEPTED_ON_IMPORT] }
            else if *authorize == "rand_authorize" {
                // A module using only the historical random-number family still runs.
                if names.get(before + 2) != Some(&"rand_p384_agree") { &operations[..1] }
                else { *operations }
            }
            else { *operations };
        if operations.iter().enumerate().any(|(offset, name)| names.get(before + 1 + offset) != Some(name)) { return Err("noncanonical host imports".into()); }
        before += 1 + operations.len();
        if *authorize == "dir_authorize" && names.get(before) == Some(&SNAPSHOT_REMAINING) { before += 1; }
    }
    // Admit the complete current family or a supported historical prefix.
    if names.get(before) == Some(&PUBLIC_IMPORTS[0]) {
        let count = PUBLIC_IMPORT_COUNTS.iter().copied().find(|&count| {
            names.get(before..before + count) == Some(&PUBLIC_IMPORTS[..count])
        }).ok_or("noncanonical host imports")?;
        before += count;
    }
    let heap_imports = names.len() == before + 1 && names[before] == "allocator_authorize";
    if if metadata { !heap_imports } else { names.len() != before } { return Err("noncanonical profile imports".into()); }
    for (module, name, ty) in &imports {
        if module != profile { return Err("import namespace disagrees with actual profile".into()); }
        let name = name.as_str();
        if name == "print_u64" || name == "print_u8" || name == "print_word" {
            if !granted.output || !same(&types, *ty, &[I32, I64], &[]) { return Err("ungranted or invalid output import".into()); }
        } else if name == "read_u8" || name == "read_run" {
            if !granted.input || !same(&types, *ty, &[I64], &[I64]) { return Err("ungranted or invalid input import".into()); }
        } else if let Some((_, _, grant)) = HOST_FAMILIES.iter().find(|(authorize, _, _)| *authorize == name) {
            if !grant(granted) || !same(&types, *ty, &[I64], &[I64]) { return Err("ungranted or invalid host authorizer import".into()); }
        } else if name == SNAPSHOT_REMAINING {
            if !granted.dir || !same(&types, *ty, &[I64, I64], &[I64]) { return Err("ungranted or invalid host import".into()); }
        } else if let Some((_, _, grant)) = HOST_FAMILIES.iter().find(|(_, operations, _)| operations.contains(&name)) {
            let params: &[u8] = match name {
                "sqlite_run" => &[I64; 5],
                "rand_p384_agree" => &[I64; 4],
                _ if THREE_IMPORTS.contains(&name) => &[I64; 3],
                _ => &[I64; 2],
            };
            if !grant(granted) || !same(&types, *ty, params, &[I64]) { return Err("ungranted or invalid host import".into()); }
        } else if PUBLIC_IMPORTS.contains(&name) {
            // Granted to every run: computation on the program's own bytes.
            let params: &[u8] = match name {
                "crypto_tls13_aes128gcm_seal" | "crypto_tls13_aes128gcm_open"
                | "crypto_tls13_aes256gcm_seal" | "crypto_tls13_aes256gcm_open" | "crypto_x509_p384_verify" => &[I64; 5],
                "crypto_rsa_pkcs1_sha256_verify" => &[I64; 4],
                _ if THREE_IMPORTS.contains(&name) => &[I64; 3],
                _ => &[I64; 2],
            };
            if !same(&types, *ty, params, &[I64]) { return Err("invalid public import".into()); }
        } else if name == "allocator_authorize" {
            if !metadata || !granted.allocator || !same(&types, *ty, &[], &[I32]) { return Err("ungranted or invalid allocator import".into()); }
        } else {
            return Err("unknown profile import".into());
        }
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    //! Hostile and malformed modules, built byte by byte (formerly scripts/test-wasm.mjs and
    //! test-wasmtime-host.mjs): each must be refused before it could run.
    use super::*;

    fn section(id: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![id];
        out.extend(leb(body.len() as u32));
        out.extend_from_slice(body);
        out
    }
    fn leb(mut n: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (n & 127) as u8;
            n >>= 7;
            if n == 0 { out.push(byte); return out; }
            out.push(byte | 128);
        }
    }
    fn name(text: &str) -> Vec<u8> { let mut out = leb(text.len() as u32); out.extend_from_slice(text.as_bytes()); out }
    /// A module from its types, imports (namespace, name, type), defined functions' types,
    /// and their bodies; it exports the last function as `run`.
    fn module(types: &[(&[u8], &[u8])], imports: &[(&str, &str, u32)], defined: &[u32], bodies: &[&[u8]], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        let mut t = leb(types.len() as u32);
        for (p, r) in types { t.push(0x60); t.extend(leb(p.len() as u32)); t.extend_from_slice(p); t.extend(leb(r.len() as u32)); t.extend_from_slice(r); }
        out.extend(section(1, &t));
        if !imports.is_empty() {
            let mut i = leb(imports.len() as u32);
            for (m, n, ty) in imports { i.extend(name(m)); i.extend(name(n)); i.push(0); i.extend(leb(*ty)); }
            out.extend(section(2, &i));
        }
        let mut f = leb(defined.len() as u32);
        for ty in defined { f.extend(leb(*ty)); }
        out.extend(section(3, &f));
        for (id, body) in extra.iter().filter(|(id, _)| *id < 7) { out.extend(section(*id, body)); }
        let mut e = leb(1); e.extend(name("run")); e.push(0); e.extend(leb(imports.len() as u32 + defined.len() as u32 - 1));
        out.extend(section(7, &e));
        let mut c = leb(bodies.len() as u32);
        for body in bodies { c.extend(leb(body.len() as u32)); c.extend_from_slice(body); }
        out.extend(section(10, &c));
        out
    }
    fn granted() -> Granted { Granted { memory_pages: 4, module_bytes: 65_536, ..Granted::default() } }
    const EMPTY: &[u8] = &[0, 0x0b];
    fn refused(bytes: &[u8], granted: &Granted, why: &str) {
        let error = inspect(bytes, granted).expect_err(why);
        assert!(error.contains(why), "expected `{why}`, got `{error}`");
    }

    #[test]
    fn a_plain_module_passes() {
        inspect(&module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]), &granted()).unwrap();
    }

    #[test]
    fn printing_needs_the_output_grant() {
        let printing = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0)], &[1], &[EMPTY], &[]);
        refused(&printing, &granted(), "ungranted or invalid output import");
        inspect(&printing, &Granted { output: true, ..granted() }).unwrap();
    }

    #[test]
    fn a_word_of_output_is_the_third_output_import_and_needs_the_grant() {
        let output = Granted { output: true, ..granted() };
        let three = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0), ("witchy.scalar.v1", "print_word", 0)], &[1], &[EMPTY], &[]);
        refused(&three, &granted(), "ungranted or invalid output import");
        inspect(&three, &output).unwrap();
        // Alone, first, or after input: not the canonical output imports.
        let alone = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_word", 0)], &[1], &[EMPTY], &[]);
        refused(&alone, &output, "noncanonical profile imports");
        let first = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_word", 0), ("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0)], &[1], &[EMPTY], &[]);
        refused(&first, &output, "noncanonical profile imports");
        // Its own signature: (capability, word), nothing returned.
        let typed = module(&[(&[I32, I64], &[]), (&[I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0), ("witchy.scalar.v1", "print_word", 1)], &[2], &[EMPTY], &[]);
        refused(&typed, &output, "ungranted or invalid output import");
    }

    #[test]
    fn a_run_of_input_is_the_second_input_import_and_needs_the_grant() {
        let input = Granted { input: true, ..granted() };
        let two = module(&[(&[I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "read_u8", 0), ("witchy.scalar.v1", "read_run", 0)], &[1], &[EMPTY], &[]);
        refused(&two, &granted(), "ungranted or invalid input import");
        inspect(&two, &input).unwrap();
        // One input import is still a module of before 2026-11-19.
        let one = module(&[(&[I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "read_u8", 0)], &[1], &[EMPTY], &[]);
        inspect(&one, &input).unwrap();
        // Alone, or first: not the canonical input imports.
        let alone = module(&[(&[I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "read_run", 0)], &[1], &[EMPTY], &[]);
        refused(&alone, &input, "noncanonical profile imports");
        let first = module(&[(&[I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "read_run", 0), ("witchy.scalar.v1", "read_u8", 0)], &[1], &[EMPTY], &[]);
        refused(&first, &input, "noncanonical profile imports");
        // Its own signature: (capability) -> run.
        let typed = module(&[(&[I64], &[I64]), (&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "read_u8", 0), ("witchy.scalar.v1", "read_run", 1)], &[2], &[EMPTY], &[]);
        refused(&typed, &input, "ungranted or invalid input import");
    }

    #[test]
    fn a_host_family_needs_its_grant() {
        let clock = module(&[(&[I64], &[I64]), (&[I64, I64], &[I64]), (&[], &[])],
            &[("witchy.scalar.v1", "clock_authorize", 0), ("witchy.scalar.v1", "clock_now_ms", 1), ("witchy.scalar.v1", "clock_timer", 1), ("witchy.scalar.v1", "clock_timer_end", 1)], &[2], &[EMPTY], &[]);
        refused(&clock, &granted(), "ungranted or invalid host authorizer import");
        inspect(&clock, &Granted { clock: true, ..granted() }).unwrap();
        // The operation without its authorizer is out of order.
        let bare = module(&[(&[I64, I64], &[I64]), (&[], &[])], &[("witchy.scalar.v1", "clock_now_ms", 0)], &[1], &[EMPTY], &[]);
        refused(&bare, &Granted { clock: true, ..granted() }, "noncanonical profile imports");
    }

    /// The launch's words are a family like any other: a module that imports them runs only
    /// where they are granted (`--args`), and no other grant stands in for it.
    #[test]
    fn the_words_of_the_launch_need_their_grant() {
        let args = module(&[(&[I64], &[I64]), (&[I64, I64], &[I64]), (&[], &[])],
            &[("witchy.scalar.v1", "args_authorize", 0), ("witchy.scalar.v1", "args_count", 1), ("witchy.scalar.v1", "args_select", 1), ("witchy.scalar.v1", "args_byte", 1)], &[2], &[EMPTY], &[]);
        refused(&args, &granted(), "ungranted or invalid host authorizer import");
        refused(&args, &Granted { env: true, grant: true, input: true, ..granted() }, "ungranted or invalid host authorizer import");
        inspect(&args, &Granted { args: true, ..granted() }).unwrap();
    }

    #[test]
    fn sqlite_requires_its_grant_and_exact_signature() {
        let authorizer: (&[u8], &[u8]) = (&[I64], &[I64]);
        let transaction: (&[u8], &[u8]) = (&[I64; 5], &[I64]);
        let types = [authorizer, transaction, (&[], &[])];
        let build = |arity| module(&types, &[
            ("witchy.scalar.v1", "sqlite_authorize", 0),
            ("witchy.scalar.v1", "sqlite_run", arity),
        ], &[2], &[EMPTY], &[]);
        let sqlite = Granted { sqlite: true, ..granted() };
        inspect(&build(1), &sqlite).unwrap();
        refused(&build(1), &granted(), "ungranted or invalid host authorizer import");
        refused(&build(0), &sqlite, "ungranted or invalid host import");
    }

    #[test]
    fn local_service_requires_its_grant_and_exact_signature() {
        let build = |exchange_type| module(
            &[(&[I64], &[I64]), (&[I64; 3], &[I64]), (&[], &[])],
            &[("witchy.scalar.v1", "local_service_authorize", 0),
              ("witchy.scalar.v1", "local_service_exchange", exchange_type)],
            &[2], &[EMPTY], &[]);
        let local = Granted { local_service: true, ..granted() };
        inspect(&build(1), &local).unwrap();
        refused(&build(1), &Granted { net: true, ..granted() }, "ungranted or invalid host authorizer import");
        refused(&build(0), &local, "ungranted or invalid host import");
    }

    /// The public crypto operations need no grant, come together after every family, and each
    /// has its own type; a signer's operations come with the secret family's grant.
    #[test]
    fn public_crypto_needs_no_grant_and_its_exact_types() {
        let two: (&[u8], &[u8]) = (&[I64, I64], &[I64]);
        let three: (&[u8], &[u8]) = (&[I64, I64, I64], &[I64]);
        let imports = |types: [u32; 3]| vec![("witchy.scalar.v1", "crypto_sha256", types[0]), ("witchy.scalar.v1", "crypto_sha384", types[1]), ("witchy.scalar.v1", "crypto_mldsa65_verify", types[2])];
        let public = module(&[two, three, (&[], &[])], &imports([0, 0, 1]), &[2], &[EMPTY], &[]);
        inspect(&public, &granted()).unwrap();
        let current = PUBLIC_IMPORTS.iter().zip([0, 0, 1, 0, 1, 3, 3, 3, 3, 0, 3, 1, 4])
            .map(|(&name, ty)| ("witchy.scalar.v1", name, ty)).collect::<Vec<_>>();
        assert_eq!(current.len(), PUBLIC_IMPORTS.len());
        let build = |imports: &[(&str, &str, u32)]| {
            module(&[two, three, (&[], &[]), (&[I64; 5], &[I64]), (&[I64; 4], &[I64])],
                imports, &[2], &[EMPTY], &[])
        };
        inspect(&build(&current), &granted()).unwrap();
        inspect(&build(&current[..5]), &granted()).unwrap();
        inspect(&build(&current[..10]), &granted()).unwrap();
        inspect(&build(&current[..11]), &granted()).unwrap();
        for incomplete in [&current[..4], &current[..9], &current[..12]] {
            refused(&build(incomplete), &granted(), "noncanonical profile imports");
        }
        for index in [5, 11, 12] {
            let mut wrong_arity = current.clone();
            wrong_arity[index].2 = 0;
            refused(&build(&wrong_arity), &granted(), "invalid public import");
        }
        let swapped = module(&[two, three, (&[], &[])], &imports([0, 1, 0]), &[2], &[EMPTY], &[]);
        refused(&swapped, &granted(), "invalid public import");
        let alone = module(&[three, (&[], &[])], &[("witchy.scalar.v1", "crypto_mldsa65_verify", 0)], &[1], &[EMPTY], &[]);
        refused(&alone, &granted(), "noncanonical profile imports");
        // The retired suite's verifier (witchy-1, ML-DSA-44) is no import of this profile: a
        // module built for it is refused, not run with another algorithm under its name.
        let retired = module(&[two, three, (&[], &[])], &[("witchy.scalar.v1", "crypto_sha256", 0), ("witchy.scalar.v1", "crypto_mldsa44_verify", 1)], &[2], &[EMPTY], &[]);
        refused(&retired, &granted(), "noncanonical host imports");
    }

    /// A module from before the signer imports seven secret operations; it still runs. Eight
    /// (a signer operation without the other) is not canonical.
    #[test]
    fn the_secret_family_without_the_signer_is_still_admitted() {
        let secret = Granted { secret: true, ..granted() };
        let old = ["secret_name_byte", "secret_get", "secret_status", "secret_byte", "secret_message_byte", "secret_hmac", "secret_digest_byte"];
        let types: [(&[u8], &[u8]); 4] = [(&[I64], &[I64]), (&[I64, I64], &[I64]), (&[I64, I64, I64], &[I64]), (&[], &[])];
        let imports = |names: &[&'static str]| {
            let mut all = vec![("witchy.scalar.v1", "secret_authorize", 0)];
            all.extend(names.iter().map(|&name| ("witchy.scalar.v1", name, if THREE_IMPORTS.contains(&name) { 2 } else { 1 })));
            all
        };
        inspect(&module(&types, &imports(&old), &[3], &[EMPTY], &[]), &secret).unwrap();
        let mut new = old.to_vec(); new.extend(["signer_public_key", "signer_sign"]);
        inspect(&module(&types, &imports(&new), &[3], &[EMPTY], &[]), &secret).unwrap();
        new.push("tls_signer_sign");
        inspect(&module(&types, &imports(&new), &[3], &[EMPTY], &[]), &secret).unwrap();
        new.push("secret_hmac_sha384");
        inspect(&module(&types, &imports(&new), &[3], &[EMPTY], &[]), &secret).unwrap();
        new.extend(["mldsa87_signer_public_key", "mldsa87_signer_sign"]);
        inspect(&module(&types, &imports(&new), &[3], &[EMPTY], &[]), &secret).unwrap();
        let mut wrong_arity = imports(&new);
        wrong_arity.last_mut().unwrap().2 = 1;
        refused(&module(&types, &wrong_arity, &[3], &[EMPTY], &[]), &secret, "ungranted or invalid host import");
        let mut half = old.to_vec(); half.push("signer_public_key");
        refused(&module(&types, &imports(&half), &[3], &[EMPTY], &[]), &secret, "noncanonical host imports");
    }

    #[test]
    fn the_network_family_without_accepted_on_is_still_admitted() {
        let net = Granted { net: true, ..granted() };
        let (_, all, _) = HOST_FAMILIES.iter().find(|(authorize, _, _)| *authorize == "net_authorize").unwrap();
        assert_eq!(all[ACCEPTED_ON_IMPORT], "net_accepted_on");
        assert_eq!(all.len(), ACCEPTED_ON_IMPORT + 1);
        let types: [(&[u8], &[u8]); 3] = [(&[I64], &[I64]), (&[I64, I64], &[I64]), (&[], &[])];
        let imports = |names: &[&'static str]| { let mut list = vec![("witchy.scalar.v1", "net_authorize", 0)]; list.extend(names.iter().map(|n| ("witchy.scalar.v1", *n, 1))); list };
        inspect(&module(&types, &imports(&all[..ACCEPTED_ON_IMPORT]), &[2], &[EMPTY], &[]), &net).unwrap();
        inspect(&module(&types, &imports(all), &[2], &[EMPTY], &[]), &net).unwrap();
        refused(&module(&types, &imports(&all[..ACCEPTED_ON_IMPORT - 1]), &[2], &[EMPTY], &[]), &net, "noncanonical host imports");
        refused(&module(&types, &imports(all), &[2], &[EMPTY], &[]), &granted(), "ungranted or invalid host authorizer import");
    }

    #[test]
    fn snapshot_query_extends_only_the_files_family() {
        let files = Granted { dir: true, ..granted() };
        let types: [(&[u8], &[u8]); 3] = [(&[I64], &[I64]), (&[I64, I64], &[I64]), (&[], &[])];
        let query = ("witchy.scalar.v1", SNAPSHOT_REMAINING, 1);
        let mut imports = vec![("witchy.scalar.v1", "dir_authorize", 0)];
        imports.extend(FILE_IMPORTS.iter().map(|name| ("witchy.scalar.v1", *name, 1)));
        inspect(&module(&types, &imports, &[2], &[EMPTY], &[]), &files).unwrap();
        imports.push(query);
        inspect(&module(&types, &imports, &[2], &[EMPTY], &[]), &files).unwrap();
        refused(&module(&types, &imports, &[2], &[EMPTY], &[]), &granted(), "ungranted or invalid host authorizer import");
        refused(&module(&types, &[query], &[2], &[EMPTY], &[]), &files, "noncanonical profile imports");
    }

    #[test]
    fn imports_outside_the_profile_are_refused() {
        let wasi = module(&[(&[I32], &[]), (&[], &[])], &[("wasi_snapshot_preview1", "proc_exit", 0)], &[1], &[EMPTY], &[]);
        refused(&wasi, &granted(), "noncanonical profile imports");
        // The right names in the wrong namespace.
        let namespace = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.frame.v1", "print_u64", 0), ("witchy.frame.v1", "print_u8", 0)], &[1], &[EMPTY], &[]);
        refused(&namespace, &Granted { output: true, ..granted() }, "import namespace disagrees with actual profile");
        // The right name with the wrong signature.
        let signature = module(&[(&[I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0)], &[1], &[EMPTY], &[]);
        refused(&signature, &Granted { output: true, ..granted() }, "ungranted or invalid output import");
    }

    #[test]
    fn literal_segment_is_confined_to_the_reserved_region() {
        let base = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[
            (5, vec![1, 1, 2, 2]),
            (6, vec![1, I32, 1, 0x41, 0x80, 0x88, 0x04, 0x0b]),
        ]);
        // Add the memory export expected by the frame profile.
        let mut frame = base;
        let mut exports = vec![2];
        exports.extend(name("run"));
        exports.extend([0, 0]);
        exports.extend(name("memory"));
        exports.extend([2, 0]);
        let prior = section(7, &[1, 3, b'r', b'u', b'n', 0, 0]);
        let start = frame.windows(prior.len()).position(|bytes| bytes == prior).unwrap();
        frame.splice(start..start + prior.len(), section(7, &exports));

        let at = |offset: &[u8], payload: &[u8]| {
            let mut data = vec![1, 0, 0x41];
            data.extend(offset);
            data.push(0x0b);
            data.extend(leb(payload.len() as u32));
            data.extend(payload);
            let mut wasm = frame.clone();
            wasm.extend(section(11, &data));
            wasm
        };
        let literal_start = [0x80, 0x88, 0x04];
        inspect(&at(&literal_start, "hé".as_bytes()), &granted()).unwrap();
        refused(&at(&[0], b"x"), &granted(), "literal data outside");
        refused(&at(&literal_start, &[0xff]), &granted(), "invalid UTF-8 literal data");
        refused(&at(&literal_start, &vec![b'x'; 64_513]), &granted(), "literal data exceeds");
    }

    #[test]
    fn forbidden_sections_are_refused() {
        // A start function would run before any check of what the module imports.
        let mut start = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]);
        start.extend(section(8, &leb(0)));
        refused(&start, &granted(), "section forbidden by host profile");
        let mut table = b"\0asm\x01\0\0\0".to_vec();
        table.extend(section(4, &[1, 0x70, 0, 0]));
        refused(&table, &granted(), "section forbidden by host profile");
        let mut data = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]);
        data.extend(section(11, &[0]));
        refused(&data, &granted(), "literal data outside the Wasm literal region");
    }

    #[test]
    fn the_secret_slot_section_comes_once_after_the_code() {
        let base = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]);
        let slots = |entries: &[(u8, &str)]| { let mut out = name(SLOTS); out.extend(leb(entries.len() as u32)); for (right, n) in entries { out.push(*right); out.extend(name(n)); } out };
        let names = { let mut n = name("name"); n.push(1); let inner = [leb(1), leb(0), name("main")].concat(); n.extend(leb(inner.len() as u32)); n.extend(inner); n };
        let secret = Granted { secret: true, ..granted() };
        let mut good = base.clone(); good.extend(section(0, &slots(&[(1, "beta"), (0, "token"), (1, "alpha")]))); good.extend(section(0, &names));
        assert_eq!(inspect_slots(&good, &secret), Ok(vec![Slot { name: "beta".into(), kind: 1 }, Slot { name: "token".into(), kind: 0 }, Slot { name: "alpha".into(), kind: 1 }]));
        // Without a secret grant the run is told which slots it does not bind.
        refused(&good, &granted(), "main takes fixed secrets the run does not bind: beta, token, alpha");
        // After the name section, twice, a right that is neither, a name twice, an odd name.
        let mut late = base.clone(); late.extend(section(0, &names)); late.extend(section(0, &slots(&[(1, "a")])));
        refused(&late, &secret, "section forbidden by host profile");
        let mut twice = base.clone(); twice.extend(section(0, &slots(&[(1, "a")]))); twice.extend(section(0, &slots(&[(1, "b")])));
        refused(&twice, &secret, "section forbidden by host profile");
        let mut application = base.clone();
        application.extend(section(0, &slots(&[(4, "signer")])));
        assert_eq!(inspect_slots(&application, &secret), Ok(vec![Slot { name: "signer".into(), kind: 4 }]));
        for (entries, why) in [(vec![(5, "a")], "invalid secret slot right"), (vec![(1, "a"), (0, "a")], "secret slot named twice"), (vec![(1, "a b")], "invalid secret slot name"), (vec![], "empty secret slot section")] {
            let mut bad = base.clone(); bad.extend(section(0, &slots(&entries)));
            refused(&bad, &secret, why);
        }
    }

    #[test]
    fn only_a_function_name_section_is_admitted_last() {
        let base = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]);
        let names = |body: Vec<u8>| { let mut out = name("name"); out.extend(body); out };
        let good = { let mut n = vec![1]; let inner = [leb(1), leb(0), name("main")].concat(); n.extend(leb(inner.len() as u32)); n.extend(inner); names(n) };
        let mut admitted = base.clone(); admitted.extend(section(0, &good));
        assert_eq!(inspect(&admitted, &granted()), Ok(()));
        // Another custom section, another subsection, a name for no function, two of them.
        let mut other = base.clone(); other.extend(section(0, &name("producers")));
        refused(&other, &granted(), "section forbidden by host profile");
        let locals = { let mut n = vec![2]; n.extend(leb(1)); n.push(0); names(n) };
        let mut local = base.clone(); local.extend(section(0, &locals));
        refused(&local, &granted(), "only function names in the name section");
        let past = { let mut n = vec![1]; let inner = [leb(1), leb(5), name("x")].concat(); n.extend(leb(inner.len() as u32)); n.extend(inner); names(n) };
        let mut beyond = base.clone(); beyond.extend(section(0, &past));
        refused(&beyond, &granted(), "invalid function name index");
        // A name with a line break, and an index past the module's functions when it imports some.
        let broken = { let mut n = vec![1]; let inner = [leb(1), leb(0), name("a\nrejected: x")].concat(); n.extend(leb(inner.len() as u32)); n.extend(inner); names(n) };
        let mut forged = base.clone(); forged.extend(section(0, &broken));
        refused(&forged, &granted(), "function name outside printable ASCII");
        // Two imports and one defined function: indices 0 to 2 only (review: 3 and 4 passed).
        let printing = module(&[(&[I32, I64], &[]), (&[], &[])], &[("witchy.scalar.v1", "print_u64", 0), ("witchy.scalar.v1", "print_u8", 0)], &[1], &[EMPTY], &[]);
        let at = |index: u32| { let mut n = vec![1]; let inner = [leb(1), leb(index), name("f")].concat(); n.extend(leb(inner.len() as u32)); n.extend(inner); let mut m = printing.clone(); m.extend(section(0, &names(n))); m };
        let output = Granted { output: true, ..granted() };
        assert_eq!(inspect(&at(2), &output), Ok(()));
        refused(&at(3), &output, "invalid function name index");
        let mut twice = admitted.clone(); twice.extend(section(0, &good));
        refused(&twice, &granted(), "section forbidden by host profile");
        // Before the code section.
        let mut early = b"\0asm\x01\0\0\0".to_vec(); early.extend(section(0, &good));
        refused(&early, &granted(), "section forbidden by host profile");
    }

    #[test]
    fn exports_are_run_alone() {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        out.extend(section(1, &[1, 0x60, 0, 0]));
        out.extend(section(3, &[1, 0]));
        let mut e = leb(2); e.extend(name("run")); e.extend([0, 0]); e.extend(name("other")); e.extend([0, 0]);
        out.extend(section(7, &e));
        out.extend(section(10, &[1, 2, 0, 0x0b]));
        refused(&out, &granted(), "only run function may be exported");
        let typed = module(&[(&[I64], &[])], &[], &[0], &[EMPTY], &[]);
        refused(&typed, &granted(), "run must be a defined () -> () function");
    }

    #[test]
    fn instructions_outside_the_profile_are_refused() {
        // f32.const, memory.size without a memory, call_indirect, i32.load without a memory.
        for body in [&[0, 0x43, 0, 0, 0, 0, 0x1a, 0x0b][..], &[0, 0x3f, 0, 0x1a, 0x0b], &[0, 0x41, 0, 0x11, 0, 0, 0x0b], &[0, 0x41, 0, 0x28, 2, 0, 0x1a, 0x0b]] {
            refused(&module(&[(&[], &[])], &[], &[0], &[body], &[]), &granted(), "instruction forbidden by scalar profile");
        }
        refused(&module(&[(&[], &[])], &[], &[0], &[&[0x01, 1, 0x7d, 0x0b]], &[]), &granted(), "local limit/type");
        refused(&module(&[(&[0x7d], &[])], &[], &[0], &[EMPTY], &[]), &granted(), "only i32/i64/f64 values allowed");
        refused(&module(&[(&[], &[])], &[], &[0], &[&[0, 0x0b, 0x01]], &[]), &granted(), "code after function end");
        refused(&module(&[(&[], &[])], &[], &[0], &[&[0, 0x02, 0x40]], &[]), &granted(), "unclosed function");
    }

    #[test]
    fn packed_profile_accepts_integer_masks_and_refuses_unlisted_operations() {
        // i64.const 0, i64x2.splat, i8x16.bitmask, i64.extend_i32_u, drop.
        let body = [0, 0x42, 0, 0xfd, 0x12, 0xfd, 0x64, 0xad, 0x1a, 0x0b];
        inspect(&module(&[(&[], &[])], &[], &[0], &[&body], &[]), &granted()).unwrap();
        for opcode in [0, 0x0b, 0x67] { // v128.load, v128.store, f32x4.ceil
            let body = [0, 0xfd, opcode, 0x0b];
            refused(&module(&[(&[], &[])], &[], &[0], &[&body], &[]), &granted(), "SIMD instruction outside integer profile");
        }
        let body = [0, 0xfd, 0x1d, 2, 0x0b];
        refused(&module(&[(&[], &[])], &[], &[0], &[&body], &[]), &granted(), "invalid SIMD word lane");
    }

    #[test]
    fn memory_shapes_are_closed() {
        let frame = |memory: Vec<u8>, global: Vec<u8>| module(&[(&[], &[])], &[], &[0], &[EMPTY], &[(5, memory), (6, global)]);
        let stack = { let mut g = vec![1, I32, 1, 0x41]; g.extend([0x80, 0x88, 0x04]); g.push(0x0b); g };
        inspect(&frame(vec![1, 1, 2, 2], stack.clone()), &granted()).unwrap();
        refused(&frame(vec![1, 0, 2], stack.clone()), &granted(), "private unshared memory32 with maximum required");
        refused(&frame(vec![1, 1, 2, 3], stack.clone()), &granted(), "private memory must have a fixed size of two or more pages");
        refused(&frame(vec![1, 1, 3, 3], stack.clone()), &granted(), "allocator memory required exactly with a heap");
        refused(&frame(vec![1, 1, 2, 2], vec![1, I32, 1, 0x41, 0, 0x0b]), &granted(), "frame stack global shape");
        refused(&frame(vec![2, 1, 4, 4, 1, 1, 1], stack.clone()), &granted(), "memory grant exceeded");
        // A memory access in a function must name its own memory, at offset zero.
        let access = |body: &[u8]| module(&[(&[], &[])], &[], &[0], &[body], &[(5, vec![1, 1, 2, 2]), (6, stack.clone())]);
        inspect(&access(&[0, 0x41, 0, 0x28, 2, 0, 0x1a, 0x0b]), &granted()).unwrap();
        refused(&access(&[0, 0x41, 0, 0x28, 2, 8, 0x1a, 0x0b]), &granted(), "frame memory offset must be zero");
        refused(&access(&[0, 0x41, 0, 0x28, 0x42, 1, 0, 0x1a, 0x0b]), &granted(), "memory access outside the function's own memory");
        refused(&access(&[0, 0x23, 1, 0x1a, 0x0b]), &granted(), "only private stack global allowed");
    }

    #[test]
    fn only_the_programs_memory_is_exported() {
        // A frame module: one memory and the stack global, then the exports given.
        let stack = { let mut g = vec![1, I32, 1, 0x41]; g.extend([0x80, 0x88, 0x04]); g.push(0x0b); g };
        let with = |exports: Vec<u8>, memory: Vec<u8>, body: &[u8]| {
            let mut out = b"\0asm\x01\0\0\0".to_vec();
            out.extend(section(1, &[1, 0x60, 0, 0]));
            out.extend(section(3, &[1, 0]));
            out.extend(section(5, &memory));
            out.extend(section(6, &stack));
            out.extend(section(7, &exports));
            let mut c = leb(1); c.extend(leb(body.len() as u32)); c.extend_from_slice(body);
            out.extend(section(10, &c));
            out
        };
        let exports = |entries: &[(&str, u8, u32)]| { let mut e = leb(entries.len() as u32); for (n, k, i) in entries { e.extend(name(n)); e.push(*k); e.extend(leb(*i)); } e };
        let memory = vec![1, 1, 2, 2];
        inspect(&with(exports(&[("run", 0, 0), ("memory", 2, 0)]), memory.clone(), EMPTY), &granted()).unwrap();
        inspect(&with(exports(&[("memory", 2, 0), ("run", 0, 0)]), memory.clone(), EMPTY), &granted()).unwrap();
        refused(&with(exports(&[("run", 0, 0), ("memory", 2, 1)]), memory.clone(), EMPTY), &granted(), "only run function may be exported");
        refused(&with(exports(&[("run", 0, 0), ("heap", 2, 0)]), memory.clone(), EMPTY), &granted(), "only run function may be exported");
        refused(&with(exports(&[("run", 0, 0), ("run", 0, 0)]), memory.clone(), EMPTY), &granted(), "only run function may be exported");
        // A scalar module has no memory to export.
        let mut scalar = b"\0asm\x01\0\0\0".to_vec();
        scalar.extend(section(1, &[1, 0x60, 0, 0]));
        scalar.extend(section(3, &[1, 0]));
        scalar.extend(section(7, &exports(&[("run", 0, 0), ("memory", 2, 0)])));
        scalar.extend(section(10, &[1, 2, 0, 0x0b]));
        refused(&scalar, &granted(), "only run function may be exported");
        // An address widened for a bulk operation: only where there is a memory.
        inspect(&with(exports(&[("run", 0, 0)]), memory.clone(), &[0, 0x41, 0, 0xad, 0x1a, 0x0b]), &granted()).unwrap();
        refused(&module(&[(&[], &[])], &[], &[0], &[&[0, 0x41, 0, 0xad, 0x1a, 0x0b]], &[]), &granted(), "instruction forbidden by scalar profile");
    }

    #[test]
    fn the_size_grant_is_kept() {
        let bytes = module(&[(&[], &[])], &[], &[0], &[EMPTY], &[]);
        refused(&bytes, &Granted { module_bytes: 8, ..granted() }, "module size/type limit");
        refused(b"\0asm\x02\0\0\0", &granted(), "invalid Wasm header");
        refused(&bytes[..bytes.len() - 1], &granted(), "truncated");
    }
}
