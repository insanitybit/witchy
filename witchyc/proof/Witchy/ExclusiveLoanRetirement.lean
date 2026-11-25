import Witchy.LoanProofs

namespace Witchy.Memory

/-- Compatibility makes an exclusive loan the sole live loan for its target. -/
theorem exclusive_target_unique (s : StateOf Value) (loan other : Loan)
    (valid : LoansValid s) (member : loan ∈ s.loans) (exclusive : loan.exclusive = true)
    (otherMember : other ∈ s.loans) (same : other.target = loan.target) : other = loan := by
  obtain ⟨i, ib, atI⟩ := List.mem_iff_getElem.mp member
  obtain ⟨j, jb, atJ⟩ := List.mem_iff_getElem.mp otherMember
  rcases Nat.lt_trichotomy i j with before | equal | after
  · have compatible := valid.2.2.2.rel_getElem_of_lt ib jb before
    rw [atI, atJ] at compatible
    have readonly := (compatible same.symm).1
    simp [exclusive] at readonly
  · subst j
    exact atJ.symm.trans atI
  · have compatible := valid.2.2.2.rel_getElem_of_lt jb ib after
    rw [atI, atJ] at compatible
    have readonly := (compatible same).2
    simp [exclusive] at readonly

/-- Ending an exclusive loan restores the owner's unborrowed permission.
    No assumption that all loans have been ended is necessary. -/
theorem end_exclusive_frees_target (s : StateOf Value) (identity : LoanId) (loan : Loan)
    (valid : LoansValid s) (found : loan? s identity = some loan)
    (exclusive : loan.exclusive = true) :
    (active (endLoan s identity).1 loan.target).isEmpty = true := by
  have member : loan ∈ s.loans := List.mem_of_find?_eq_some found
  have named : loan.identity = identity := by simpa using List.find?_some found
  have empty : active (endLoan s identity).1 loan.target = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro other otherMember
    simp only [endLoan, found] at otherMember
    obtain ⟨old, retained⟩ := List.mem_filter.mp otherMember
    intro same
    have target : other.target = loan.target := by simpa using same
    have equal := exclusive_target_unique s loan other valid member exclusive old target
    simp [equal, named] at retained
  simp [empty]

end Witchy.Memory
