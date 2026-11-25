//! One canonical function/type index assignment shared by encoding and receipts.
use super::ir::Program;

pub(super) struct Indices {
    pub print: Option<u32>,
    /// The byte output import; present exactly when `print` is.
    pub print_byte: Option<u32>,
    /// Eight bytes, lowest first, in one call (`console.print_word`): the third output import.
    pub print_word: Option<u32>,
    /// Up to seven bytes of input in one call (`input.next_run()`): the second input import.
    pub read_run: Option<u32>,
    /// The input import: after the three output imports, before the allocator's.
    pub read: Option<u32>,
    /// Per host family: its authorizer's index, followed by one import per operation.
    pub host: [Option<u32>; super::host::Family::ALL.len()],
    pub authorize: Option<u32>,
    pub source: u32,
    pub type_info: u32,
    pub validate: u32,
    pub try_alloc: u32,
    pub capacity: u32,
    pub try_release: u32,
    pub slot_prepare: u32,
    /// The range modes of `slot_prepare`, a helper of their own.
    pub range_prepare: u32,
    pub authorize_type: u32,
    pub helper_types: u32,
    /// Type indices for host operations with arities 3, 4, and 5.
    pub extended_host_types: [Option<u32>; 3],
}
/// Output imports, together or none: a number and a newline, one byte, eight bytes.
const OUTPUT: u32 = 3;
/// Input imports, together or none: one byte, a run of up to seven.
const INPUT: u32 = 2;
impl Indices {
    pub fn new(program: &Program) -> Self {
        let heap = program.layouts.uses_heap();
        let print = program.output.then_some(0);
        let print_byte = program.output.then_some(1);
        let print_word = program.output.then_some(2);
        let read = program.input.then_some(OUTPUT * u32::from(program.output));
        let read_run = program.input.then_some(OUTPUT * u32::from(program.output) + 1);
        let mut next = OUTPUT * u32::from(program.output) + INPUT * u32::from(program.input);
        let mut host = [None; super::host::Family::ALL.len()];
        for family in super::host::Family::ALL {
            if !program.host[family.ordinal()] { continue; }
            host[family.ordinal()] = Some(next);
            next += family.imports();
        }
        let host_imports = next - OUTPUT * u32::from(program.output) - INPUT * u32::from(program.input);
        let authorize = heap.then_some(OUTPUT * u32::from(program.output) + INPUT * u32::from(program.input) + host_imports);
        let source = OUTPUT * u32::from(program.output) + INPUT * u32::from(program.input) + host_imports + u32::from(authorize.is_some());
        let arithmetic = source + program.functions.len() as u32;
        let type_info = arithmetic + 6;
        let authorize_type = 3 + program.functions.len() as u32;
        let mut next_type = authorize_type + if heap { 1 + super::allocator::HELPERS } else { 0 };
        let extended_host_types = [3, 4, 5].map(|arity| {
            super::host::Family::ALL.iter().any(|family| {
                program.host[family.ordinal()] && family.ops().iter().any(|op| op.arity() == arity)
            }).then(|| {
                let index = next_type;
                next_type += 1;
                index
            })
        });
        Self { print, print_byte, print_word, read_run, read, host, authorize, source, type_info, validate: type_info + 1,
            try_alloc: type_info + 2, capacity: type_info + 3, try_release: type_info + 4, slot_prepare: type_info + 5, range_prepare: type_info + 6,
            authorize_type, helper_types: authorize_type + 1, extended_host_types }
    }
}
