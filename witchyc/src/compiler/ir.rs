//! A closed typed IR with checked places and explicit ownership transfers.
//! Console is opaque affine authority supplied by the private main entry.
use super::{scalar::Scalar, place::Place};
pub(super) use super::operations::{Binary, Bits, Float, Signed, StorageOp};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Ty {
    Unit,
    Unsigned(Scalar),
    /// Two's-complement 64-bit integer.
    I64,
    /// IEEE 754 binary64 (docs/wasm-f64.md).
    F64,
    Bool,
    Console,
    /// `Console<Read>`: authority to take bytes of the run's input.
    Input,
    Allocator,
    /// `Dir<Read>`, `File<Write>` and the rest: a handle the host issued (host.rs).
    Host(super::host::Host),
    Buffer(u32),
    Boxed,
    Aggregate(u32),
    Sum(u32),
    /// Private physical tag carrier; never a source Bool or initialized payload.
    Tag,
    /// `[T]`, behind a reference only: the number indexes the layouts' slice types.
    Slice(u32),
    Borrow { mutable: bool, pointee: super::borrow::Pointee },
}
impl Ty {
    pub const U64: Self = Self::Unsigned(Scalar::U64);
}

#[derive(Debug)]
pub(super) struct Expr {
    pub ty: Ty,
    pub kind: Value,
}

#[derive(Debug)]
pub(super) enum Value {
    /// Immutable UTF-8 bytes stored for the lifetime of the executable.
    StaticStr(std::sync::Arc<str>),
    Unit,
    U64(u64),
    Bool(bool),
    ReadPlace(Place),
    EntryConsole,
    EntryInput,
    EntryAllocator,
    /// One of `main`'s host capabilities: a directory with one right, the clock, randomness;
    /// with the number its family's authorizer is given (`Host::code`, or `host::secret_slot`).
    EntryHost(super::host::Host, u64),
    TryAlloc { allocator: Box<Expr>, capacity: Box<Expr>, type_id: u32, prefix_only: bool },
    Capacity(Box<Expr>, u32),
    /// Read the incarnation-bearing token as inert data, under a shared loan.
    AllocationKey(Box<Expr>, u32),
    TryRelease(Box<Expr>, u32),
    Storage { op: StorageOp, receiver: Box<Expr>, index: Box<Expr>, value: Option<Box<Expr>>, buffer: u32 },
    Move { slot: u32, place: Place },
    /// Return sole owner field while consuming entire wrapper's logical availability.
    Extract { wrapper: u32, place: Place },
    Discard(Box<Expr>),
    Address(Place),
    Stored(Place, Box<Expr>),
    Aggregate(Vec<Expr>),
    Sum { arm: u8, payload: Box<Expr> },
    Repeat(Box<Expr>, u32),
    /// A value of one anonymous sum as a value of a wider one: for each tag of the source, its tag in the result.
    Retag(Box<Expr>, Vec<u8>),
    Project(Box<Expr>, u32),
    Index(Box<Expr>, Box<Expr>, u32),
    /// A slice's element `i`, by address; then the stride, and how it is reached.
    SliceAt(Box<Expr>, Box<Expr>, u32, super::logical_body::Reach),
    /// A copy of a slice's element `i` (past the end traps); then the stride.
    /// Whether the index is tested: `false` below the length by construction (logical_body).
    SliceIndex(Box<Expr>, Box<Expr>, u32, bool),
    /// A slice's elements `from..to` (both checked), as a slice; then the stride.
    /// Read eight bytes from an initialized byte slice, in little-endian order.
    SliceReadLe64(Box<Expr>, Box<Expr>),
    SlicePart(Box<Expr>, Box<Expr>, Box<Expr>, u32, bool),
    /// `logical_body::Value::SliceWithin`, with the element's stride and whether it is text.
    SliceWithin(Box<Expr>, Box<Expr>, Box<Expr>, u32, bool),
    /// `target.copy_from(source)`, with the element's stride.
    SliceCopy(Box<Expr>, Box<Expr>, u32),
    Widen(Box<Expr>),
    /// An operation on I64 values: one operand for the two conversions, two for the rest.
    Signed(Signed, Vec<Expr>),
    /// An operation on F64 values.
    Float(Float, Vec<Expr>),
    Bits(Bits, Vec<Expr>),
    BoxOp(super::operations::BoxOp, u32, Vec<Expr>),
    Not(Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
    Call(u32, Vec<Expr>),
    Print(Place, Box<Expr>, bool),
    /// The next byte of input, or 256 when it has ended; or (`true`) a run of up to seven bytes
    /// with their count in the top byte. The place is the `Console<Read>`.
    Read(Place, bool),
    /// A host operation: the handle read from the place or given as the first operand, then
    /// one number. The handle's type says which operations are possible (host.rs).
    Host(super::host::HostOp, Option<Place>, Vec<Expr>),
    /// `memory.exchange(&mut a, &mut b)`: the two places exchange their values; both stay initialized.
    Exchange(Place, Place),
    /// Tasks (library/wasm-tasks.wy, docs/design-async.md). `TaskSpawn` makes a block of memory
    /// holding a closure, its argument, a save area and a stack, from the call that will start it:
    /// `entry` is that call, `work.call(input)`, whose arguments are evaluated and kept, not run.
    /// The result is `Result<Task, A>`, the argument handed back when there is no memory.
    /// `buffer` is the type id the block is allocated as (Buffer<U64>).
    TaskSpawn { allocator: Box<Expr>, bytes: Box<Expr>, entry: Box<Expr>, buffer: u32 },
    /// Run a task (`&mut Task`) until it finishes, `Ok(())`, or pauses, `Err(token)`.
    /// Runs a task, handing a paused one the word its pause returns.
    TaskStep(Box<Expr>, Box<Expr>),
    /// A task's state: 0 not yet stepped, 1 paused, 2 finished.
    TaskStatus(Box<Expr>),
    /// The word the running task's latest step handed in, read before it pauses: at its start,
    /// CANCELLED when it was cancelled before it began. Outside a task, 0.
    TaskWord,
    /// Whether code is running inside a task (`memory.in_task()`).
    InTask,
    /// Give back a finished task's memory; one that has not finished traps.
    TaskFinish(Box<Expr>, u32),
    /// End this step of the running task, waiting for `token`.
    Pause(Box<Expr>),
}

#[derive(Debug)]
pub(super) enum Stmt {
    Set(u32, Expr),
    StorePlace(Place, Option<Binary>, Expr),
    Eval(Expr),
    Return(Expr, Vec<u32>),
    Expire(Vec<u32>),
    Replace(u32, Place, Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Match { value: Expr, arms: Vec<super::match_arm::Arm> },
    Break,
    Continue,
    /// Stop the run: Wasm `unreachable`.
    Trap,
    /// A match on a stored sum through a reference: each arm binds the address of its
    /// variant's value, `offsets[tag]` bytes into the sum.
    /// `tag`: in a function that may pause, the local the stored tag is read into once, so going
    /// back in takes the arm that was taken even if the arm changed the variant (pause.rs).
    MatchRef { value: Expr, offsets: Vec<u32>, arms: Vec<super::match_arm::Arm>, tag: Option<u32> },
}

#[derive(Debug)]
pub(super) struct Function {
    pub params: Vec<Ty>,
    pub result: Ty,
    /// Includes parameters first, followed by all lexically scoped locals.
    pub locals: Vec<Ty>,
    pub body: Vec<Stmt>,
    pub frame: super::frame::Frame,
    /// Counting-`for` counters, by local: their `+= 1` cannot overflow (logical_body::Function::steps).
    pub steps: std::collections::BTreeSet<u32>,
    /// The hidden locals of owner assignments over a field, by first slot
    /// (logical_body::Function::overwrites): what `vacant_store` may write as one store.
    pub overwrites: std::collections::BTreeSet<u32>,
}

#[derive(Debug)]
pub(super) struct Program {
    pub functions: Vec<Function>,
    pub main: u32,
    pub output: bool,
    /// Whether the program names `Console<Read>`: it then imports the input function.
    pub input: bool,
    /// Per host family (`Family::ALL` order): whether an emitted function holds one of its
    /// values, and so whether its imports are present.
    pub host: [bool; super::host::Family::ALL.len()],
    pub layouts: super::layout::Layouts,
    pub semantic_receipt: String,
    pub selected_definitions: Vec<(String,usize)>,
    pub instance_receipt: String,
    /// Each function's name, as source writes it, with its type arguments (wasm-traps).
    pub names: Vec<String>,
    /// Which functions may pause, and the kinds of task (pause.rs).
    pub tasks: super::pause::Tasks,
    /// Each function's source definition.
    pub definitions: Vec<u32>,
    /// `main`'s secret parameters, by name and right, in slot order (logical_body.rs).
    pub secret_slots: Vec<(String, u8)>,
    /// Per function: its proof shows every unsigned `+ - *` in range, so they are emitted
    /// without checks (scalar_vc.rs). Set only once that proof has been accepted.
    pub proved_arithmetic: Vec<bool>,
    /// Per function: its proof shows every array index below its length, so the bounds checks
    /// are not emitted (scalar_vc.rs, `INDEX_PROVED`). Set only once that proof is accepted.
    pub proved_index: Vec<bool>,
    /// Per definition: every slice read was proved in range (`SLICE_PROVED`); set once accepted.
    pub proved_slices: Vec<bool>,
    /// Every explicit Trap has its own kernel-checked impossible-path obligation.
    pub proved_unreachable: Vec<bool>,
}

impl Program {
    pub fn frame_profile(&self) -> bool {
        self.layouts.static_text || self.functions.iter().any(|f| f.frame.bytes != 0 || f.params.iter().any(|t| matches!(t, Ty::Borrow { .. })))
    }
}
