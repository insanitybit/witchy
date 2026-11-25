import Witchy.CoreStoragePreservation
import Witchy.CoreInitialStorage
import Witchy.CoreLoanMetadata
import Witchy.ExecutionPath

namespace Witchy.Core

theorem storage_step_loans (s next : StateOf Value) (instruction : Instruction)
    (valid : Memory.LoansValid s.memory) (accepted : step s instruction = .ok next) :
    Memory.LoansValid next.memory :=
  step_loan_property_cells Memory.LoanInvariant s next instruction valid
    (fun handle exclusive => Memory.borrow_loans_valid s.memory handle exclusive valid)
    (fun identity => Memory.end_loans_valid s.memory identity valid)
    (fun handle => (Memory.release_empty_loan_property Memory.LoanInvariant s.memory handle Value.trivialRelease).mpr valid)
    accepted

/-- Arbitrary control flow, early failure and exhausted fuel retain global
    storage validity. This does not establish a source record's initialized
    prefix or assert that a procedure can execute without fault. -/
theorem run_storage_invariant (program : Array Instruction) (fuel pc : Nat) (s : StateOf Value)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory) :
    StorageStateInvariant (run program fuel pc s).1 := by
  have both := run_preserves_cells (fun state : StateOf Value =>
      StorageStateInvariant state ∧ Memory.LoansValid state.memory) (fun state next instruction facts accepted =>
        ⟨step_storage_invariant state next instruction facts.1 facts.2 accepted,
          storage_step_loans state next instruction facts.2 accepted⟩) program fuel pc s ⟨known, valid⟩
  exact both.1

theorem ExecutionPath.storage_invariant (program : Array Instruction) (cost pc target : Nat)
    (s current : StateOf Value) (path : ExecutionPath program cost pc s target current)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory) : StorageStateInvariant current := by
  have preserved := run_storage_invariant program cost pc s known valid
  rw [continues_exact_prefix program cost pc target s current (path.continues program cost pc target s current)] at preserved
  exact preserved

theorem host_path_storage_invariant (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s current : StateOf Value) (host : initialOf Value limits roots registers = .ok s)
    (program : Array Instruction) (cost pc target : Nat) (path : ExecutionPath program cost pc s target current) :
    StorageStateInvariant current :=
  path.storage_invariant program cost pc target s current (initial_storage_invariant limits roots registers s host)
    (host_run_resource_loans_valid limits roots registers s host program 0 pc)

theorem host_run_storage_invariant (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s : StateOf Value) (host : initialOf Value limits roots registers = .ok s)
    (program : Array Instruction) (fuel pc : Nat) : StorageStateInvariant (run program fuel pc s).1 :=
  run_storage_invariant program fuel pc s (initial_storage_invariant limits roots registers s host)
    (host_run_resource_loans_valid limits roots registers s host program 0 pc)

end Witchy.Core
