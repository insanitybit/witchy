import Witchy.BorrowRoots
import Witchy.GenericMemoryRelease

namespace Witchy.Core

theorem owner_get_mem (s : StateOf Value) (index handle : Nat)
    (accepted : get s index = .ok (.owner handle)) : some (.owner handle) ∈ s.registers :=
  List.mem_of_getElem? (get_register_lookup s index (.owner handle) accepted)

/-- Every successful Core access reaches a registered owner in the current
    whole-block loan model, even when the operand is an opaque loan token. -/
theorem resolved_access_root (s : StateOf Value) (operand target : Nat) (permission : Memory.Access)
    (known : LoanRootsRegistered s.registers s.memory.loans)
    (accessed : access s operand = .ok permission)
    (resolved : Memory.resolve s.memory permission true = .ok target) :
    ∃ receiver : Nat, s.registers[receiver]? = some (some (.owner target)) := by
  have member : some (Value.owner target) ∈ s.registers := by
    cases current : get s operand with
    | error reason => simp [access, current, Bind.bind] at accessed
    | ok value =>
      cases value with
      | owner handle =>
        simp [access, current, Bind.bind, Pure.pure] at accessed
        cases accessed
        simp only [Memory.resolve] at resolved
        split at resolved <;> cases resolved
        exact owner_get_mem s operand target current
      | reference identity =>
        simp [access, current, Bind.bind, Pure.pure] at accessed
        cases accessed
        cases found : Memory.loan? s.memory identity with
        | none => simp [Memory.resolve, found] at resolved
        | some loan =>
          simp only [Memory.resolve, found] at resolved
          split at resolved <;> cases resolved
          exact known loan (List.mem_of_find?_eq_some found)
      | number _ | boolean _ | capability _ | scalarAggregate _ => simp [access, current, Bind.bind] at accessed
  obtain ⟨receiver, bound, found⟩ := List.mem_iff_getElem.mp member
  exact ⟨receiver, List.getElem?_eq_some_iff.mpr ⟨bound, found⟩⟩

theorem put_loan_roots (s next : StateOf Value) (dst : Nat) (value : Value)
    (known : LoanRootsRegistered s.registers s.memory.loans) (accepted : put s dst value = .ok next) :
    LoanRootsRegistered next.registers next.memory.loans := by
  cases old : s.registers[dst]? with
  | none => simp [put, vacant, old] at accepted
  | some cell =>
    cases cell with
    | none =>
      simp [put, vacant, old] at accepted
      cases accepted
      exact loan_roots_vacant _ _ _ _ known old
    | some value => simp [put, vacant, old] at accepted

theorem putCopy_loan_roots (s next : StateOf Value) (dst : Nat) (value : Value)
    (known : LoanRootsRegistered s.registers s.memory.loans) (accepted : putCopy s dst value = .ok next) :
    LoanRootsRegistered next.registers next.memory.loans := by
  unfold putCopy at accepted
  split at accepted <;> try cases accepted
  all_goals
    apply loan_roots_set _ _ _ _ known
    intro loan member
    simp_all

theorem move_loan_roots (s inserted : StateOf Value) (dst src : Nat) (value : Value)
    (known : LoanRootsRegistered s.registers s.memory.loans)
    (present : get s src = .ok value) (accepted : put s dst value = .ok inserted) :
    LoanRootsRegistered (inserted.registers.set src none) inserted.memory.loans := by
  cases old : s.registers[dst]? with
  | none => simp [put, vacant, old] at accepted
  | some cell =>
    cases cell with
    | none =>
      simp [put, vacant, old] at accepted
      cases accepted
      exact loan_roots_move _ _ _ _ _ known old (get_register_lookup s src value present)
    | some value => simp [put, vacant, old] at accepted

theorem released_owner_unborrowed (memory : Memory.StateOf Value) (handle : Nat)
    (discardable : Value → Bool) (done : Unit)
    (accepted : (Memory.releaseEmpty memory handle discardable).2 = .ok done) :
    TransferUnborrowed memory (.owner handle) := by
  cases done
  obtain ⟨block, found, free, trivial, effect⟩ := Memory.release_checked_safe memory handle discardable accepted
  exact free

theorem allocate_loans (memory : Memory.StateOf Value) (count : Nat) :
    (Memory.allocate memory count).1.loans = memory.loans := by
  unfold Memory.allocate
  repeat' first | rfl | split

theorem release_loans (memory : Memory.StateOf Value) (handle : Nat) (discardable : Value → Bool) :
    (Memory.releaseEmpty memory handle discardable).1.loans = memory.loans := by
  unfold Memory.releaseEmpty
  repeat' first | rfl | split

theorem allocate_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value) (count : Nat)
    (known : LoanRootsRegistered registers memory.loans) :
    LoanRootsRegistered registers (Memory.allocate memory count).1.loans := by
  unfold Memory.allocate
  repeat' first | exact known | split

theorem take_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (permission : Memory.Access) (index : Nat) (known : LoanRootsRegistered registers memory.loans) :
    LoanRootsRegistered registers (Memory.take memory permission index).1.loans := by
  rw [Memory.take_preserves_loans]
  exact known

theorem write_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (permission : Memory.Access) (index : Nat) (value : Value) (known : LoanRootsRegistered registers memory.loans) :
    LoanRootsRegistered registers (Memory.writeEmpty memory permission index value).1.loans := by
  rw [Memory.write_preserves_loans]
  exact known

theorem borrow_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (handle : Nat) (exclusive : Bool) (known : LoanRootsRegistered registers memory.loans)
    (owner : some (.owner handle) ∈ registers) :
    LoanRootsRegistered registers (Memory.borrow memory handle exclusive).1.loans := by
  unfold Memory.borrow
  repeat' first | exact known | split
  intro loan member
  rcases List.mem_cons.mp member with same | old
  · subst loan
    exact owner
  · exact known loan old

theorem end_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (identity : Nat) (known : LoanRootsRegistered registers memory.loans) :
    LoanRootsRegistered registers (Memory.endLoan memory identity).1.loans := by
  unfold Memory.endLoan
  split
  · exact known
  · intro loan member
    exact known loan (List.mem_filter.mp member).1

theorem release_loan_roots (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (handle : Nat) (discardable : Value → Bool) (known : LoanRootsRegistered registers memory.loans) :
    LoanRootsRegistered registers (Memory.releaseEmpty memory handle discardable).1.loans := by
  unfold Memory.releaseEmpty
  repeat' first | exact known | split

end Witchy.Core
