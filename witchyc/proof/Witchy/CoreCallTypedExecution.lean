import Witchy.CoreCallExecution
import Witchy.CoreCallStorageCoverage
import Witchy.TypedStorageRuntime
import Witchy.NativeMemory

namespace Witchy.Core.Call

/-- Check a physical header only after resolving the existing logical permission.
    Invalid references are still rejected by the original Core primitive. -/
def physicalAccess (element : Storage.Element) (s : StateOf Value) (instruction : Core.Instruction) : Bool :=
  let source := match instruction with
    | .read _ src _ | .readUnchecked _ src _ | .take _ src _ | .takeMove _ src _ => some src
    | .writeEmpty src _ _ | .storeMove src _ _ | .release src => some src
    | _ => none
  match source with
  | none => true
  | some src =>
    match access s src with
    | .error _ => true
    | .ok permission =>
      match Memory.resolve s.memory permission false with
      | .error _ => true
      | .ok handle =>
        match Memory.block? s.memory handle with
        | none => true
        | some block =>
          Storage.blockMatchesElement block element &&
            match instruction with
            | .writeEmpty _ _ value | .storeMove _ _ value =>
              match get s value with
              | .error _ => true
              | .ok value => Storage.blockAccepts block value
            | _ => true

theorem physicalAccess_eq (element : Storage.Element) (s : StateOf Value) (instruction : Core.Instruction) :
    physicalAccess element s instruction = true := by
  cases instruction <;> simp only [physicalAccess, Storage.blockMatchesElement_eq]
  all_goals repeat' first | rfl | split

/-- The same observation through the physical block lookup, so a packed block
    is never materialized as a list. -/
def utf8ObserveNative (memory : Memory.StateOf Value) (access : Memory.Access) (length : Nat) : Result Bool :=
  utf8ObserveWith (fun block length => utf8By (fun index => Storage.Native.lookup block index) (Storage.Native.count block) length)
    memory access length

theorem utf8By_native (block : Storage.Block Value) (length : Nat) :
    utf8By (fun index => Storage.Native.lookup block index) (Storage.Native.count block) length =
      utf8Cells block.slots length := by
  simp only [utf8Cells, Storage.Native.lookup, Storage.Block.lookup, Storage.Native.count,
    Storage.Block.count, Storage.Block.slots_getElem?, Storage.Block.slots_length]

theorem utf8ObserveNative_eq (memory : Memory.StateOf Value) (access : Memory.Access) (length : Nat) :
    utf8ObserveNative memory access length = utf8Observe memory access length := by
  simp only [utf8ObserveNative, utf8Observe, utf8By_native]

/-- Original local procedure/PC selects a descriptor; relocated operands select
    register effects. Pure definitions erase this physical representation choice. -/
def storageStep (plan : StoragePlan) (procedure pc : Nat) (s : StateOf Value)
    (instruction : Core.Instruction) : Result (StateOf Value) := do
  if physicalAccess (plan procedure pc) s instruction then pure () else .error .type
  match instruction with
  | .allocate dst count =>
    vacant s dst
    let count ← number s count
    let (memory, outcome) := Storage.allocate s.memory count.toNat (plan procedure pc)
    let handle ← fromMemory outcome
    put { s with memory } dst (.owner handle)
  | .tryAllocate tag dst error count =>
    if tag = dst ∨ tag = error ∨ dst = error then .error .instruction else pure ()
    vacant s dst
    let _ ← putCopy s tag (.boolean false)
    let _ ← putCopy s error (.number 0)
    let count ← number s count
    let (memory, outcome) := Storage.allocate s.memory count.toNat (plan procedure pc)
    match outcome with
    | .ok handle =>
      let next ← put { s with memory } dst (.owner handle)
      putCopy next tag (.boolean true)
    | .error reason =>
      let code : Option UInt64 := match reason with
        | .allocation => some 0
        | .metadata => some 1
        | _ => none
      match code with
      | none => .error (.memory reason)
      | some code =>
        let next ← putCopy s error (.number code)
        putCopy next tag (.boolean false)
  | .read dst src index => do
    let cell ← fromMemory (Memory.Native.read s.memory (← access s src) (← number s index).toNat)
    let value ← StorageCell.decodeNumber cell
    putCopy s dst (.number value)
  | .utf8Check dst src length => do
    let valid ← utf8ObserveNative s.memory (← access s src) (← number s length).toNat
    putCopy s dst (.boolean valid)
  | .readUnchecked dst src index => do
    let cell ← fromMemory (Memory.Native.readUnchecked s.memory (← access s src) (← number s index).toNat (StorageCell.encodeNumber 0))
    let value ← StorageCell.decodeNumber cell
    putCopy s dst (.number value)
  | .writeEmpty dst index value => do
    let (memory, outcome) := Memory.Native.writeEmpty s.memory (← access s dst)
      (← number s index).toNat (StorageCell.encodeNumber (← number s value))
    let _ ← fromMemory outcome
    return { s with memory }
  | .take dst src index => do
    -- A non-copyable destination is never overwritten, even after a memory take.
    match s.registers[dst]? with
    | some none | some (some (.number _)) => pure ()
    | _ => .error .type
    let (memory, outcome) := Memory.Native.take s.memory (← access s src) (← number s index).toNat
    let cell ← fromMemory outcome
    let value ← StorageCell.decodeNumber cell
    putCopy { s with memory } dst (.number value)
  | .storeMove dst index source => do
    if StorageCell.ownedCells (Cell := Value) then pure () else .error .type
    let value ← get s source
    match value with
    | .owner handle =>
      if !(Memory.active s.memory handle).isEmpty then .error .liveOwner else pure ()
    | _ => pure ()
    let permission ← access s dst
    let offset ← number s index
    let target ← fromMemory (Memory.resolve s.memory permission true)
    match value with
    | .owner handle => if handle = target then .error .liveOwner else pure ()
    | _ => pure ()
    let cell ← StorageCell.encodeValue value
    let (memory, outcome) := Memory.Native.writeEmpty s.memory permission offset.toNat cell
    let _ ← fromMemory outcome
    return { s with memory, registers := s.registers.set source none }
  | .takeMove dst src index => do
    if StorageCell.ownedCells (Cell := Value) then pure () else .error .type
    vacant s dst
    let (memory, outcome) := Memory.Native.take s.memory (← access s src) (← number s index).toNat
    let cell ← fromMemory outcome
    let value := StorageCell.decodeValue cell
    match value with
    | .owner handle =>
      if !(Memory.active memory handle).isEmpty then .error .liveOwner else pure ()
    | _ => pure ()
    put { s with memory } dst value
  | .release src => do
    match ← get s src with
    | .owner h =>
      let (memory, outcome) := Memory.Native.releaseEmpty s.memory h Core.Value.trivialRelease
      let _ ← fromMemory outcome
      return { s with memory, registers := s.registers.set src none }
    | _ => .error .type
  | instruction => Core.step s instruction

theorem storageStep_eq (plan : StoragePlan) (procedure pc : Nat) (s : StateOf Value)
    (instruction : Core.Instruction) : storageStep plan procedure pc s instruction = Core.step s instruction := by
  cases instruction <;> simp [storageStep, physicalAccess_eq, Storage.allocate_eq, Memory.Native.read_eq, Memory.Native.readUnchecked_eq, Memory.Native.writeEmpty_eq, Memory.Native.take_eq, Memory.Native.releaseEmpty_eq, utf8ObserveNative_eq, Core.step]
  all_goals rfl

def runStorage (plan : StoragePlan) (program : Array Procedure) : Nat → Machine → Machine × Result Unit
  | 0, machine => (machine, .error .fuel)
  | fuel + 1, machine =>
    match advance program machine (storageStep plan) with
    | .error reason => (machine, .error reason)
    | .ok none => (machine, .ok ())
    | .ok (some next) => runStorage plan program fuel next

theorem runStorage_eq (plan : StoragePlan) (program : Array Procedure) (fuel : Nat) (machine : Machine) :
    runStorage plan program fuel machine = run program fuel machine := by
  have hook : storageStep plan = fun _ _ => Core.step := by funext p pc s i; exact storageStep_eq plan p pc s i
  induction fuel generalizing machine with
  | zero => rfl
  | succ fuel ih => simp only [runStorage, run, hook]; split <;> simp_all

end Witchy.Core.Call
