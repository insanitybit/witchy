import Witchy.LoanTargets

namespace Witchy.Memory

theorem allocate_preserves_loans (s : State) (count : Nat) : (allocate s count).1.loans = s.loans := by
  unfold allocate
  repeat' first | rfl | split

theorem allocate_preserves_old_block (s : State) (count : Nat) (handle : Handle) (block : Block)
    (found : block? s handle = some block) : block? (allocate s count).1 handle = some block := by
  have bound := (List.getElem?_eq_some_iff.mp (block_lookup s handle block found)).1
  unfold allocate
  split
  · split
    · simpa only [block?, List.getElem?_append_left bound] using found
    · exact found
  · exact found

/-- Fresh allocation cannot revive an old loan: every old loan already targets
    a live block, whereas the issued handle did not exist before allocation. -/
theorem allocation_has_no_old_loans (s : State) (count : Nat) (handle : Handle)
    (targetsLive : Core.LoanTargetsLive s) (accepted : (allocate s count).2 = .ok handle) :
    (active (allocate s count).1 handle).isEmpty = true := by
  have fresh := (allocation_fresh s count handle accepted).1
  have absent : block? s handle = none := by rw [fresh]; simp [block?]
  have different : ∀ loan ∈ s.loans, loan.target ≠ handle := by
    intro loan member equal
    have live := targetsLive loan member
    rw [equal] at live
    simp [Core.memoryCount, absent] at live
  apply List.isEmpty_iff.mpr
  change ((allocate s count).1.loans.filter (fun loan => loan.target == handle)) = []
  rw [allocate_preserves_loans]
  apply List.filter_eq_nil_iff.mpr
  intro loan member
  simpa using different loan member

theorem allocation_charges_full_count (s : State) (count : Nat) (handle : Handle)
    (accepted : (allocate s count).2 = .ok handle) :
    (allocate s count).1.available + count = s.available := by
  by_cases metadata : s.blocks.length < s.limits.allocations
  · by_cases quota : count ≤ s.available
    · simpa only [allocate, metadata, quota, ↓reduceIte] using Nat.sub_add_cancel quota
    · simp [allocate, metadata, quota] at accepted
  · simp [allocate, metadata] at accepted

theorem allocation_distinct_live_block (s : State) (count : Nat) (fresh existing : Handle)
    (block : Block) (accepted : (allocate s count).2 = .ok fresh)
    (found : block? s existing = some block) : fresh ≠ existing := by
  have identity := (allocation_fresh s count fresh accepted).1
  have bound := (List.getElem?_eq_some_iff.mp (block_lookup s existing block found)).1
  rw [identity]
  exact Ne.symm (Nat.ne_of_lt bound)

theorem allocation_error_kind_of {Cell : Type} (s : StateOf Cell) (count : Nat) (reason : Error)
    (refused : (allocate s count).2 = .error reason) : reason = .allocation ∨ reason = .metadata := by
  unfold allocate at refused
  split at refused
  · split at refused
    · cases refused
    · cases refused
      exact Or.inl rfl
  · cases refused
    exact Or.inr rfl

theorem allocation_error_kind (s : State) (count : Nat) (reason : Error)
    (refused : (allocate s count).2 = .error reason) : reason = .allocation ∨ reason = .metadata :=
  allocation_error_kind_of s count reason refused

end Witchy.Memory
