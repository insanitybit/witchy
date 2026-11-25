import Witchy.LogicFootprint
import Witchy.LoanCleanup

namespace Witchy.Logic
open Core

private theorem loan_identity_unique (loans : List Memory.Loan) (left right : Memory.Loan)
    (distinct : loans.Pairwise (fun a b => a.identity ≠ b.identity))
    (leftMember : left ∈ loans) (rightMember : right ∈ loans)
    (same : left.identity = right.identity) : left = right := by
  obtain ⟨i, ib, atI⟩ := List.mem_iff_getElem.mp leftMember
  obtain ⟨j, jb, atJ⟩ := List.mem_iff_getElem.mp rightMember
  rcases Nat.lt_trichotomy i j with before | equal | after
  · have apart := distinct.rel_getElem_of_lt ib jb before
    rw [atI, atJ] at apart
    exact False.elim (apart same)
  · subst j
    exact atI.symm.trans atJ
  · have apart := distinct.rel_getElem_of_lt jb ib after
    rw [atI, atJ] at apart
    exact False.elim (apart same.symm)

private theorem end_other_permissions (memory : Memory.StateOf Value) (identity : Nat)
    (loan : Memory.Loan) (other : Nat) (valid : Memory.LoansValid memory)
    (found : Memory.loan? memory identity = some loan) (apart : other ≠ loan.target) :
    Memory.active (Memory.endLoan memory identity).1 other = Memory.active memory other := by
  rw [Memory.active_after_end]
  apply List.filter_eq_self.mpr
  intro current member
  have different : current.identity ≠ identity := by
    intro same
    have named : loan.identity = identity := by simpa using List.find?_some found
    have equal := loan_identity_unique memory.loans current loan valid.2.2.1
      (List.mem_filter.mp member).1 (List.mem_of_find?_eq_some found) (same.trans named.symm)
    have target : current.target = other := by
      simpa using (List.mem_filter.mp member).2
    rw [equal] at target
    exact apart target.symm
  simp [different]

/-- Ending either kind of loan preserves every unrelated loan and permission
    view. Validity supplies identity uniqueness once in the shared rule layer. -/
theorem end_locality (store : StateOf Value) (source identity : Nat) (loan : Memory.Loan)
    (reference : store.registers[source]? = some (some (.reference identity)))
    (valid : Memory.LoansValid store.memory)
    (found : Memory.loan? store.memory identity = some loan) :
    ∃ next, step store (.endLoan source) = .ok next ∧
      UnchangedOutside [.register source, .loan identity, .permissions loan.target, .loanSupply] store next ∧
      next.registers[source]? = some none ∧ Memory.loan? next.memory identity = none ∧
      next.memory = (Memory.endLoan store.memory identity).1 := by
  let next : StateOf Value := { store with
    registers := store.registers.set source none
    memory := (Memory.endLoan store.memory identity).1 }
  have performed : step store (.endLoan source) = .ok next := by
    simp [step, Core.get, reference, Memory.endLoan, found, fromMemory, next, Bind.bind, Pure.pure]
  refine ⟨next, performed, ?_, ?_, ?_, rfl⟩
  · intro location outside
    cases location with
    | register index =>
      have different : source ≠ index := by intro same; subst index; simp at outside
      exact List.getElem?_set_ne different
    | block handle => simp [observe, next, Memory.block?, Memory.endLoan, found] <;> rfl
    | loan other =>
      have different : other ≠ identity := by intro same; subst other; simp at outside
      exact Memory.end_preserves_other_identity store.memory identity other different
    | permissions other =>
      have different : other ≠ loan.target := by
        intro same
        rw [same] at outside
        simp at outside
      change (Memory.block? (Memory.endLoan store.memory identity).1 other).map
        (fun _ => Memory.active (Memory.endLoan store.memory identity).1 other) =
          (Memory.block? store.memory other).map (fun _ => Memory.active store.memory other)
      have blocks : Memory.block? (Memory.endLoan store.memory identity).1 other = Memory.block? store.memory other := by
        simp [Memory.block?, Memory.endLoan, found] <;> rfl
      rw [blocks, end_other_permissions store.memory identity loan other valid found different]
    | allocator => simp [observe, next, Memory.endLoan, found]
    | loanSupply => simp at outside
    | authority => rfl
  · exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp reference).1
  · exact Memory.ended_loan_is_invalid store.memory identity loan found

end Witchy.Logic
