//! Primary semantic typed body: logical bindings and nominal types, never physical slots.
use super::{scalar::Scalar, logical_place::Place};
use std::collections::BTreeSet;
pub(super) use super::operations::{Binary, Bits, Float, Signed, StorageOp};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Ty {
    Unit,
    Param { owner: FunctionId, index: u32 },
    Unsigned(Scalar),
    /// Two's-complement 64-bit integer.
    I64,
    /// IEEE 754 binary64 (docs/wasm-f64.md).
    F64,
    Bool,
    Console,
    Input,
    Allocator,
    /// `Dir<Read>`, `File<Write>` and the rest: a handle the host issued (host.rs).
    Host(super::host::Host),
    Buffer(u32),
    Boxed(u32),
    Weak(u32),
    Aggregate(u32),
    Sum(u32),
    /// `[T]`: a run of initialized elements, only ever behind a reference (`&[T]`); the
    /// number indexes the slice types (semantic_types `slices`).
    Slice(u32),
    Borrow { mutable: bool, pointee: Pointee },
}
impl Ty {
    pub const U64: Self = Self::Unsigned(Scalar::U64);
}

#[derive(Clone, Debug)]
pub(super) struct Expr {
    pub ty: Ty,
    pub kind: Value,
}

#[derive(Clone, Debug)]
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
    /// A temporary that is borrowed: the value is stored in its own local and the result is that local's address.
    Stored(Place, Box<Expr>),
    Aggregate(Vec<Expr>),
    Sum { arm: u8, payload: Box<Expr> },
    Repeat(Box<Expr>, u32),
    /// A value of one anonymous sum as a value of a wider one: for each tag of the source, its tag in the result.
    Retag(Box<Expr>, Vec<u8>),
    Project(Box<Expr>, u32),
    Index(Box<Expr>, Box<Expr>, u32),
    /// `a.slice()`: the whole fixed array at the place, of this many elements, as `&[T]`.
    SliceOf(Place, u32),
    /// `s.len()`: how many elements a slice has.
    SliceLen(Box<Expr>),
    /// A reference to element `i`, reached as `Reach` says.
    SliceAt(Box<Expr>, Box<Expr>, Reach),
    /// `s[i]`: a copy of element `i`; past the end traps, unless `false`: the index is a counter
    /// of `for i in a..s.len()`, below the length read once (iteration.rs `COUNTED`).
    SliceIndex(Box<Expr>, Box<Expr>, bool),
    /// `s.part(from, to)`: elements `from..to`; `from > to` or `to > len` traps.
    /// Read eight bytes from an initialized byte slice, in little-endian order.
    SliceReadLe64(Box<Expr>, Box<Expr>),
    SlicePart(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `s.part_proved(from, to)`: as `SlicePart`, but where the function's proof over numbers
    /// shows `from <= to <= s.len()` (every slice site of the function shown: `SLICE_PROVED`)
    /// no range test is emitted and nothing traps. On text an end inside a scalar gives the
    /// empty text at `from`, never a trap. Without the proof it keeps `SlicePart`'s range test.
    SliceWithin(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `target.copy_from(source)`: every element of the shared slice written over the unique
    /// slice's, in order; lengths that differ trap. Elements are one plain scalar each.
    SliceCopy(Box<Expr>, Box<Expr>),
    Widen(Box<Expr>),
    ProofUtf8(Box<Expr>),
    /// An operation on I64 values: one operand for the two conversions, two for the rest.
    Signed(Signed, Vec<Expr>),
    /// An operation on F64 values: one operand for the conversions and functions, two for the rest.
    Float(Float, Vec<Expr>),
    Bits(Bits, Vec<Expr>),
    BoxOp(super::operations::BoxOp, u32, Vec<Expr>),
    Not(Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
    Call(InstanceCall),
    Print(Place, Box<Expr>, bool),
    /// The next byte of input, or 256 when it has ended. The place is the `Console<Read>`.
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
    TaskSpawn { allocator: Box<Expr>, bytes: Box<Expr>, entry: Box<Expr> },
    /// Run a task (`&mut Task`) until it finishes, `Ok(())`, or pauses, `Err(token)`.
    TaskStep(Box<Expr>, Box<Expr>),
    TaskStatus(Box<Expr>),
    TaskWord,
    /// Whether code is running inside a task (`memory.in_task()`).
    InTask,
    /// Give back a finished task's memory; one that has not finished traps.
    TaskFinish(Box<Expr>),
    /// End this step of the running task, waiting for `token`.
    Pause(Box<Expr>),
}

/// Authored loop invariants, resolved for whichever proof model checks the function.
#[derive(Clone, Debug, Default)]
pub(super) struct Invariants {
    pub memory: Vec<super::memory_contract::Predicate>,
    pub record: Vec<super::record_contract::Predicate>,
    /// Clauses over the scalar locals in scope, by local id.
    pub scalar: Vec<super::contract::Prop>,
    /// Where the loop is written (its condition, or a `for`'s range): for a refusal's place only.
    pub at: Option<(usize, usize)>,
}
#[derive(Clone, Debug)]
pub(super) enum Stmt {
    Set(u32, Expr),
    StorePlace(Place, Option<Binary>, Expr),
    Eval(Expr),
    /// The value, the locals that end here, and where the `return` is written (its value's
    /// place; `None` for one the checker wrote or a bare `return`): for a refusal's place only.
    Return(Expr, Vec<u32>, Option<(usize, usize)>),
    Expire(Vec<u32>),
    Replace(u32, Place, Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>, Invariants),
    Match { value: Expr, arms: Vec<Arm> },
    /// Leave, or restart, the innermost loop. Never inside a function with a contract.
    Break,
    Continue,
    /// Stop the run with a trap. A supported proof model must exclude this path.
    /// Where it was written and what wrote it, for a refusal of that proof (`ast::Site`).
    Trap(crate::ast::Site),
}

/// A proof covers this effect only when every explicit trap in the typed body
/// contributed its own path-to-False obligation. An unvisited site fails closed.
pub(super) fn traps_covered(body: &[Stmt], sites: &BTreeSet<usize>) -> bool {
    body.iter().all(|stmt| match stmt {
        Stmt::Trap(_) => sites.contains(&(stmt as *const Stmt as usize)),
        Stmt::If(_, yes, no) => traps_covered(yes, sites) && traps_covered(no, sites),
        Stmt::While(_, body, _) => traps_covered(body, sites),
        Stmt::Match { arms, .. } => arms.iter().all(|arm| traps_covered(&arm.body, sites)),
        Stmt::Set(..) | Stmt::StorePlace(..) | Stmt::Eval(_) | Stmt::Return(..)
        | Stmt::Expire(_) | Stmt::Replace(..) | Stmt::Break | Stmt::Continue => true,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct FunctionId(pub u32);
#[derive(Clone, Debug)]
pub(super) struct InstanceCall {
    pub definition: FunctionId,
    pub instance: Option<u32>,
    pub contract_site: Option<u32>,
    pub substitutions: Vec<Ty>,
    pub args: Vec<Expr>,
    pub result: Ty,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct LogicalLocal {
    pub id: u32,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Option<(usize, usize)>,
}
#[derive(Clone, Debug)]
pub(super) struct Arm { pub payload: Ty, pub binding: Option<u32>, pub body: Vec<Stmt> }
#[derive(Clone, Debug)]
pub(super) struct Function {
    pub definition: FunctionId,
    pub type_parameters: Vec<TypeParameter>,
    pub calls: Vec<CallEdge>,
    pub contract: Option<super::contract::Contract>,
    pub name: String,
    pub checked_nodes: usize,
    pub checks: u32,
    pub params: Vec<Ty>,
    pub result: Ty,
    pub locals: Vec<LogicalLocal>,
    /// The source name of each named local, by id (a refused proof names what it assigns).
    pub local_names: std::collections::BTreeMap<u32, String>,
    pub body: Vec<Stmt>,
    pub addressed: BTreeSet<u32>,
    pub element_limits: Vec<Ty>,
    /// A trait method: no body. Calls are replaced by the implementing type's method.
    pub required: bool,
    /// `nopanic fn`: refused unless what is emitted for it cannot trap (traps.rs); the keyword's place.
    pub nopanic: Option<std::ops::Range<usize>>,
    /// The file it was written in (an index into the bundle's files).
    pub module: u32,
    /// `pure fn`: for a trait's method, one a contract may read as a value of its receiver.
    pub pure: bool,
    /// `suspends`: it may pause, so it runs only on a task's stack, never `main`'s.
    pub suspends: bool,
    /// The counters of counting `for` loops: each written only by `+= 1` right after
    /// `counter < end` held, so that step is emitted without an overflow check.
    pub steps: BTreeSet<u32>,
    /// The hidden locals of owner assignments over a field (check.rs `vacant_assign`): each
    /// names the three statements the checker wrote for one assignment. No source can make one.
    pub overwrites: BTreeSet<u32>,
    /// Where each call with a contract is written, by its `contract_site`: for a refusal's line only.
    pub call_spans: Vec<(usize, usize)>,
    /// Where its name is written: for the place of a refusal of its contract that has none.
    pub span: (usize, usize),
}
#[derive(Clone, Copy, Debug)]
pub(super) enum ParameterFamily {
    /// Copyable, droppable storage values.
    CopyStorage,
    /// Any storable value, owners included; the body moves each one exactly once.
    AnyStorage,
}
#[derive(Clone, Debug)]
/// `work`: bounded by `WitchyOnce0` (work to run once, `race`'s `A`): given authority by value.
///
/// `bounded`: it has a trait bound (or is a trait method's `Self`): code generic in it can still
/// reach, through an impl for one type, what is of that type (an impl for `U64` may answer a
/// `&mut U64` into any record it is given), so it is not opaque to which fields a result reaches.
pub(super) struct TypeParameter { pub ty: Ty, pub name: String, pub family: ParameterFamily, pub work: bool, pub bounded: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CallEdge { pub definition: FunctionId, pub substitutions: Vec<Ty> }
#[derive(Clone, Debug)]
pub(super) struct Program {
    pub assertion_sites: std::collections::BTreeMap<(usize,usize),(u32,String,String)>,
    pub functions: Vec<Function>,
    pub main: FunctionId,
    pub types: super::semantic_types::Types,
    /// Per function, what the suspension check reads (effects.rs).
    pub effects: Vec<super::effects::Facts>,
    /// `(trait, type)` for each `impl Trait for Type` (`#Trait` for a blanket impl).
    pub implements: Vec<(String, String)>,
    /// `main`'s `Secret<Seal>` and `Secret<Reveal>` parameters in order: each one's name and
    /// right, the module's `witchy.secret-slots` section (`host::secret_slot`).
    pub secret_slots: Vec<(String, u8)>,
    /// How many of `main`'s first locals are its parameters, the grants of a run included
    /// (`caps::main_grant`): what a run of the program can be granted.
    pub main_parameters: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Pointee {
    Param(FunctionId, u32),
    Unsigned(Scalar),
    I64,
    F64,
    Bool,
    Console,
    Input,
    Allocator,
    /// `Dir<Read>`, `File<Write>` and the rest: a handle the host issued (host.rs).
    Host(super::host::Host),
    Buffer(u32),
    Boxed(u32),
    Weak(u32),
    Aggregate(u32),
    Sum(u32),
    Slice(u32),
}
impl Pointee {
    pub fn from_ty(ty: Ty) -> Option<Self> {
        match ty {
            Ty::Param { owner, index } => Some(Self::Param(owner,index)),
            Ty::Unsigned(s) => Some(Self::Unsigned(s)),
            Ty::I64 => Some(Self::I64),
            Ty::F64 => Some(Self::F64),
            Ty::Bool => Some(Self::Bool),
            Ty::Console => Some(Self::Console),
            Ty::Input => Some(Self::Input),
            Ty::Allocator => Some(Self::Allocator),
            Ty::Host(h) => Some(Self::Host(h)),
            Ty::Weak(id) => Some(Self::Weak(id)),
            Ty::Boxed(id) => Some(Self::Boxed(id)),
            Ty::Buffer(id) => Some(Self::Buffer(id)),
            Ty::Aggregate(index) => Some(Self::Aggregate(index)),
            Ty::Sum(index) => Some(Self::Sum(index)),
            Ty::Slice(index) => Some(Self::Slice(index)),
            _ => None,
        }
    }
    pub fn ty(self) -> Ty {
        match self { Self::Param(owner,index)=>Ty::Param{owner,index}, Self::Unsigned(s) => Ty::Unsigned(s), Self::I64 => Ty::I64, Self::F64 => Ty::F64, Self::Bool => Ty::Bool, Self::Console => Ty::Console, Self::Input => Ty::Input, Self::Allocator => Ty::Allocator, Self::Host(h) => Ty::Host(h), Self::Buffer(id) => Ty::Buffer(id), Self::Boxed(id) => Ty::Boxed(id), Self::Weak(id) => Ty::Weak(id), Self::Aggregate(index) => Ty::Aggregate(index), Self::Sum(index) => Ty::Sum(index), Self::Slice(index) => Ty::Slice(index) }
    }
}

/// How a slice's element `i` is reached (`Value::SliceAt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// `s.at(i)`: a `Result<&T, U64>`, `Err(i)` past the end.
    Result,
    /// `s.cell(i)`: the `&T` itself; past the end traps.
    Trap,
    /// The element of a `for` over a shared slice (iteration.rs, `Way::Slice`): the slice was
    /// bound once to a local nothing can change, its length read once, and `i` counts below it,
    /// so `i` is in range by construction and there is nothing to test. Made only there.
    Walk,
}
