import Witchy.CoreBorrowEntry
import Witchy.ExclusiveLoanRetirement

namespace Witchy.Core

/-- Exact inventory excludes a stale registered loan. -/
theorem registered_reference_loan (s : StateOf Value) (source identity : Nat)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (present : s.registers[source]? = some (some (.reference identity))) :
    ∃ loan, Memory.loan? s.memory identity = some loan := by
  have positive := present_resource_count_positive s.registers source (.reference identity) (.loan identity) present rfl
  have inventory := owned (.loan identity)
  cases found : Memory.loan? s.memory identity with
  | none => simp [memoryCount, found] at inventory; omega
  | some loan => exact ⟨loan, rfl⟩

/-- Retiring a registered reference derives actual execution and invalidates
    its permission, retaining storage and all independent invariants. -/
theorem end_loan_storage_ready (s : StateOf Value) (source identity : Nat)
    (present : s.registers[source]? = some (some (.reference identity)))
    (invariant : StorageStateInvariant s) (valid : Memory.LoansValid s.memory) :
    ∃ next, step s (.endLoan source) = .ok next ∧ StorageStateInvariant next ∧
      Memory.LoansValid next.memory ∧ Memory.Retired next.memory identity ∧
      next.registers = s.registers.set source none ∧ next.registers[source]? = some none ∧
      next.memory = (Memory.endLoan s.memory identity).1 ∧
      next.memory.blocks = s.memory.blocks ∧ next.memory.available = s.memory.available ∧
      next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (.endLoan source) → Continues program 1 pc s (pc + 1) next) := by
  obtain ⟨loan, found⟩ := registered_reference_loan s source identity invariant.inventory present
  let memory := (Memory.endLoan s.memory identity).1
  have ended : Memory.endLoan s.memory identity = (memory, .ok ()) := by
    simp [Memory.endLoan, found, memory]
  have accepted : (Memory.endLoan s.memory identity).2 = .ok () := by rw [ended]
  let next : StateOf Value := { s with memory, registers := s.registers.set source none }
  have performed : step s (.endLoan source) = .ok next := by
    simp [step, get, present, ended, fromMemory, Bind.bind, Pure.pure, next]
  obtain ⟨owned, metadata, _targets, retired⟩ := Storage.end_register_contract s.registers s.memory source identity
    invariant.inventory valid (invariant.loan_targets s) present accepted
  have graph := Storage.end_ownership_wellFounded s.memory identity invariant.structural
  have blocks : memory.blocks = s.memory.blocks := by simp [memory, Memory.endLoan, found]
  have capabilities : Storage.allCapabilitiesValid next.authority next.registers next.memory := by
    constructor
    · exact Storage.cells_caps_set s.authority s.registers source none invariant.capabilities.1
        (by intro value same; cases same)
    · simpa only [Storage.heapCapabilitiesValid, next, blocks] using invariant.capabilities.2
  have roots := step_loan_roots s next (.endLoan source) invariant.roots performed
  refine ⟨next, performed, ⟨owned, graph, capabilities, roots⟩, metadata, retired, rfl, ?_, rfl,
    blocks, ?_, rfl, ?_⟩
  · simp [next, List.getElem?_set_self (List.getElem?_eq_some_iff.mp present).1]
  · simp [next, memory, Memory.endLoan, found]
  · intro program pc code
    exact ExecutionEdge.continues program pc (pc + 1) s next (.instruction code performed)

/-- Retiring the migration reference restores the owner permission needed
    by checked release, while unrelated loans may remain live. -/
theorem end_exclusive_storage_ready (s : StateOf Value) (source identity : Nat) (loan : Memory.Loan)
    (present : s.registers[source]? = some (some (.reference identity)))
    (found : Memory.loan? s.memory identity = some loan) (exclusive : loan.exclusive = true)
    (invariant : StorageStateInvariant s) (valid : Memory.LoansValid s.memory) :
    ∃ next, step s (.endLoan source) = .ok next ∧ StorageStateInvariant next ∧
      Memory.LoansValid next.memory ∧ Memory.Retired next.memory identity ∧
      next.registers = s.registers.set source none ∧
      Memory.resolve next.memory (.owner loan.target) true = .ok loan.target ∧
      next.memory.blocks = s.memory.blocks ∧ next.memory.available = s.memory.available ∧
      next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (.endLoan source) → Continues program 1 pc s (pc + 1) next) := by
  obtain ⟨next, performed, known, metadata, retired, registers, _empty, memory, blocks, available, authority, execution⟩ :=
    end_loan_storage_ready s source identity present invariant valid
  refine ⟨next, performed, known, metadata, retired, registers, ?_, blocks, available, authority, execution⟩
  have free := Memory.end_exclusive_frees_target s.memory identity loan valid found exclusive
  simp only [memory, Memory.resolve, free, ↓reduceIte]

end Witchy.Core
