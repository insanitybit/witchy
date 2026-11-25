import Witchy.Core

namespace Witchy.Core

/-- One operational path, using the same Memory and Authority definitions that
    the proofs describe. This is dynamically checked core execution, not yet a
    proof of source typing or an ahead-of-time ownership checker. -/
def step [StorageCell Cell] (s : StateOf Cell) : Instruction → Result (StateOf Cell)
  | .constant dst value => putCopy s dst (.number value)
  | .boolean dst value => putCopy s dst (.boolean value)
  | .packMove dst sources widths => packRegisters s dst sources widths
  | .unpackMove destinations src widths => unpackRegisters s destinations src widths
  | .copy dst src => do putCopy s dst (← get s src)
  | .move dst src => do
    let value ← get s src
    match value with
    | .owner h =>
      if !(Memory.active s.memory h).isEmpty then .error .liveOwner else pure ()
    | _ => pure ()
    let next ← put s dst value
    return { next with registers := next.registers.set src none }
  | .clear src => do
    match ← get s src with
    | .owner _ | .reference _ => .error .liveOwner
    | _ => return { s with registers := s.registers.set src none }
  | .calculate dst left right op => do
    let value ← arithmetic op (← number s left) (← number s right)
    putCopy s dst value
  | .allocate dst count => do
    vacant s dst
    let count ← number s count
    let (memory, outcome) := Memory.allocate s.memory count.toNat
    let handle ← fromMemory outcome
    put { s with memory } dst (.owner handle)
  | .tryAllocate tag dst error count => do
    if tag = dst ∨ tag = error ∨ dst = error then .error .instruction else pure ()
    vacant s dst
    let _ ← putCopy s tag (.boolean false)
    let _ ← putCopy s error (.number 0)
    let count ← number s count
    let (memory, outcome) := Memory.allocate s.memory count.toNat
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
  | .borrow dst src exclusive => do
    vacant s dst
    match ← get s src with
    | .owner handle =>
      let (memory, outcome) := Memory.borrow s.memory handle exclusive
      let loan ← fromMemory outcome
      put { s with memory } dst (.reference loan)
    | _ => .error .type
  | .endLoan src => do
    match ← get s src with
    | .reference id =>
      let (memory, outcome) := Memory.endLoan s.memory id
      let _ ← fromMemory outcome
      return { s with memory, registers := s.registers.set src none }
    | _ => .error .type
  | .read dst src index => do
    let cell ← fromMemory (Memory.read s.memory (← access s src) (← number s index).toNat)
    let value ← StorageCell.decodeNumber cell
    putCopy s dst (.number value)
  | .readUnchecked dst src index => do
    let cell ← fromMemory (Memory.readUnchecked s.memory (← access s src) (← number s index).toNat (StorageCell.encodeNumber 0))
    let value ← StorageCell.decodeNumber cell
    putCopy s dst (.number value)
  | .writeEmpty dst index value => do
    let (memory, outcome) := Memory.writeEmpty s.memory (← access s dst)
      (← number s index).toNat (StorageCell.encodeNumber (← number s value))
    let _ ← fromMemory outcome
    return { s with memory }
  | .take dst src index => do
    -- A non-copyable destination is never overwritten, even after a memory take.
    match s.registers[dst]? with
    | some none | some (some (.number _)) => pure ()
    | _ => .error .type
    let (memory, outcome) := Memory.take s.memory (← access s src) (← number s index).toNat
    let cell ← fromMemory outcome
    let value ← StorageCell.decodeNumber cell
    putCopy { s with memory } dst (.number value)
  | .storeMove dst index source => do
    if StorageCell.ownedCells (Cell := Cell) then pure () else .error .type
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
    let (memory, outcome) := Memory.writeEmpty s.memory permission offset.toNat cell
    let _ ← fromMemory outcome
    return { s with memory, registers := s.registers.set source none }
  | .takeMove dst src index => do
    if StorageCell.ownedCells (Cell := Cell) then pure () else .error .type
    vacant s dst
    let (memory, outcome) := Memory.take s.memory (← access s src) (← number s index).toNat
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
      let (memory, outcome) := StorageCell.release s.memory h
      let _ ← fromMemory outcome
      return { s with memory, registers := s.registers.set src none }
    | _ => .error .type
  | .narrow dst src rights => do
    vacant s dst
    let token ← cap s src
    let (authority, outcome) := Authority.step s.authority (.narrow token rights)
    match ← fromAuthority outcome with
    | some id => put { s with authority } dst (.capability id)
    | none => .error .instruction
  | .emit token value => do
    let (authority, outcome) := Authority.step s.authority
      (.emit (← cap s token) (Int.ofNat (← number s value).toNat))
    let _ ← fromAuthority outcome
    return { s with authority }
  | .emitByte token value => do
    let byte ← number s value
    let token ← cap s token
    if byte.toNat < 256 then
      let (authority, outcome) := Authority.step s.authority (.emitByte token byte.toUInt8)
      let _ ← fromAuthority outcome
      return { s with authority }
    else .error .type
  | .utf8Check dst src length => do
    let valid ← utf8Observe s.memory (← access s src) (← number s length).toNat
    putCopy s dst (.boolean valid)
  | .loanRoom dst count => do
    let requested ← number s count
    putCopy s dst (.boolean (decide (s.memory.loans.length + requested.toNat ≤ s.memory.limits.loans)))
  | _ => .error .instruction

def terminal (s : StateOf Cell) : Bool :=
  s.memory.blocks.all Option.isNone && s.memory.loans.isEmpty &&
  s.registers.all fun value =>
    match value with
    | some (.owner _) | some (.reference _) => false
    | _ => true

/-- Fuel bounds the prototype executor. It is an explicit resource limit, not
    a termination proof. Error results retain the last state and its owners. -/
def run [StorageCell Cell] (program : Array Instruction) : Nat → Nat → StateOf Cell → StateOf Cell × Result Unit
  | 0, _, s => (s, .error .fuel)
  | fuel + 1, pc, s =>
    match program[pc]? with
    | none => (s, .error .instruction)
    | some (.halt) =>
      if terminal s then (s, .ok ()) else (s, .error .liveOwner)
    | some (.jump target) => run program fuel target s
    | some (.branchFalse register target) =>
      match get s register with
      | .ok (.boolean value) => run program fuel (if value then pc + 1 else target) s
      | .ok _ => (s, .error .type)
      | .error e => (s, .error e)
    | some instruction =>
      match step s instruction with
      | .error e => (s, .error e)
      | .ok next => run program fuel (pc + 1) next

end Witchy.Core
