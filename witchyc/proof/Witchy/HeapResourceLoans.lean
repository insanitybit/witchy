import Witchy.HeapResources
import Witchy.LoanTargets
import Witchy.GenericMemoryRelease

namespace Witchy.Storage

theorem initial_generic_loans (Value : Type) (limits : Memory.Limits) :
    Memory.LoansValid (Memory.initialOf Value limits) ∧ Core.LoanTargetsLive (Memory.initialOf Value limits) := by
  constructor
  · simp [Memory.LoansValid, Memory.LoanProperty, Memory.LoanInvariant, Memory.initialOf]
  · intro loan member
    simp [Memory.initialOf] at member

private theorem borrow_heap_inventory (memory : Memory.StateOf Core.Value) (handle : Memory.Handle) (exclusive : Bool)
    (resource : Core.Resource) : heapResources (Memory.borrow memory handle exclusive).1 resource = heapResources memory resource := by
  unfold Memory.borrow
  repeat' first | rfl | split

private theorem end_heap_inventory (memory : Memory.StateOf Core.Value) (identity : Memory.LoanId)
    (resource : Core.Resource) : heapResources (Memory.endLoan memory identity).1 resource = heapResources memory resource := by
  unfold Memory.endLoan
  repeat' first | rfl | split

theorem borrow_to_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (handle identity destination : Nat) (exclusive : Bool) (owned : OwnsHeapResources registers memory)
    (valid : Memory.LoansValid memory) (vacant : registers[destination]? = some none)
    (accepted : (Memory.borrow memory handle exclusive).2 = .ok identity) :
    OwnsHeapResources (registers.set destination (some (.reference identity))) (Memory.borrow memory handle exclusive).1 := by
  intro resource
  have prior := owned resource
  have stored := borrow_heap_inventory memory handle exclusive resource
  have live := Core.borrow_resource_accounting memory handle exclusive identity valid accepted resource
  have inserted := Core.register_set_accounting registers destination none (some (.reference identity)) resource vacant
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at inserted
  rw [stored, live]
  omega

theorem end_from_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (source identity : Nat) (owned : OwnsHeapResources registers memory)
    (present : registers[source]? = some (some (.reference identity)))
    (accepted : (Memory.endLoan memory identity).2 = .ok ()) :
    OwnsHeapResources (registers.set source none) (Memory.endLoan memory identity).1 := by
  intro resource
  have prior := owned resource
  have stored := end_heap_inventory memory identity resource
  have live := Core.end_resource_accounting memory identity accepted resource
  have removed := Core.register_set_accounting registers source (some (.reference identity)) none resource present
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at removed
  rw [stored]
  omega

/-- Inventory, permission compatibility and live targets are retained together
    at the borrowing boundary. Handle-origin/lifetime typing remains separate. -/
theorem borrow_register_contract (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (handle identity destination : Nat) (exclusive : Bool) (owned : OwnsHeapResources registers memory)
    (valid : Memory.LoansValid memory) (targets : Core.LoanTargetsLive memory)
    (vacant : registers[destination]? = some none) (accepted : (Memory.borrow memory handle exclusive).2 = .ok identity) :
    OwnsHeapResources (registers.set destination (some (.reference identity))) (Memory.borrow memory handle exclusive).1 ∧
      Memory.LoansValid (Memory.borrow memory handle exclusive).1 ∧
      Core.LoanTargetsLive (Memory.borrow memory handle exclusive).1 := by
  exact ⟨borrow_to_register_owns registers memory handle identity destination exclusive owned valid vacant accepted,
    Memory.borrow_loans_valid memory handle exclusive valid, Core.borrow_loan_targets_live memory handle exclusive targets⟩

theorem end_register_contract (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (source identity : Nat) (owned : OwnsHeapResources registers memory)
    (valid : Memory.LoansValid memory) (targets : Core.LoanTargetsLive memory)
    (present : registers[source]? = some (some (.reference identity)))
    (accepted : (Memory.endLoan memory identity).2 = .ok ()) :
    OwnsHeapResources (registers.set source none) (Memory.endLoan memory identity).1 ∧
      Memory.LoansValid (Memory.endLoan memory identity).1 ∧ Core.LoanTargetsLive (Memory.endLoan memory identity).1 ∧
      Memory.Retired (Memory.endLoan memory identity).1 identity := by
  have retired : Memory.Retired (Memory.endLoan memory identity).1 identity := by
    cases found : Memory.loan? memory identity with
    | none => simp [Memory.endLoan, found] at accepted
    | some loan => exact Memory.end_retires_identity memory identity loan valid found
  exact ⟨end_from_register_owns registers memory source identity owned present accepted,
    Memory.end_loans_valid memory identity valid, Core.end_loan_targets_live memory identity targets, retired⟩

theorem borrow_refused_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (handle : Nat) (exclusive : Bool) (reason : Memory.Error) (owned : OwnsHeapResources registers memory)
    (refused : (Memory.borrow memory handle exclusive).2 = .error reason) :
    OwnsHeapResources registers (Memory.borrow memory handle exclusive).1 := by
  rw [Memory.borrow_failure_atomic memory handle exclusive reason refused]
  exact owned

theorem end_refused_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (identity : Nat) (reason : Memory.Error) (owned : OwnsHeapResources registers memory)
    (refused : (Memory.endLoan memory identity).2 = .error reason) :
    OwnsHeapResources registers (Memory.endLoan memory identity).1 := by
  rw [Memory.end_failure_atomic memory identity reason refused]
  exact owned

end Witchy.Storage
