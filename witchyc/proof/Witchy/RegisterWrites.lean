import Witchy.ScalarAggregateFrames
import Witchy.CoreCellExecution

namespace Witchy.Core

variable {Cell : Type}

/-- Conservative register writes, including moved/cleared source cells and all
    allocation outcomes. Heap mutation and capability output do not write cells. -/
def Instruction.registerWrites : Instruction → List Nat
  | .constant dst _ | .boolean dst _ | .copy dst _ | .calculate dst _ _ _ | .loanRoom dst _ | .utf8Check dst _ _
  | .allocate dst _ | .borrow dst _ _ | .read dst _ _ | .readUnchecked dst _ _ | .take dst _ _ | .takeMove dst _ _ | .narrow dst _ _ => [dst]
  | .packMove dst sources _ => dst :: sources
  | .unpackMove destinations src _ => src :: destinations
  | .move dst src => [dst, src]
  | .storeMove _ _ source => [source]
  | .clear src | .endLoan src | .release src => [src]
  | .tryAllocate tag dst error _ => [tag, dst, error]
  | .writeEmpty _ _ _ | .emit _ _ | .emitByte _ _ | .jump _ | .branchFalse _ _ | .halt => []

private theorem put_registers (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : put s dst value = .ok next) :
    next.registers = s.registers.set dst (some value) := by
  simp only [put, Bind.bind] at accepted
  split at accepted <;> cases accepted
  rfl

private theorem copy_registers (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) :
    next.registers = s.registers.set dst (some value) := by
  unfold putCopy at accepted
  split at accepted <;> cases accepted
  all_goals rfl

variable [StorageCell Cell]

/-- Checked against actual generic-cell execution, including consuming heap
    transfers and every resource/capability instruction. -/
theorem step_register_frame (s next : StateOf Cell) (instruction : Instruction) (index : Nat)
    (outside : index ∉ instruction.registerWrites) (accepted : step s instruction = .ok next) :
    next.registers[index]? = s.registers[index]? := by
  cases instruction
  case packMove dst sources widths =>
    exact (pack_frame s next dst sources widths index (by simpa [Instruction.registerWrites] using outside) accepted).1
  case unpackMove destinations src widths =>
    exact (unpack_frame s next destinations src widths index (by simpa [Instruction.registerWrites] using outside) accepted).1
  all_goals simp only [Instruction.registerWrites, List.mem_cons, List.not_mem_nil, or_false, not_or] at outside
  all_goals simp only [step, Bind.bind, Pure.pure, fromMemory, fromAuthority] at accepted
  all_goals
    repeat' first
      | (cases accepted)
      | (split at accepted <;> try simp_all)
  all_goals grind only [→ put_registers, → copy_registers, List.getElem?_set_ne]

end Witchy.Core
