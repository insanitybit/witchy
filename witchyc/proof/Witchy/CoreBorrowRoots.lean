import Witchy.BorrowRootEffects
import Witchy.ScalarAggregateRoots
import Witchy.CoreCellExecution

namespace Witchy.Core

/-- Successful resource-valued Core steps retain registered roots for all live
    loans. The fixed StorageCell instance and all dynamic guards are used. -/
theorem step_loan_roots (s next : StateOf Value) (instruction : Instruction)
    (known : LoanRootsRegistered s.registers s.memory.loans)
    (accepted : step s instruction = .ok next) :
    LoanRootsRegistered next.registers next.memory.loans := by
  cases instruction <;>
    simp only [step, Bind.bind, Pure.pure, fromMemory, fromAuthority,
      StorageCell.ownedCells, StorageCell.encodeValue, StorageCell.decodeValue,
      StorageCell.decodeNumber, StorageCell.release] at accepted
  case packMove dst sources widths => exact pack_loan_roots s next dst sources widths known accepted
  case unpackMove destinations src widths => exact unpack_loan_roots s next destinations src widths known accepted
  case move dst src =>
    repeat' first
      | (cases accepted; first
        | exact known
        | (apply move_loan_roots s _ dst src _ known <;> assumption))
      | (split at accepted <;> try simp_all)
  case clear src =>
    repeat' first
      | (cases accepted; apply loan_roots_clear s src _ known <;> first | assumption | simp [TransferUnborrowed])
      | (split at accepted <;> try simp_all)
  case storeMove dst index source =>
    repeat' first
      | (cases accepted
         simp only [Memory.write_preserves_loans]
         apply loan_roots_clear s source _ known
         · assumption
         · simp_all [TransferUnborrowed])
      | (split at accepted <;> try simp_all)
  case release src =>
    repeat' first
      | (cases accepted
         simp only [release_loans]
         apply loan_roots_clear s src _ known
         · assumption
         · solve_by_elim [released_owner_unborrowed])
      | (split at * <;> try simp_all)
  case borrow dst src exclusive =>
    repeat' first
      | (apply put_loan_roots _ _ _ _ ?_ accepted
         apply borrow_loan_roots _ _ _ _ known
         apply owner_get_mem s src
         assumption)
      | (cases accepted)
      | (split at accepted <;> try simp_all)
  case endLoan src =>
    repeat' first
      | (cases accepted
         apply end_loan_roots
         apply loan_roots_clear s src _ known
         · assumption
         · trivial)
      | (split at accepted <;> try simp_all)
  case tryAllocate tag dst error count =>
    repeat' first
      | (apply putCopy_loan_roots _ _ _ _ ?_ accepted
         first
           | exact known
           | (apply put_loan_roots
              case accepted => assumption
              simpa only [allocate_loans] using known)
           | (apply putCopy_loan_roots s _ _ _ known
              assumption))
      | (cases accepted <;> contradiction)
      | (split at accepted <;> try simp_all)
  all_goals
    repeat' first
      | (apply putCopy_loan_roots _ _ _ _ ?_ accepted
         simpa only [Memory.take_preserves_loans, allocate_loans] using known)
      | (apply put_loan_roots _ _ _ _ ?_ accepted
         simpa only [Memory.take_preserves_loans, allocate_loans] using known)
      | (cases accepted <;> simpa only [Memory.write_preserves_loans] using known)
      | (split at accepted <;> try simp_all)

theorem run_loan_roots (program : Array Instruction) (fuel pc : Nat) (s : StateOf Value)
    (known : LoanRootsRegistered s.registers s.memory.loans) :
    LoanRootsRegistered (run program fuel pc s).1.registers (run program fuel pc s).1.memory.loans := by
  exact run_preserves_cells (fun state => LoanRootsRegistered state.registers state.memory.loans)
    (fun state next instruction roots accepted => step_loan_roots state next instruction roots accepted)
    program fuel pc s known

theorem host_run_loan_roots (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s : StateOf Value) (accepted : initialOf Value limits roots registers = .ok s)
    (program : Array Instruction) (fuel pc : Nat) :
    LoanRootsRegistered (run program fuel pc s).1.registers (run program fuel pc s).1.memory.loans := by
  apply run_loan_roots program fuel pc s
  unfold initialOf at accepted
  split at accepted
  · cases accepted
    simp [LoanRootsRegistered, Memory.initialOf]
  · cases accepted

end Witchy.Core
