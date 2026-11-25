import Witchy.StorageBlock

namespace Witchy.Memory

/-- Checked-core handles are allocation identities, never raw byte addresses.
    Allocation IDs are not reused; their metadata has its own budget. -/
abbrev Handle := Nat
abbrev LoanId := Nat

structure Limits where
  slots : Nat
  allocations : Nat
  /-- Maximum simultaneously live loans, not the number ever issued. -/
  loans : Nat
  deriving Repr, DecidableEq

abbrev Block := Storage.Block UInt64

structure Loan where
  identity : LoanId
  target : Handle
  exclusive : Bool
  deriving Repr, DecidableEq

structure StateOf (Value : Type) where
  blocks : List (Option (Storage.Block Value))
  loans : List Loan
  available : Nat
  limits : Limits
  /-- A fresh identity counter, never an index into the active table. Nat does
      not wrap; native execution is additionally bounded by the core's fuel. -/
  nextLoan : LoanId
  deriving Repr, DecidableEq

/-- Scalar compatibility specialization used by the current source executor. -/
abbrev State := StateOf UInt64

inductive Error where
  | allocation | metadata | invalidHandle | invalidLoan | borrowed
  | bounds | uninitialized | initialized | permission
  deriving Repr, DecidableEq

inductive Result (α : Type) where
  | ok (value : α)
  | error (reason : Error)
  deriving Repr, DecidableEq

inductive Access where
  | owner (handle : Handle)
  | loan (id : LoanId)
  deriving Repr, DecidableEq

/-- Host-created empty generic heap; no operation mints initial authority. -/
def initialOf (Value : Type) (limits : Limits) : StateOf Value := ⟨[], [], limits.slots, limits, 0⟩

def initial (limits : Limits) : State := ⟨[], [], limits.slots, limits, 0⟩

def block? (s : StateOf Value) (h : Handle) : Option (Storage.Block Value) := s.blocks[h]?.bind id
def loan? (s : StateOf Value) (key : LoanId) : Option Loan :=
  s.loans.find? (fun loan => loan.identity == key)

def active (s : StateOf Value) (h : Handle) : List Loan :=
  s.loans.filter (fun l => l.target == h)

def borrowable (s : StateOf Value) (h : Handle) (exclusive : Bool) : Bool :=
  if exclusive then (active s h).isEmpty else (active s h).all (fun l => !l.exclusive)

def capacity (blocks : List (Option (Storage.Block Value))) : Nat :=
  (blocks.map (fun b => b.map (fun b => b.slots.length) |>.getD 0)).sum

def quota (s : StateOf Value) : Nat := s.available + capacity s.blocks

/-- Allocation initializes no payloads; all cells begin uninitialized. -/
def allocate (s : StateOf Value) (count : Nat) : StateOf Value × Result Handle :=
  if s.blocks.length < s.limits.allocations then
    if count ≤ s.available then
      ({ s with
          blocks := s.blocks ++ [some ⟨List.replicate count none⟩]
          available := s.available - count }, .ok s.blocks.length)
    else (s, .error .allocation)
  else (s, .error .metadata)

def borrow (s : StateOf Value) (h : Handle) (exclusive : Bool) : StateOf Value × Result LoanId :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some _ =>
    if s.loans.length < s.limits.loans then
      if borrowable s h exclusive then
        ({ s with
            loans := ⟨s.nextLoan, h, exclusive⟩ :: s.loans
            nextLoan := s.nextLoan + 1 }, .ok s.nextLoan)
      else (s, .error .borrowed)
    else (s, .error .metadata)

def endLoan (s : StateOf Value) (id : LoanId) : StateOf Value × Result Unit :=
  match loan? s id with
  | none => (s, .error .invalidLoan)
  | some _ => ({ s with loans := s.loans.filter (fun loan => loan.identity != id) }, .ok ())

/-- Resolve a permission before accessing storage. Even a stale/forged numeric
    core handle cannot authorize an unchecked memory operation. Source-level
    handle opacity and affine ownership remain the frontend checker's duty. -/
def resolve (s : StateOf Value) (access : Access) (write : Bool) : Result Handle :=
  match access with
  | .owner h =>
    if (active s h).isEmpty then .ok h else .error .borrowed
  | .loan id =>
    match loan? s id with
    | none => .error .invalidLoan
    | some l =>
      if write && !l.exclusive then .error .permission else .ok l.target

def read (s : StateOf Value) (access : Access) (index : Nat) : Result Value :=
  match resolve s access false with
  | .error e => .error e
  | .ok h =>
    match block? s h with
    | none => .error .invalidHandle
    | some b =>
      match b.slots[index]? with
      | none => .error .bounds
      | some none => .error .uninitialized
      | some (some value) => .ok value

/-- Compiled form of `read`: the same permission, handle, bounds and
    initialization refusals, with the cell fetched by array index. -/
def readDense (s : StateOf Value) (access : Access) (index : Nat) : Result Value :=
  match resolve s access false with
  | .error e => .error e
  | .ok h =>
    match block? s h with
    | none => .error .invalidHandle
    | some b =>
      match Storage.Block.lookup b index with
      | none => .error .bounds
      | some none => .error .uninitialized
      | some (some value) => .ok value

@[csimp] theorem read_eq_readDense : @read = @readDense := by
  funext Value s access index
  simp only [read, readDense, Storage.Block.slots_getElem?, Storage.Block.lookup]

/-- Trusted native bridge: caller admission must prove the indexed cell exists
    and is initialized. The pure fallback only totalizes the logical model;
    native code never uses it and performs no bounds or Option-tag check. The
    native helper dispatches the private block representation then loads the
    selected object/byte/word payload by direct index; no list is traversed. -/
def slotUnchecked (block : @& Storage.Block Value) (index : @& Nat) (fallback : Value) : Value :=
  ((block.slots[index]?).bind id).getD fallback

def readUnchecked (s : StateOf Value) (access : Access) (index : Nat) (fallback : Value) : Result Value :=
  match resolve s access false with
  | .error e => .error e
  | .ok h =>
    match block? s h with
    | none => .error .invalidHandle
    | some b => .ok (slotUnchecked b index fallback)

theorem readUnchecked_initialized (s : StateOf Value) (access : Access) (handle index : Nat)
    (block : Storage.Block Value) (value fallback : Value)
    (permission : resolve s access false = .ok handle)
    (stored : block? s handle = some block)
    (initialized : block.slots[index]? = some (some value)) :
    readUnchecked s access index fallback = .ok value := by
  simp [readUnchecked, permission, stored, slotUnchecked, initialized]

/-- Initialize only empty slots; overwriting an owner is never implicit. -/
def writeEmpty (s : StateOf Value) (access : Access) (index : Nat) (value : Value) :
    StateOf Value × Result Unit :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match b.slots[index]? with
      | none => (s, .error .bounds)
      | some (some _) => (s, .error .initialized)
      | some none =>
        ({ s with blocks := s.blocks.set h (some ⟨b.slots.set index (some value)⟩) }, .ok ())

/-- Moving a payload out clears initialization; subsequent takes refuse it. -/
def take (s : StateOf Value) (access : Access) (index : Nat) : StateOf Value × Result Value :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match b.slots[index]? with
      | none => (s, .error .bounds)
      | some none => (s, .error .uninitialized)
      | some (some value) =>
        ({ s with blocks := s.blocks.set h (some ⟨b.slots.set index none⟩) }, .ok value)

/-- Scalar destruction is trivial, but loans must end before storage release.
    Resource-valued slots use releaseEmpty after checked disposition. -/
def release (s : State) (h : Handle) : State × Result Unit :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some b =>
    if (active s h).isEmpty then
      ({ s with blocks := s.blocks.set h none, available := s.available + b.slots.length }, .ok ())
    else (s, .error .borrowed)

@[simp] theorem discard_none_only (cell : Option Value) :
    cell.elim true (fun _ => false) = cell.isNone := by
  cases cell <;> rfl

/-- By default only uninitialized cells can be released. The optional raw
    disposition predicate requires evidence that accepted payloads have no live
    duty. Core uses a fixed predicate proved to reject allocation/loan owners;
    a caller-supplied predicate is not itself checked proof evidence. -/
def releaseEmpty (s : StateOf Value) (h : Handle)
    (discardable : Value → Bool := fun _ => false) : StateOf Value × Result Unit :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some block =>
    if (active s h).isEmpty then
      if block.slots.all (fun cell => cell.elim true discardable) then
        ({ s with blocks := s.blocks.set h none, available := s.available + block.slots.length }, .ok ())
      else (s, .error .initialized)
    else (s, .error .borrowed)

/-! Compiled forms of the remaining slot operations. Each is proved equal to
    its logical definition and substituted by the compiler (`@[csimp]`), so no
    compiled access path converts a block to a list. A write or take replaces
    one array cell; the array is copied first when the block is still shared
    with the previous state, and updated in place when it is not. -/

def writeEmptyDense (s : StateOf Value) (access : Access) (index : Nat) (value : Value) :
    StateOf Value × Result Unit :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match Storage.Block.lookup b index with
      | none => (s, .error .bounds)
      | some (some _) => (s, .error .initialized)
      | some none =>
        ({ s with blocks := s.blocks.set h (some (Storage.Block.replace b index (some value))) }, .ok ())

@[csimp] theorem writeEmpty_eq_writeEmptyDense : @writeEmpty = @writeEmptyDense := by
  funext Value s access index value
  simp only [writeEmpty, writeEmptyDense, Storage.Block.slots_getElem?, Storage.Block.mk_slots_set, Storage.Block.lookup, Storage.Block.replace]

def takeDense (s : StateOf Value) (access : Access) (index : Nat) : StateOf Value × Result Value :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match Storage.Block.lookup b index with
      | none => (s, .error .bounds)
      | some none => (s, .error .uninitialized)
      | some (some value) =>
        ({ s with blocks := s.blocks.set h (some (Storage.Block.replace b index none)) }, .ok value)

@[csimp] theorem take_eq_takeDense : @take = @takeDense := by
  funext Value s access index
  simp only [take, takeDense, Storage.Block.slots_getElem?, Storage.Block.mk_slots_set, Storage.Block.lookup, Storage.Block.replace]

def releaseDense (s : State) (h : Handle) : State × Result Unit :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some b =>
    if (active s h).isEmpty then
      ({ s with blocks := s.blocks.set h none, available := s.available + Storage.Block.count b }, .ok ())
    else (s, .error .borrowed)

@[csimp] theorem release_eq_releaseDense : @release = @releaseDense := by
  funext s h
  simp only [release, releaseDense, Storage.Block.slots_length, Storage.Block.count]

def releaseEmptyDense (s : StateOf Value) (h : Handle)
    (discardable : Value → Bool := fun _ => false) : StateOf Value × Result Unit :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some block =>
    if (active s h).isEmpty then
      if Storage.Block.every block (fun cell => cell.elim true discardable) then
        ({ s with blocks := s.blocks.set h none, available := s.available + Storage.Block.count block }, .ok ())
      else (s, .error .initialized)
    else (s, .error .borrowed)

@[csimp] theorem releaseEmpty_eq_releaseEmptyDense : @releaseEmpty = @releaseEmptyDense := by
  funext Value s h discardable
  simp only [releaseEmpty, releaseEmptyDense, Storage.Block.slots_length, Storage.Block.slots_all, Storage.Block.count, Storage.Block.every]
  rfl

end Witchy.Memory
