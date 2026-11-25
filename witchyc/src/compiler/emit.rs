//! Mechanical core Wasm encoding from the private checked IR. No text opcode
//! parsing, external assembler, C/LLVM, Lean extraction, or runtime interpreter.
mod tasks;

mod places;
mod statements;
mod storage;
mod boxes;
mod expressions;
mod packed;

use super::ir::*;
use super::indices::Indices;
use super::frame::{self, Frame};
use super::place::{Place, Projection};
use super::layout::Layouts;
use std::collections::BTreeMap;
use super::scalar::Scalar;
use crate::diagnostic::Diagnostic;

pub(super) fn unsigned(out: &mut Vec<u8>, mut n: u32) {
    loop {
        let byte = (n & 127) as u8;
        n >>= 7;
        out.push(byte | if n != 0 { 128 } else { 0 });
        if n == 0 {
            break;
        }
    }
}
pub(super) fn signed(out: &mut Vec<u8>, mut n: i64) {
    loop {
        let byte = (n & 127) as u8;
        n >>= 7;
        let done = (n == 0 && byte & 64 == 0) || (n == -1 && byte & 64 != 0);
        out.push(byte | if done { 0 } else { 128 });
        if done {
            break;
        }
    }
}
fn name(out: &mut Vec<u8>, value: &str) {
    unsigned(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}
fn section(out: &mut Vec<u8>, id: u8, data: Vec<u8>) {
    out.push(id);
    unsigned(out, data.len() as u32);
    out.extend(data);
}
fn valtype(t: Ty) -> u8 {
    match t {
        Ty::Bool | Ty::Console | Ty::Allocator | Ty::Tag | Ty::Borrow { .. } => 0x7f,
        // The input grant is a 64-bit value: it is handed to an import of the type (i64) -> (i64).
        Ty::Unsigned(_) | Ty::I64 | Ty::Boxed | Ty::Buffer(_) | Ty::Input | Ty::Host(_) => 0x7e,
        Ty::F64 => 0x7c,
        Ty::Unit => 0x40,
        Ty::Aggregate(_) | Ty::Sum(_) | Ty::Slice(_) => unreachable!("compound ABI was flattened"),
    }
}
fn signature(out: &mut Vec<u8>, params: &[Ty], results: &[Ty]) {
    out.push(0x60);
    unsigned(out, params.len() as u32);
    out.extend(params.iter().map(|t| valtype(*t)));
    unsigned(out, results.len() as u32);
    out.extend(results.iter().map(|t| valtype(*t)));
}
pub(super) fn indexed(out: &mut Vec<u8>, op: u8, index: u32) {
    out.push(op);
    unsigned(out, index);
}
pub(super) fn get(out: &mut Vec<u8>, local: u32) {
    indexed(out, 0x20, local);
}
pub(super) fn call(out: &mut Vec<u8>, function: u32) {
    indexed(out, 0x10, function);
}
pub(super) fn trap_if(out: &mut Vec<u8>) {
    out.extend([0x04, 0x40, 0x00, 0x0b]);
}
/// `trap_if` that says why: `i32.const why; drop` just before the `unreachable`, run only when
/// it traps, so the host can name the check from the instructions at the trap (and the words
/// stay with the code wherever the inliner writes it). The codes are the host's
/// (`trap_reason`, tools/wasmtime-host): 1-3 `+ - *` on unsigned numbers, 4 I64 arithmetic,
/// 5 a value too large for a narrower type, 6 an index out of range, 7 a shift by the width or
/// more, 8 an invalid slice range, 10 a non-boundary UTF-8 substring endpoint.
pub(super) fn trap_for(out: &mut Vec<u8>, why: u8) {
    debug_assert!(why < 64);
    out.extend([0x04, 0x40, 0x41, why, 0x1a, 0x00, 0x0b]);
}

pub(super) fn module(program: &Program, indices: &Indices, tracked: &std::collections::BTreeSet<u32>) -> Result<(Vec<u8>,Vec<String>), Diagnostic> {
    let mut features = Vec::new();
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let heap = program.layouts.uses_heap();
    let mut literals = Literals::new(heap);
    let profile = if heap { super::BUFFER_PROFILE } else if program.frame_profile() { super::FRAME_PROFILE } else { super::PROFILE };
    // Types 0 = output(i32 capability, i64 value), 1 = checked arithmetic,
    // 2 = scalar range check, then one exact signature for each source function. Duplicate signatures
    // are legal; avoiding interning keeps this first emitter easy to inspect.
    let mut types = Vec::new();
    unsigned(&mut types, 3 + program.functions.len() as u32 + if heap { 1 + super::allocator::HELPERS } else { 0 } + indices.extended_host_types.iter().flatten().count() as u32);
    signature(&mut types, &[Ty::Bool, Ty::U64], &[]);
    signature(&mut types, &[Ty::U64, Ty::U64], &[Ty::U64]);
    signature(&mut types, &[Ty::U64], &[Ty::U64]);
    for f in &program.functions {
        signature(&mut types, &f.params, &program.layouts.flat(f.result));
    }
    if heap {
        signature(&mut types, &[], &[Ty::Allocator]);
        signature(&mut types, &[Ty::U64], &[Ty::U64, Ty::U64]);
        signature(&mut types, &[Ty::U64, Ty::U64], &[Ty::U64]);
        signature(&mut types, &[Ty::Allocator, Ty::U64, Ty::U64], &[Ty::Tag, Ty::U64, Ty::U64]);
        signature(&mut types, &[Ty::U64, Ty::U64], &[Ty::U64]);
        signature(&mut types, &[Ty::U64, Ty::U64], &[Ty::Tag, Ty::U64]);
        signature(&mut types, &[Ty::U64, Ty::U64, Ty::U64, Ty::U64, Ty::U64], &[Ty::U64, Ty::U64]);
        signature(&mut types, &[Ty::U64, Ty::U64, Ty::U64, Ty::U64, Ty::U64], &[Ty::U64, Ty::U64]);
    }
    for (extra, ty) in indices.extended_host_types.iter().enumerate() {
        if ty.is_some() {
            signature(&mut types, &[Ty::U64; 5][..extra + 3], &[Ty::U64]);
        }
    }
    section(&mut out, 1, types);
    if indices.source != 0 {
        let mut imports = Vec::new();
        unsigned(&mut imports, indices.source);
        if indices.print.is_some() {
            // The output imports share type 0: (capability, value). One prints a number and a
            // newline, one a byte, one eight bytes of a word, lowest first; none gives the host
            // any view of guest memory.
            for import in ["print_u64", "print_u8", "print_word"] {
                name(&mut imports, profile); name(&mut imports, import);
                imports.extend([0, 0]);
            }
        }
        if indices.read.is_some() {
            // Type 2, (i64) -> (i64): the grant goes in, the next byte or 256 comes out. The host
            // hands over a value; it writes no guest memory.
            name(&mut imports, profile); name(&mut imports, "read_u8");
            imports.extend([0, 2]);
            // Type 2 too: up to seven bytes, lowest first, with their count in the top byte; a run
            // ends after a newline or a byte of 128 or more, so nothing it holds belongs to the
            // next line, and a count of 0 is the end of input.
            name(&mut imports, profile); name(&mut imports, "read_run");
            imports.extend([0, 2]);
        }
        for family in super::host::Family::ALL {
            if indices.host[family.ordinal()].is_none() { continue; }
            // Type 2, (i64) -> (i64): a code in (a directory's right), `main`'s value out. Then
            // the operations, each of type 1, (i64, i64) -> (i64). None gives the host a view
            // of guest memory.
            // A public family (`Crypto`) has no authorizer. Larger host signatures have their own type.
            if let Some(authorize) = family.authorize() {
                name(&mut imports, profile); name(&mut imports, authorize);
                imports.extend([0, 2]);
            }
            for op in family.ops() {
                name(&mut imports, profile); name(&mut imports, op.import());
                imports.push(0);
                let arity = op.arity();
                let ty = if arity == 2 { 1 } else {
                    indices.extended_host_types[arity - 3].expect("type for imported host operation")
                };
                unsigned(&mut imports, ty);
            }
        }
        if indices.authorize.is_some() {
            name(&mut imports, profile); name(&mut imports, "allocator_authorize");
            imports.push(0); unsigned(&mut imports, indices.authorize_type);
        }
        section(&mut out, 2, imports);
    }
    let mut functions = Vec::new();
    unsigned(&mut functions, program.functions.len() as u32 + 6 + if heap { super::allocator::HELPERS } else { 0 });
    for i in 0..program.functions.len() {
        unsigned(&mut functions, i as u32 + 3);
    }
    functions.extend([1, 1, 1, 2, 2, 2]);
    if heap { for ordinal in 0..super::allocator::HELPERS { unsigned(&mut functions, indices.helper_types + ordinal); } }
    section(&mut out, 3, functions);
    let memory_section_at = out.len();
    let memory_exported = heap || program.frame_profile();
    if memory_exported {
        // One private mutable i32 SP; a program with tasks adds where a pause is (`STATE`), the
        // running task's block (`TASK`) and the bounds of the stack frames are on (`LOW`, `HIGH`),
        // which a step moves to the task's own stack.
        let mut global = vec![if program.tasks.used { 5 } else { 1 }, 0x7f, 1, 0x41];
        signed(&mut global, frame::HIGH as i64);
        global.push(0x0b);
        if program.tasks.used {
            for initial in [0, 0, frame::low() as i64, frame::HIGH as i64] { global.extend([0x7f, 1, 0x41]); signed(&mut global, initial); global.push(0x0b); }
        }
        section(&mut out, 6, global);
    }
    // `run`; and the program's memory, which bulk host operations read and write within its
    // bounds. The allocator's memory is never exported.
    let mut exports = vec![if memory_exported { 2 } else { 1 }];
    name(&mut exports, "run");
    exports.push(0);
    unsigned(&mut exports, program.main + indices.source);
    if memory_exported { name(&mut exports, "memory"); exports.extend([2, 0]); }
    section(&mut out, 7, exports);
    let mut encoded = Vec::new();
    let mut bodies = Vec::new();
    for (ordinal, f) in program.functions.iter().enumerate() {
        let resumable = program.tasks.pausing.get(ordinal).copied().unwrap_or(false);
        let mut emitter = Emitter {
            tracked: tracked.contains(&program.definitions[ordinal]).then_some(program.names[ordinal].as_str()),
            features: &mut features,
            literals: &mut literals,
            indices,
            layouts: &program.layouts,
            locals: f.locals.clone(),
            frame: &f.frame,
            old_sp: None,
            base: None,
            labels: 0,
            scratch: None,
            tasks: &program.tasks,
            resumable,
            proved: program.proved_arithmetic.get(ordinal).copied().unwrap_or(false),
            proved_index: program.proved_index.get(ordinal).copied().unwrap_or(false),
            proved_slices: program.proved_slices.get(ordinal).copied().unwrap_or(false),
            steps: &f.steps,
            site: 0,
            depth: 0,
            resume: 0,
        };
        let mut instructions = Vec::new();
        emitter.hit(&mut instructions, "entry");
        if resumable {
            emitter.resumable_body(&mut instructions, f);
        } else {
            emitter.prologue(&mut instructions, f.params.len() as u32);
            emitter.block(&mut instructions, &f.body);
        }
        if emitter.locals.len() > 4096 {
            return Err(Diagnostic::new(
                "W0403",
                0..0,
                "scalarized function exceeds 4096 locals including temporaries",
            ));
        }
        // Return coverage was checked; a non-unit fallthrough is impossible.
        if resumable {
        } else if f.result != Ty::Unit {
            instructions.push(0x00);
        } else {
            emitter.restore(&mut instructions);
        }
        instructions.push(0x0b);
        bodies.push(super::inline::Body {
            params: f.params.iter().map(|t| valtype(*t)).collect(),
            results: program.layouts.flat(f.result).iter().map(|t| valtype(*t)).collect(),
            locals: emitter.locals[f.params.len()..].iter().map(|t| valtype(*t)).collect(),
            code: instructions,
        });
    }
    super::inline::run(&mut bodies, indices.source);
    for function in bodies {
        let mut body = Vec::new();
        // Consecutive locals of one type as one entry, `(count, type)` (it wrote `(1, type)` for
        // each: two bytes a local, and a function had hundreds after writing in).
        let mut runs: Vec<(u32, u8)> = Vec::new();
        for t in function.locals {
            match runs.last_mut() { Some((count, ty)) if *ty == t => *count += 1, _ => runs.push((1, t)) }
        }
        unsigned(&mut body, runs.len() as u32);
        for (count, t) in runs {
            unsigned(&mut body, count);
            body.push(t);
        }
        body.extend(function.code);
        encoded.push(body);
    }
    for op in [Binary::Add, Binary::Sub, Binary::Mul] {
        let body = arithmetic(op);
        encoded.push(body);
    }
    for scalar in Scalar::NARROW {
        let mut body = vec![0]; // no additional locals
        get(&mut body, 0);
        body.push(0x42);
        signed(&mut body, scalar.layout().maximum() as i64);
        body.push(0x56);
        trap_for(&mut body, 5);
        get(&mut body, 0);
        body.push(0x0b);
        encoded.push(body);
    }
    if heap {
        encoded.extend(super::allocator::bodies(program));
    }
    if literals.overflow {
        return Err(Diagnostic::new("W0401", 0..0, format!("static UTF-8 bytes exceed the Wasm literal region: a program's literals share the {} bytes between the stack and the heap; read long text from a file or the input instead of writing it as literals", super::allocator::HEAP_LOW - u64::from(super::allocator::STACK_HIGH))));
    }
    if memory_exported {
        let mut memory = Vec::new();
        memory_section(&mut memory, heap);
        out.splice(memory_section_at..memory_section_at, memory);
    }
    if !program.tasks.used {
        super::inline::trim_uncalled(&mut encoded, indices.source, program.main);
    }
    let mut code = Vec::new();
    unsigned(&mut code, encoded.len() as u32);
    for body in encoded {
        unsigned(&mut code, body.len() as u32);
        code.extend(body);
    }
    section(&mut out, 10, code);
    if !literals.bytes.is_empty() {
        // One active program-memory segment, at the first byte above the stack.
        let mut data = vec![1, 0, 0x41];
        signed(&mut data, i64::from(super::allocator::STACK_HIGH));
        data.push(0x0b);
        unsigned(&mut data, literals.bytes.len() as u32);
        data.extend(literals.bytes);
        section(&mut out, 11, data);
    }
    // `witchy.secret-slots`, only in a program whose `main` takes fixed secrets: each slot's
    // kind (0 `Secret<Reveal>`, 1 `Secret<Seal>`, 2 `Signer`) and parameter name, in slot order. The host binds each name
    // to its grant before the program starts, and refuses a run whose grants do not match.
    if !program.secret_slots.is_empty() {
        let mut slots = Vec::new();
        name(&mut slots, "witchy.secret-slots");
        unsigned(&mut slots, program.secret_slots.len() as u32);
        for (slot, kind) in &program.secret_slots { slots.push(*kind); name(&mut slots, slot); }
        section(&mut out, 0, slots);
    }
    // The standard `name` section: what a trap's backtrace calls each function. Nothing runs
    // or is checked by it; the host reads it to say where a run stopped and, when the stopping
    // function is a check of its own, why (`witchy.overflow +`, `witchy.too large for U8`).
    let mut entries = Vec::new();
    let mut count = 0u32;
    let names = &mut entries;
    for (ordinal, f) in program.names.iter().enumerate().take(program.functions.len()) {
        unsigned(names, indices.source + ordinal as u32);
        // `Name@file` and generated `#` parts are the compiler's; the person wrote the rest.
        let shown: String = f.split('@').next().unwrap_or(f).split('#').next().unwrap_or(f)
            // The host admits printable ASCII without a backtick in a name.
            .chars().map(|c| if (' '..='~').contains(&c) && c != '`' { c } else { '?' }).take(256).collect();
        name(names, &shown);
        count += 1;
    }
    for (offset, helper) in ["witchy.overflow +", "witchy.overflow -", "witchy.overflow *", "witchy.too large for U8", "witchy.too large for U16", "witchy.too large for U32"].iter().enumerate() {
        unsigned(names, indices.source + (program.functions.len() + offset) as u32);
        name(names, helper);
        count += 1;
    }
    let mut names = Vec::new();
    unsigned(&mut names, count);
    names.extend(entries);
    let mut custom = Vec::new();
    name(&mut custom, "name");
    custom.push(1);
    unsigned(&mut custom, names.len() as u32);
    custom.extend(names);
    section(&mut out, 0, custom);
    if features.len()>4096 {
        return Err(Diagnostic::new("W0400",0..0,"guest coverage exceeds 4096 edges"));
    }
    Ok((out,features))
}

// Memory 0 holds program frames, literals and cell payloads. A heap adds
// memory 1, which is private to the allocator. Neither memory grows.
fn memory_section(out: &mut Vec<u8>, heap: bool) {
    let mut memory = vec![if heap { 2 } else { 1 }, 1];
    let chosen = super::allocator::heap();
    let pages = if heap { chosen.pages } else { frame::PAGES };
    unsigned(&mut memory, pages);
    unsigned(&mut memory, pages);
    if heap {
        memory.push(1);
        unsigned(&mut memory, chosen.metadata_pages());
        unsigned(&mut memory, chosen.metadata_pages());
    }
    section(out, 5, memory);
}

struct Literals {
    high: u64,
    addresses: std::collections::BTreeMap<std::sync::Arc<str>, u32>,
    bytes: Vec<u8>,
    overflow: bool,
}

impl Literals {
    fn new(heap: bool) -> Self {
        let high = if heap { super::allocator::HEAP_LOW } else { u64::from(frame::PAGES) * 65_536 };
        Self { high, addresses: Default::default(), bytes: Vec::new(), overflow: false }
    }

    fn address(&mut self, text: &std::sync::Arc<str>) -> u32 {
        if text.is_empty() {
            return super::allocator::STACK_HIGH;
        }
        if let Some(&address) = self.addresses.get(text) {
            return address;
        }
        let address = super::allocator::STACK_HIGH + self.bytes.len() as u32;
        if u64::from(address) + text.len() as u64 > self.high {
            self.overflow = true;
            return 0;
        }
        self.bytes.extend_from_slice(text.as_bytes());
        self.addresses.insert(text.clone(), address);
        address
    }
}

struct Emitter<'a> {
    tracked: Option<&'a str>,
    features: &'a mut Vec<String>,
    literals: &'a mut Literals,
    layouts: &'a Layouts,
    locals: Vec<Ty>,
    indices: &'a Indices,
    frame: &'a Frame,
    old_sp: Option<u32>,
    base: Option<u32>,
    /// Wasm labels opened by statements since the innermost loop began.
    labels: u32,
    /// Three i64 locals for checked arithmetic written in place, made when first needed.
    scratch: Option<u32>,
    tasks: &'a super::pause::Tasks,
    /// This function may pause: it is emitted to unwind and resume (pause.rs).
    resumable: bool,
    /// Its accepted proof shows every unsigned `+ - *` in range: they are emitted unchecked.
    proved: bool,
    /// This function's proof shows every array index below its length.
    proved_index: bool,
    /// Every slice read in this function was proved below the slice's length.
    proved_slices: bool,
    /// Counting-`for` counters: `+= 1` on one is a plain add.
    steps: &'a std::collections::BTreeSet<u32>,
    /// The number of the next site (a call that may pause) in this function.
    site: u32,
    /// Labels inside the unwind block and outside the innermost loop's own.
    depth: u32,
    /// The i32 local holding the site a resumed function goes back into.
    resume: u32,
}

/// Globals of a program with tasks (the SP is global 0).
const STATE: u32 = 1;
const TASK: u32 = 2;
const LOW: u32 = 3;
const HIGH: u32 = 4;
/// What `STATE` says: running, unwinding to pause, going back in to resume.
const NORMAL: u32 = 0;
const UNWIND: u32 = 1;
const REWIND: u32 = 2;
/// A task's block (library/wasm-tasks.wy): a header, the input, the save area
/// (where paused functions keep their locals) and the stack. Offsets in the header:
const STATUS: u32 = 0; // u32: 0 not started, 1 paused, 2 finished
const KIND: u32 = 4; // u32: which entry a step calls
const SAVED_SP: u32 = 8; // u32: the stack pointer where the task paused
const STACK_LOW: u32 = 12; // u32: where the save area ends and the stack begins
const TOKEN: u32 = 16; // u64: what the task waits for, from its pause
const CURSOR: u32 = 24; // u32: the top of the save area
const STACK_HIGH: u32 = 28; // u32: where the stack begins, growing down
const INPUT: u32 = 32;
impl Emitter<'_> {
    fn hit(&mut self, out: &mut Vec<u8>, edge: &str) {
        let Some(function) = self.tracked else { return };
        let Some(index) = self.indices.print else { return };
        let id = self.features.len();
        self.features.push(format!("{{\"function\":\"{function}\",\"edge\":\"{edge}\"}}"));
        // Test-profile feedback uses the reserved zero handle, inaccessible to Witchy code.
        out.extend([0x41,0,0x42]);
        signed(out,id as i64);
        call(out,index);
    }

    fn scratch(&mut self) -> u32 {
        *self.scratch.get_or_insert_with(|| {
            let first = self.locals.len() as u32;
            self.locals.extend([Ty::U64; 3]);
            first
        })
    }
    /// Checked `+`, `-` or `*` of the two i64 on the stack, written in place: the same tests as the
    /// module's arithmetic helpers, without the call. Both operands are already evaluated, so the
    /// scratch locals are never live across a nested expression.
    fn checked(&mut self, out: &mut Vec<u8>, op: Binary) {
        let (a, b, c) = { let first = self.scratch(); (first, first + 1, first + 2) };
        indexed(out, 0x21, b);
        indexed(out, 0x22, a);
        get(out, b);
        match op {
            Binary::Add => {
                out.push(0x7c);
                indexed(out, 0x22, b);
                get(out, a);
                out.push(0x54); // sum < left: wrapped
                trap_for(out, 1);
                get(out, b);
            }
            Binary::Sub => {
                out.push(0x54); // left < right
                trap_for(out, 2);
                get(out, a);
                get(out, b);
                out.push(0x7d);
            }
            Binary::Mul => {
                out.push(0x7e);
                indexed(out, 0x21, c);
                // Two operands below 2^32 cannot wrap; otherwise divide back, as the helper does.
                get(out, a);
                get(out, b);
                out.extend([0x84, 0x42, 32, 0x88, 0x50, 0x45, 0x04, 0x40]);
                get(out, b);
                out.extend([0x50, 0x45, 0x04, 0x40]);
                get(out, c);
                get(out, b);
                out.push(0x80);
                get(out, a);
                out.push(0x52);
                trap_for(out, 3);
                out.extend([0x0b, 0x0b]);
                get(out, c);
            }
            _ => unreachable!("only +, - and * are checked"),
        }
    }
    /// Trap unless the i64 on the stack fits `scalar`; nothing for U64.
    /// An I64 operation on the one or two i64 on the stack. `+`, `-` and `*` trap when the exact
    /// result is not an I64; `/` and `%` are the engine's, which trap on a zero divisor and `/`
    /// on the one quotient that does not fit. The two conversions keep the 64 bits as they are.
    fn signed(&mut self, out: &mut Vec<u8>, op: Signed) {
        let simple = match op {
            Signed::FromBits | Signed::Bits => return,
            Signed::Eq => Some(0x51), Signed::Ne => Some(0x52), Signed::Lt => Some(0x53), Signed::Gt => Some(0x55),
            Signed::Le => Some(0x57), Signed::Ge => Some(0x59), Signed::Div => Some(0x7f), Signed::Rem => Some(0x81),
            Signed::Add | Signed::Sub | Signed::Mul => None,
        };
        if let Some(opcode) = simple { out.push(opcode); return; }
        let (a, b, c) = { let first = self.scratch(); (first, first + 1, first + 2) };
        indexed(out, 0x21, b);
        indexed(out, 0x22, a);
        get(out, b);
        out.push(match op { Signed::Add => 0x7c, Signed::Sub => 0x7d, _ => 0x7e });
        indexed(out, 0x21, c);
        match op {
            // The sum's sign differs from both operands' signs.
            Signed::Add => { get(out, a); get(out, c); out.push(0x85); get(out, b); get(out, c); out.push(0x85); out.extend([0x83, 0x42, 0, 0x53]); trap_for(out, 4); }
            // The operands' signs differ and the difference's sign is not the left one's.
            Signed::Sub => { get(out, a); get(out, b); out.push(0x85); get(out, a); get(out, c); out.push(0x85); out.extend([0x83, 0x42, 0, 0x53]); trap_for(out, 4); }
            // Divide back. The division itself traps for the one product it cannot check, -1 * MIN.
            _ => {
                get(out, a);
                out.extend([0x50, 0x45, 0x04, 0x40]);
                get(out, c); get(out, a); out.push(0x7f); get(out, b); out.push(0x52);
                trap_for(out, 4);
                out.push(0x0b);
            }
        }
        get(out, c);
    }
    fn narrow(&mut self, out: &mut Vec<u8>, scalar: Scalar) {
        if scalar.narrow_index().is_none() { return; }
        let a = self.scratch();
        indexed(out, 0x22, a);
        out.push(0x42);
        signed(out, scalar.layout().maximum() as i64);
        out.push(0x56);
        trap_for(out, 5);
        get(out, a);
    }
}
enum Location {
    Local(u32),
    Memory(u32),
    Dynamic { root: u32, root_ty: Ty, index: u32 },
}
/// Tasks: unwinding, resuming, and the operations on a task's block (docs/design-async.md).
fn arithmetic(op: Binary) -> Vec<u8> {
    // One i64 temporary at local 2. Operands are evaluated once, left-to-right
    // by the caller. Arithmetic is unsigned and traps before returning overflow.
    let mut out = vec![1, 1, 0x7e];
    match op {
        Binary::Add => {
            get(&mut out, 0);
            get(&mut out, 1);
            out.push(0x7c);
            indexed(&mut out, 0x22, 2);
            get(&mut out, 0);
            out.push(0x54);
            trap_for(&mut out, 1);
            get(&mut out, 2);
        }
        Binary::Sub => {
            get(&mut out, 0);
            get(&mut out, 1);
            out.push(0x54);
            trap_for(&mut out, 2);
            get(&mut out, 0);
            get(&mut out, 1);
            out.push(0x7d);
        }
        Binary::Mul => {
            get(&mut out, 0);
            get(&mut out, 1);
            out.push(0x7e);
            indexed(&mut out, 0x21, 2);
            get(&mut out, 1);
            out.push(0x50);
            out.push(0x45);
            out.extend([0x04, 0x40]);
            get(&mut out, 2);
            get(&mut out, 1);
            out.push(0x80);
            get(&mut out, 0);
            out.push(0x52);
            trap_for(&mut out, 3);
            out.push(0x0b);
            get(&mut out, 2);
        }
        _ => unreachable!(),
    }
    out.push(0x0b);
    out
}
