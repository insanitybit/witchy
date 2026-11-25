import Witchy.Memory
import Witchy.Authority
import Witchy.StorageScalar

namespace Witchy.Core

inductive Value where
  | number (value : UInt64)
  | boolean (value : Bool)
  | owner (handle : Memory.Handle)
  | reference (loan : Memory.LoanId)
  | capability (token : Nat)
  /-- Numeric record leaves only: this constructor can never contain a resource. -/
  | scalarAggregate (fields : List UInt64)
  deriving Repr, DecidableEq

inductive Arithmetic where
  | add | subtract | multiply | less | equal
  deriving Repr, DecidableEq

/-- Core data cannot encode a handle with a numeric constant. Owners and loans
    originate only in the allocation/borrow instructions; capabilities only in
    host roots or explicit attenuation. Register IDs are not memory addresses. -/
inductive Instruction where
  | constant (dst : Nat) (value : UInt64)
  | boolean (dst : Nat) (value : Bool)
  | copy (dst src : Nat)
  | move (dst src : Nat)
  | clear (src : Nat)
  | calculate (dst left right : Nat) (operation : Arithmetic)
  | allocate (dst count : Nat)
  | tryAllocate (tag dst error count : Nat)
  | borrow (dst src : Nat) (exclusive : Bool)
  | endLoan (src : Nat)
  | read (dst src index : Nat)
  | readUnchecked (dst src index : Nat)
  | writeEmpty (dst index value : Nat)
  | take (dst src index : Nat)
  | storeMove (dst index source : Nat)
  | takeMove (dst src index : Nat)
  | release (src : Nat)
  | narrow (dst src : Nat) (rights : List Authority.Right)
  | emit (capability value : Nat)
  | jump (destination : Nat)
  | branchFalse (condition destination : Nat)
  | halt
  | loanRoom (dst count : Nat)
  /-- Aggregate transfers carry each leaf's checked scalar width in bytes. -/
  | packMove (dst : Nat) (sources : List Nat) (widths : List Nat)
  | unpackMove (destinations : List Nat) (src : Nat) (widths : List Nat)
  /-- One raw output byte. The operand is a number register whose value must be
      below 256; nothing is truncated. -/
  | emitByte (capability value : Nat)
  /-- Read-only: is the first `length` cells of `src` a UTF-8 byte sequence? -/
  | utf8Check (dst src length : Nat)
  deriving Repr, DecidableEq

structure StateOf (Cell : Type) where
  registers : List (Option Value)
  memory : Memory.StateOf Cell
  authority : Authority.State
  deriving Repr, DecidableEq

/-- Compatibility specialization for the existing scalar source program. -/
abbrev State := StateOf UInt64

inductive Fault where
  | register | occupied | type | liveOwner | arithmetic | instruction | fuel
  | memory (reason : Memory.Error)
  | authority (reason : Authority.Error)
  deriving Repr, DecidableEq

inductive Result (α : Type) where
  | ok (value : α)
  | error (reason : Fault)
  deriving Repr, DecidableEq

instance : Monad Result where
  pure := .ok
  bind value next := match value with
    | .ok x => next x
    | .error e => .error e

/-- Cell representation for the shared executor. The compiler selects one of
    the concrete instances; source programs cannot define or select codecs.
    This computes storage behavior and grants no proof assumptions. -/
class StorageCell (Cell : Type) where
  ownedCells : Bool
  encodeValue : Value → Result Cell
  decodeValue : Cell → Value
  encodeNumber : UInt64 → Cell
  decodeNumber : Cell → Result UInt64
  release : Memory.StateOf Cell → Memory.Handle → Memory.StateOf Cell × Memory.Result Unit

instance scalarStorageCell : StorageCell UInt64 where
  ownedCells := false
  encodeValue := fun _ => .error .type
  decodeValue := .number
  encodeNumber := id
  decodeNumber := .ok
  release := Memory.release

/-- Only payloads without allocation or loan obligations can be discarded. -/
def Value.trivialRelease : Value → Bool
  | .owner _ | .reference _ => false
  | _ => true

instance resourceStorageCell : StorageCell Value where
  ownedCells := true
  encodeValue := .ok
  decodeValue := id
  encodeNumber := .number
  decodeNumber value := match value with
    | .number number => .ok number
    | _ => .error .type
  release := fun memory handle => Memory.releaseEmpty memory handle Value.trivialRelease

def fromMemory : Memory.Result α → Result α
  | .ok value => .ok value
  | .error e => .error (.memory e)

def fromAuthority : Authority.Result → Result (Option Nat)
  | .ok value => .ok value
  | .error e => .error (.authority e)

/-- Trusted host construction; no program instruction calls this function. -/
def initialOf (Cell : Type) (limits : Memory.Limits) (roots : List Authority.Token) (registers : Nat) :
    Result (StateOf Cell) :=
  if roots.length ≤ registers ∧ registers ≤ 4096 then
    .ok ⟨roots.mapIdx (fun i _ => some (.capability i)) ++
      List.replicate (registers - roots.length) none,
      Memory.initialOf Cell limits, Authority.initial roots⟩
  else .error .register

def initial (limits : Memory.Limits) (roots : List Authority.Token) (registers : Nat) : Result State :=
  initialOf UInt64 limits roots registers

def get (s : StateOf Cell) (index : Nat) : Result Value :=
  match s.registers[index]?.bind id with
  | none => .error .register
  | some value => .ok value

def vacant (s : StateOf Cell) (index : Nat) : Result Unit :=
  match s.registers[index]? with
  | some none => .ok ()
  | _ => .error .occupied

def put (s : StateOf Cell) (index : Nat) (value : Value) : Result (StateOf Cell) := do
  vacant s index
  return { s with registers := s.registers.set index (some value) }

def number (s : StateOf Cell) (index : Nat) : Result UInt64 := do
  match ← get s index with
  | .number value => return value
  | _ => .error .type

def access (s : StateOf Cell) (index : Nat) : Result Memory.Access := do
  match ← get s index with
  | .owner h => return .owner h
  | .reference id => return .loan id
  | _ => .error .type

def cap (s : StateOf Cell) (index : Nat) : Result Nat := do
  match ← get s index with
  | .capability token => return token
  | _ => .error .type

def putCopy (s : StateOf Cell) (index : Nat) (value : Value) : Result (StateOf Cell) :=
  match s.registers[index]?, value with
  | some none, .number _ | some none, .boolean _ | some none, .scalarAggregate _ =>
    .ok { s with registers := s.registers.set index (some value) }
  | some (some (.number _)), .number _ | some (some (.boolean _)), .boolean _
    | some (some (.scalarAggregate _)), .scalarAggregate _ =>
    .ok { s with registers := s.registers.set index (some value) }
  | _, _ => .error .type

/-- Preflight uses the ORIGINAL state for every operand. No mutation occurs on refusal.
    Every leaf value must fit its descriptor's range, `2^(8·width)`. -/
def scalarFieldsValid (widths : List Nat) (fields : List UInt64) : Bool :=
  decide (widths.length = fields.length) && !fields.isEmpty &&
    (widths.zip fields).all (fun (width, value) => decide (value.toNat < Storage.Scalar.bound width))

def packRegisters (s : StateOf Cell) (dst : Nat) (sources : List Nat) (widths : List Nat) : Result (StateOf Cell) := do
  if sources.Nodup ∧ dst ∉ sources then pure () else .error .register
  vacant s dst
  let fields ← sources.mapM (number s)
  if scalarFieldsValid widths fields then pure () else .error .type
  let registers := sources.foldl (fun registers index => registers.set index none) s.registers
  return { s with registers := registers.set dst (some (.scalarAggregate fields)) }

def unpackRegisters (s : StateOf Cell) (destinations : List Nat) (src : Nat) (widths : List Nat) : Result (StateOf Cell) := do
  if destinations.Nodup ∧ src ∉ destinations then pure () else .error .register
  let fields ← match ← get s src with
    | .scalarAggregate fields => pure fields
    | _ => .error .type
  if destinations.length = fields.length ∧ scalarFieldsValid widths fields then pure () else .error .type
  let _ ← destinations.mapM (vacant s)
  let registers := (destinations.zip fields).foldl
    (fun registers (index, value) => registers.set index (some (.number value))) (s.registers.set src none)
  return { s with registers }

def arithmetic (operation : Arithmetic) (a b : UInt64) : Result Value :=
  match operation with
  | .less => .ok (.boolean (a < b))
  | .equal => .ok (.boolean (a == b))
  | .add | .subtract | .multiply =>
    let n := match operation with
      | .add => a.toNat + b.toNat
      | .subtract => a.toNat - b.toNat
      | _ => a.toNat * b.toNat
    if n < 2^64 ∧ (operation ≠ .subtract ∨ b ≤ a) then .ok (.number (.ofNat n))
    else .error .arithmetic



/-- The first `length` cells as bytes, read through one lookup function. There
    is no answer unless every index is in bounds and initialized, every cell
    decodes as a number and every number is below 256. Nothing is truncated or
    defaulted. -/
def bytesBy [StorageCell Cell] (lookup : Nat → Option (Option Cell)) (length : Nat) : Option (List UInt8) :=
  (List.range length).mapM fun index =>
    match lookup index with
    | some (some cell) =>
      match StorageCell.decodeNumber cell with
      | .ok value => if value.toNat < 256 then some value.toUInt8 else none
      | .error _ => none
    | _ => none

/-- THE definition of "these cells hold UTF-8": the byte prefix exists and Lean's
    own `ByteArray.validateUTF8` accepts it. The source formula, the observing
    instruction and the packed executable path all go through this function. -/
def utf8By [StorageCell Cell] (lookup : Nat → Option (Option Cell)) (size length : Nat) : Bool :=
  -- A length beyond the block is refused before any work proportional to it.
  if length ≤ size then
    match bytesBy lookup length with
    | some bytes => ByteArray.validateUTF8 ⟨bytes.toArray⟩
    | none => false
  else false

def utf8Cells [StorageCell Cell] (slots : List (Option Cell)) (length : Nat) : Bool :=
  utf8By (fun index => slots[index]?) slots.length length

/-- Read-only observation of the block an owner or loan currently resolves to. -/
def utf8ObserveWith (check : Storage.Block Cell → Nat → Bool) (memory : Memory.StateOf Cell)
    (access : Memory.Access) (length : Nat) : Result Bool :=
  match Memory.resolve memory access false with
  | .error reason => .error (.memory reason)
  | .ok handle =>
    match Memory.block? memory handle with
    | none => .error (.memory .invalidHandle)
    | some block => .ok (check block length)

def utf8Observe [StorageCell Cell] (memory : Memory.StateOf Cell) (access : Memory.Access) (length : Nat) : Result Bool :=
  utf8ObserveWith (fun block length => utf8Cells block.slots length) memory access length

/-- The observation answers exactly `utf8Cells` of the current resolved block. -/
theorem utf8_observe_cells [StorageCell Cell] (memory : Memory.StateOf Cell) (access : Memory.Access)
    (handle length : Nat) (block : Storage.Block Cell)
    (permission : Memory.resolve memory access false = .ok handle)
    (stored : Memory.block? memory handle = some block) :
    utf8Observe memory access length = .ok (utf8Cells block.slots length) := by
  simp only [utf8Observe, utf8ObserveWith, permission, stored]

theorem utf8_observe_no_register_fault [StorageCell Cell] (memory : Memory.StateOf Cell) (access : Memory.Access)
    (length : Nat) : utf8Observe memory access length ≠ .error .register := by
  unfold utf8Observe utf8ObserveWith
  split
  · simp
  · split <;> simp

end Witchy.Core
