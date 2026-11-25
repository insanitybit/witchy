import Witchy.MemoryContents
import Witchy.LoanProofs

namespace Witchy.Memory

theorem borrow_preserves_other_target (s : State) (changed handle : Handle) (exclusive : Bool)
    (different : changed ≠ handle) : active (borrow s changed exclusive).1 handle = active s handle := by
  unfold borrow
  repeat' first | rfl | split | (simp [active, different])

theorem borrow_is_sole_target (s : State) (handle : Handle) (exclusive : Bool) (identity : LoanId)
    (accepted : (borrow s handle exclusive).2 = .ok identity)
    (empty : (active s handle).isEmpty = true) :
    ∀ loan ∈ active (borrow s handle exclusive).1 handle, loan.identity = identity := by
  have oldEmpty : s.loans.filter (fun loan => loan.target == handle) = [] := List.isEmpty_iff.mp empty
  cases found : block? s handle with
  | none => simp [borrow, found] at accepted
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : borrowable s handle exclusive = true
      · simp only [borrow, found, room, rights, ↓reduceIte, Result.ok.injEq] at accepted
        subst identity
        simp [borrow, found, room, rights, active, oldEmpty]
      · simp [borrow, found, room, rights] at accepted
    · simp [borrow, found, room] at accepted

theorem consecutive_borrows_distinct (s : State) (source destination : Handle) (reader writer : LoanId)
    (readAccepted : (borrow s source false).2 = .ok reader)
    (writeAccepted : (borrow (borrow s source false).1 destination true).2 = .ok writer) : writer ≠ reader := by
  have first := borrow_issues_identity s source false reader readAccepted
  have second := borrow_issues_identity (borrow s source false).1 destination true writer writeAccepted
  rw [second.1, first.2.2, first.1]
  exact Nat.succ_ne_self _

theorem active_after_end (s : StateOf Cell) (identity : LoanId) (handle : Handle) :
    active (endLoan s identity).1 handle =
      (active s handle).filter (fun loan => loan.identity != identity) := by
  cases found : loan? s identity with
  | none =>
    simp only [endLoan, found]
    symm
    apply List.filter_eq_self.mpr
    intro loan member
    have absent := List.find?_eq_none.mp found loan (List.mem_filter.mp member).1
    simpa using absent
  | some loan => simp only [endLoan, found, active, List.filter_filter, Bool.and_comm]

theorem end_clears_sole_target (s : State) (identity : LoanId) (handle : Handle)
    (sole : ∀ loan ∈ active s handle, loan.identity = identity) :
    (active (endLoan s identity).1 handle).isEmpty = true := by
  rw [active_after_end]
  apply List.isEmpty_iff.mpr
  apply List.filter_eq_nil_iff.mpr
  intro loan member
  simp [sole loan member]

theorem end_preserves_empty_target (s : StateOf Cell) (identity : LoanId) (handle : Handle)
    (empty : (active s handle).isEmpty = true) :
    (active (endLoan s identity).1 handle).isEmpty = true := by
  rw [active_after_end, List.isEmpty_iff.mp empty]
  rfl

theorem end_preserves_available (s : State) (identity : LoanId) :
    (endLoan s identity).1.available = s.available := by
  unfold endLoan
  split <;> rfl

theorem filter_unique_identity_length (loans : List Loan) (identity : LoanId) (loan : Loan)
    (distinct : loans.Pairwise (fun a b => a.identity ≠ b.identity))
    (found : loans.find? (fun current => current.identity == identity) = some loan) :
    (loans.filter (fun current => current.identity != identity)).length + 1 = loans.length := by
  induction loans generalizing loan with
  | nil => simp at found
  | cons head tail ih =>
    obtain ⟨headDistinct, tailDistinct⟩ := List.pairwise_cons.mp distinct
    by_cases equalHead : head.identity = identity
    · have retained : tail.filter (fun current => current.identity != identity) = tail := by
        apply List.filter_eq_self.mpr
        intro current member
        have different : current.identity ≠ identity := by
          intro equal
          exact headDistinct current member (equalHead.trans equal.symm)
        simpa using different
      simp [equalHead, retained]
    · have foundTail : tail.find? (fun current => current.identity == identity) = some loan := by
        simpa [List.find?_cons, equalHead] using found
      have count := ih loan tailDistinct foundTail
      simpa [equalHead, Nat.add_assoc] using congrArg Nat.succ count

theorem end_releases_one_entry (s : State) (identity : LoanId) (loan : Loan)
    (valid : LoansValid s) (found : loan? s identity = some loan) :
    (endLoan s identity).1.loans.length + 1 = s.loans.length := by
  rw [endLoan, found]
  exact filter_unique_identity_length s.loans identity loan valid.2.2.1 found

theorem end_leaves_metadata_room (s : State) (identity : LoanId) (loan : Loan)
    (valid : LoansValid s) (found : loan? s identity = some loan) :
    (endLoan s identity).1.loans.length < (endLoan s identity).1.limits.loans := by
  have count := end_releases_one_entry s identity loan valid found
  have bounded := valid.1
  have sameLimit : (endLoan s identity).1.limits = s.limits := by
    unfold endLoan
    split <;> rfl
  rw [sameLimit]
  omega

theorem end_copy_metadata_room (s : State) (reader writer : LoanId) (writeLoan : Loan)
    (valid : LoansValid s) (writeLive : loan? s writer = some writeLoan) (different : writer ≠ reader) :
    let ended := (endLoan (endLoan s reader).1 writer).1
    LoansValid ended ∧ ended.loans.length < ended.limits.loans := by
  have retained := (end_preserves_other_identity s reader writer different).trans writeLive
  have firstValid := end_loans_valid s reader valid
  exact ⟨end_loans_valid _ writer firstValid, end_leaves_metadata_room _ writer writeLoan firstValid retained⟩

/-- End the reader and writer after copying. Other unrelated loans may remain;
    only the two target-specific sole-loan premises are required. -/
theorem end_copy_loans (s : State) (reader writer : LoanId) (source destination : Handle)
    (readLoan writeLoan : Loan) (readLive : loan? s reader = some readLoan)
    (writeLive : loan? s writer = some writeLoan) (different : writer ≠ reader)
    (sourceSole : ∀ loan ∈ active s source, loan.identity = reader)
    (destinationSole : ∀ loan ∈ active s destination, loan.identity = writer) :
    let first := endLoan s reader
    let second := endLoan first.1 writer
    first.2 = .ok () ∧ second.2 = .ok () ∧
    (active second.1 source).isEmpty = true ∧ (active second.1 destination).isEmpty = true ∧
    (∀ handle, block? second.1 handle = block? s handle) ∧ second.1.available = s.available := by
  dsimp only
  have writerRetained : loan? (endLoan s reader).1 writer = some writeLoan :=
    (end_preserves_other_identity s reader writer different).trans writeLive
  have readerEnded : (endLoan s reader).2 = .ok () := by simp [endLoan, readLive]
  have writerEnded : (endLoan (endLoan s reader).1 writer).2 = .ok () := by
    rw [endLoan, writerRetained]
  refine ⟨readerEnded, writerEnded, ?_, ?_, ?_, ?_⟩
  · exact end_preserves_empty_target _ writer source (end_clears_sole_target s reader source sourceSole)
  · apply end_clears_sole_target _ writer destination
    intro loan member
    rw [active_after_end] at member
    exact destinationSole loan (List.mem_filter.mp member).1
  · intro handle
    rw [end_preserves_block, end_preserves_block]
  · rw [end_preserves_available, end_preserves_available]

end Witchy.Memory
