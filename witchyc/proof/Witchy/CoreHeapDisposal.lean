import Witchy.HeapDisposalSafety
import Witchy.ExecutionPath

namespace Witchy.Storage

/-- Incoming storage facts needed by checked leaf disposition. Readiness says
    nothing about successful execution and cannot discard initialized children. -/
def HeapDisposalReady (memory : Memory.StateOf Core.Value) : Core.Value → Prop
  | .owner handle => ∃ block, Memory.block? memory handle = some block ∧
      (Memory.active memory handle).isEmpty = true ∧ ∀ cell ∈ block.slots, cell = none
  | .reference identity => ∃ loan, Memory.loan? memory identity = some loan
  | _ => True

theorem heap_disposal_ready (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (ready : HeapDisposalReady memory value) : (disposeHeapValue memory value).2 = .ok () := by
  cases value with
  | owner handle =>
    obtain ⟨block, stored, free, empty⟩ := ready
    have allEmpty : block.slots.all Option.isNone = true :=
      List.all_eq_true.mpr (fun cell member => by simp [empty cell member])
    simp [disposeHeapValue, disposeValueUsing, Memory.releaseEmpty, stored, free, allEmpty]
  | reference identity =>
    obtain ⟨loan, live⟩ := ready
    simp [disposeHeapValue, disposeValueUsing, Memory.endLoan, live]
  | scalarAggregate fields => rfl
  | number value => rfl
  | boolean value => rfl
  | capability token => rfl

end Witchy.Storage

namespace Witchy.Core

/-- Instruction selection for the current resolved one-cell payload variants.
    Agreement with source type/liveness resolution remains a compiler obligation. -/
def disposalInstruction (source : Nat) : Value → Instruction
  | .owner _ => .release source
  | .reference _ => .endLoan source
  | _ => .clear source

/-- Pure incoming readiness derives actual consuming Core execution, including
    the strict empty-owner policy's agreement with Core trivial release. -/
theorem heap_disposal_effect (s : StateOf Value) (source : Nat) (value : Value)
    (present : s.registers[source]? = some (some value))
    (ready : Storage.HeapDisposalReady s.memory value) :
    step s (disposalInstruction source value) = .ok
      { s with memory := (Storage.disposeHeapValue s.memory value).1, registers := s.registers.set source none } := by
  have found : get s source = .ok value := by simp [get, present]
  have accepted := Storage.heap_disposal_ready s.memory value ready
  cases value with
  | owner handle =>
    obtain ⟨block, stored, free, empty⟩ := ready
    have trivial : block.slots.all (fun cell => cell.elim true Value.trivialRelease) = true :=
      List.all_eq_true.mpr (fun cell member => by rw [empty cell member]; rfl)
    have allEmpty : block.slots.all Option.isNone = true :=
      List.all_eq_true.mpr (fun cell member => by simp [empty cell member])
    simp [step, disposalInstruction, found, StorageCell.release, Memory.releaseEmpty,
      stored, free, trivial, allEmpty, Storage.disposeHeapValue, Storage.disposeValueUsing,
      Bind.bind, Pure.pure, fromMemory]
  | reference identity =>
    simp [step, disposalInstruction, found, Storage.disposeHeapValue, Storage.disposeValueUsing,
      fromMemory, Bind.bind] at accepted ⊢
    rw [accepted]
    rfl
  | scalarAggregate fields => simp [step, disposalInstruction, found, Storage.disposeHeapValue, Storage.disposeValueUsing, Bind.bind, Pure.pure]
  | number value => simp [step, disposalInstruction, found, Storage.disposeHeapValue, Storage.disposeValueUsing, Bind.bind, Pure.pure]
  | boolean value => simp [step, disposalInstruction, found, Storage.disposeHeapValue, Storage.disposeValueUsing, Bind.bind, Pure.pure]
  | capability token => simp [step, disposalInstruction, found, Storage.disposeHeapValue, Storage.disposeValueUsing, Bind.bind, Pure.pure]

/-- Actual one-instruction disposition retains all three independent heap
    invariants, clears exactly the consumed register, and preserves authority. -/
theorem heap_disposal_contract (s : StateOf Value) (source : Nat) (value : Value)
    (present : s.registers[source]? = some (some value))
    (ready : Storage.HeapDisposalReady s.memory value)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory) :
    ∃ next, step s (disposalInstruction source value) = .ok next ∧
      next.memory = (Storage.disposeHeapValue s.memory value).1 ∧
      next.registers = s.registers.set source none ∧ next.registers[source]? = some none ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧
      next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (disposalInstruction source value) →
        Continues program 1 pc s (pc + 1) next) := by
  let next : StateOf Value :=
    { s with memory := (Storage.disposeHeapValue s.memory value).1, registers := s.registers.set source none }
  have performed : step s (disposalInstruction source value) = .ok next :=
    heap_disposal_effect s source value present ready
  have accepted := Storage.heap_disposal_ready s.memory value ready
  have inventory : Storage.OwnsHeapResources next.registers next.memory := by
    intro resource
    have prior := owned resource
    have disposed := Storage.dispose_heap_value_accounting s.memory value accepted resource
    have cleared := register_set_accounting s.registers source (some value) none resource present
    have zero : weight resource none = 0 := by simp [weight]
    rw [zero] at cleared
    change registerCount (s.registers.set source none) resource +
      Storage.heapResources (Storage.disposeHeapValue s.memory value).1 resource =
      memoryCount (Storage.disposeHeapValue s.memory value).1 resource
    omega
  refine ⟨next, performed, rfl, rfl, List.getElem?_set_self (List.getElem?_eq_some_iff.mp present).1,
    inventory, Storage.dispose_heap_value_wellFounded s.memory value structural, ?_, rfl, ?_⟩
  · exact ⟨Storage.cells_caps_set s.authority s.registers source none capabilities.1
      (by intro item impossible; cases impossible),
      Storage.dispose_heap_value_capabilities s.memory value s.authority capabilities.2⟩
  · intro program pc code
    exact ExecutionEdge.continues program pc (pc + 1) s next (.instruction code performed)

end Witchy.Core
