import Witchy.ResourceMemory

namespace Witchy.Core

/-- An active loan must retain a live target, including zero-slot blocks. -/
def LoanTargetsLive (memory : Memory.StateOf α) : Prop :=
  ∀ loan ∈ memory.loans, memoryCount memory (.allocation loan.target) = 1

theorem initial_loan_targets_live (limits : Memory.Limits) :
    LoanTargetsLive (Memory.initial limits) := by
  simp [LoanTargetsLive, Memory.initial]

theorem loan_targets_frame (memory next : Memory.StateOf α) (live : LoanTargetsLive memory)
    (sameLoans : next.loans = memory.loans)
    (sameBlocks : ∀ handle, memoryCount next (.allocation handle) =
      memoryCount memory (.allocation handle)) : LoanTargetsLive next := by
  intro loan member
  rw [sameBlocks]
  exact live loan (by simpa only [sameLoans] using member)

theorem write_loan_targets_live (memory : Memory.StateOf α) (access : Memory.Access)
    (index : Nat) (value : α) (live : LoanTargetsLive memory) :
    LoanTargetsLive (Memory.writeEmpty memory access index value).1 := by
  apply loan_targets_frame memory _ live
  · unfold Memory.writeEmpty
    repeat' first | rfl | split
  · intro handle
    exact write_preserves_resources memory access index value (.allocation handle)

theorem take_loan_targets_live (memory : Memory.StateOf α) (access : Memory.Access)
    (index : Nat) (live : LoanTargetsLive memory) :
    LoanTargetsLive (Memory.take memory access index).1 := by
  apply loan_targets_frame memory _ live
  · unfold Memory.take
    repeat' first | rfl | split
  · intro handle
    exact take_preserves_resources memory access index (.allocation handle)

theorem end_loan_targets_live (memory : Memory.StateOf α) (identity : Memory.LoanId)
    (live : LoanTargetsLive memory) : LoanTargetsLive (Memory.endLoan memory identity).1 := by
  cases found : Memory.loan? memory identity with
  | none => simpa [Memory.endLoan, found] using live
  | some loan =>
    intro current member
    simp only [Memory.endLoan, found, List.mem_filter] at member
    simp only [Memory.endLoan, found, memoryCount, Memory.block?]
    change memoryCount memory (.allocation current.target) = 1
    exact live current member.1

theorem allocate_loan_targets_live (memory : Memory.StateOf α) (count : Nat)
    (live : LoanTargetsLive memory) : LoanTargetsLive (Memory.allocate memory count).1 := by
  cases outcome : (Memory.allocate memory count).2 with
  | error reason => simpa only [Memory.allocation_failure_atomic memory count reason outcome] using live
  | ok handle =>
    have accounting := allocation_resource_accounting memory count handle outcome
    have fresh := (Memory.allocation_fresh memory count handle outcome).1
    have sameLoans : (Memory.allocate memory count).1.loans = memory.loans := by
      unfold Memory.allocate
      repeat' first | rfl | split
    intro loan member
    have old := live loan (by simpa only [sameLoans] using member)
    have different : handle ≠ loan.target := by
      intro equal
      rw [← equal, fresh] at old
      simp [memoryCount, Memory.block?] at old
    simpa [weight, valueResource, different, old] using accounting (.allocation loan.target)

theorem borrow_loan_targets_live (memory : Memory.StateOf α) (handle : Memory.Handle)
    (exclusive : Bool) (live : LoanTargetsLive memory) :
    LoanTargetsLive (Memory.borrow memory handle exclusive).1 := by
  cases found : Memory.block? memory handle with
  | none => simpa [Memory.borrow, found] using live
  | some block =>
    by_cases room : memory.loans.length < memory.limits.loans
    · by_cases rights : Memory.borrowable memory handle exclusive = true
      · intro loan member
        simp only [Memory.borrow, found, room, rights, ↓reduceIte, List.mem_cons] at member ⊢
        rcases member with equal | old
        · subst loan
          change memoryCount memory (.allocation handle) = 1
          simp [memoryCount, found]
        · change memoryCount memory (.allocation loan.target) = 1
          exact live loan old
      · simpa [Memory.borrow, found, room, rights] using live
    · simpa [Memory.borrow, found, room] using live

theorem release_loan_targets_live (memory : Memory.State) (handle : Memory.Handle)
    (live : LoanTargetsLive memory) : LoanTargetsLive (Memory.release memory handle).1 := by
  cases found : Memory.block? memory handle with
  | none => simpa [Memory.release, found] using live
  | some block =>
    by_cases free : (Memory.active memory handle).isEmpty = true
    · intro loan member
      simp only [Memory.release, found, free, ↓reduceIte] at member ⊢
      have different : handle ≠ loan.target := by
        intro equal
        have activeMember : loan ∈ Memory.active memory handle := by
          simp [Memory.active, member, equal]
        have empty : Memory.active memory handle = [] := List.isEmpty_iff.mp free
        rw [empty] at activeMember
        cases activeMember
      simp only [memoryCount, Memory.block?, List.getElem?_set_ne different]
      change memoryCount memory (.allocation loan.target) = 1
      exact live loan member
    · simpa [Memory.release, found, free] using live

end Witchy.Core
