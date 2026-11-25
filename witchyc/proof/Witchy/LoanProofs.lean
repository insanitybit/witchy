import Witchy.MemoryProofs

namespace Witchy.Memory

/-- Only live entries consume loan metadata. Identities remain globally fresh,
    even after their entries have been removed. -/
def LoanInvariant (loans : List Loan) (next limit : Nat) : Prop :=
  loans.length ≤ limit ∧
  (∀ loan ∈ loans, loan.identity < next) ∧
  loans.Pairwise (fun left right => left.identity ≠ right.identity) ∧
  loans.Pairwise (fun left right => left.target = right.target →
    left.exclusive = false ∧ right.exclusive = false)

def LoanProperty (predicate : List Loan → Nat → Nat → Prop) (s : StateOf Value) : Prop :=
  predicate s.loans s.nextLoan s.limits.loans

def LoansValid (s : StateOf Value) : Prop := LoanProperty LoanInvariant s

def Retired (s : StateOf Value) (key : LoanId) : Prop :=
  key < s.nextLoan ∧ loan? s key = none

theorem allocate_loan_property (predicate : List Loan → Nat → Nat → Prop) (s : StateOf Value) (count : Nat) :
    LoanProperty predicate (allocate s count).1 ↔ LoanProperty predicate s := by
  unfold allocate
  split
  · split <;> rfl
  · rfl

theorem release_loan_property (predicate : List Loan → Nat → Nat → Prop) (s : State) (h : Handle) :
    LoanProperty predicate (release s h).1 ↔ LoanProperty predicate s := by
  unfold release
  split
  · rfl
  · split <;> rfl

theorem write_loan_property (predicate : List Loan → Nat → Nat → Prop) (s : StateOf Value)
    (access : Access) (index : Nat) (value : Value) :
    LoanProperty predicate (writeEmpty s access index value).1 ↔ LoanProperty predicate s := by
  unfold writeEmpty
  repeat' first | rfl | split

theorem take_loan_property (predicate : List Loan → Nat → Nat → Prop) (s : StateOf Value)
    (access : Access) (index : Nat) :
    LoanProperty predicate (take s access index).1 ↔ LoanProperty predicate s := by
  unfold take
  repeat' first | rfl | split

theorem initial_loans_valid (limits : Limits) : LoansValid (initial limits) := by
  simp [LoansValid, LoanProperty, LoanInvariant, initial]

theorem borrowable_compatible (s : StateOf Value) (h : Handle) (exclusive : Bool)
    (rights : borrowable s h exclusive = true) (loan : Loan) (member : loan ∈ s.loans)
    (same : h = loan.target) : exclusive = false ∧ loan.exclusive = false := by
  have present : loan ∈ active s h := by simp [active, member, same]
  cases exclusive with
  | false =>
    have readonly := List.all_eq_true.mp rights loan present
    exact ⟨rfl, by simpa using readonly⟩
  | true =>
    have empty : active s h = [] := by simpa [borrowable] using rights
    simp [empty] at present

theorem borrow_loans_valid (s : StateOf Value) (h : Handle) (exclusive : Bool) (hv : LoansValid s) :
    LoansValid (borrow s h exclusive).1 := by
  cases hb : block? s h with
  | none => simpa [borrow, hb] using hv
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : borrowable s h exclusive = true
      · simp only [borrow, hb, room, rights, ↓reduceIte, LoansValid, LoanProperty,
          LoanInvariant, List.length_cons]
        refine ⟨by omega, ?_, ?_, ?_⟩
        · intro loan member
          rcases List.mem_cons.mp member with fresh | old
          · subst loan
            simp
          · have hf := hv.2.1 loan old
            exact Nat.lt_succ_of_lt hf
        · apply List.pairwise_cons.mpr
          refine ⟨?_, hv.2.2.1⟩
          intro loan member equal
          have hf := hv.2.1 loan member
          change s.nextLoan = loan.identity at equal
          exact Nat.ne_of_lt hf equal.symm
        · apply List.pairwise_cons.mpr
          exact ⟨fun loan member same => borrowable_compatible s h exclusive rights loan member same,
            hv.2.2.2⟩
      · simpa [borrow, hb, room, rights] using hv
    · simpa [borrow, hb, room] using hv

theorem end_loans_valid (s : StateOf Value) (key : LoanId) (hv : LoansValid s) :
    LoansValid (endLoan s key).1 := by
  cases hl : loan? s key with
  | none => simpa [endLoan, hl] using hv
  | some loan =>
    simp only [endLoan, hl]
    refine ⟨Nat.le_trans (List.length_filter_le _ _) hv.1, ?_,
      hv.2.2.1.filter _, hv.2.2.2.filter _⟩
    · intro current member
      exact hv.2.1 current (List.mem_filter.mp member).1

theorem borrow_failure_atomic (s : StateOf Value) (h : Handle) (exclusive : Bool) (reason : Error)
    (failed : (borrow s h exclusive).2 = .error reason) : (borrow s h exclusive).1 = s := by
  cases hb : block? s h with
  | none => simp [borrow, hb]
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : borrowable s h exclusive = true
      · simp [borrow, hb, room, rights] at failed
      · simp [borrow, hb, room, rights]
    · simp [borrow, hb, room]

theorem end_failure_atomic (s : StateOf Value) (key : LoanId) (reason : Error)
    (failed : (endLoan s key).2 = .error reason) : (endLoan s key).1 = s := by
  cases hl : loan? s key with
  | none => simp [endLoan, hl]
  | some loan => simp [endLoan, hl] at failed

theorem borrow_preserves_retired (s : StateOf Value) (h : Handle) (exclusive : Bool) (key : LoanId)
    (hr : Retired s key) : Retired (borrow s h exclusive).1 key := by
  cases hb : block? s h with
  | none => simpa [borrow, hb] using hr
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : borrowable s h exclusive = true
      · have different : s.nextLoan ≠ key := Ne.symm (Nat.ne_of_lt hr.1)
        simp only [borrow, hb, room, rights, ↓reduceIte, Retired]
        constructor
        · exact Nat.lt_succ_of_lt hr.1
        · simpa [loan?, different] using hr.2
      · simpa [borrow, hb, room, rights] using hr
    · simpa [borrow, hb, room] using hr

theorem end_preserves_retired (s : StateOf Value) (ended key : LoanId) (hr : Retired s key) :
    Retired (endLoan s ended).1 key := by
  cases hl : loan? s ended with
  | none => simpa [endLoan, hl] using hr
  | some loan =>
    simp only [endLoan, hl, Retired]
    refine ⟨hr.1, ?_⟩
    apply List.Sublist.find?_eq_none List.filter_sublist
    exact hr.2

theorem end_retires_identity (s : StateOf Value) (key : LoanId) (loan : Loan)
    (hv : LoansValid s) (hl : loan? s key = some loan) : Retired (endLoan s key).1 key := by
  have member : loan ∈ s.loans := List.mem_of_find?_eq_some hl
  have identity : loan.identity = key := by simpa using List.find?_some hl
  constructor
  · simp only [endLoan, hl]
    simpa [identity] using hv.2.1 loan member
  · exact ended_loan_is_invalid s key loan hl

theorem borrow_issues_identity (s : StateOf Value) (h : Handle) (exclusive : Bool) (key : LoanId)
    (accepted : (borrow s h exclusive).2 = .ok key) :
    key = s.nextLoan ∧
    loan? (borrow s h exclusive).1 key = some ⟨s.nextLoan, h, exclusive⟩ ∧
    (borrow s h exclusive).1.nextLoan = s.nextLoan + 1 := by
  cases hb : block? s h with
  | none => simp [borrow, hb] at accepted
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : borrowable s h exclusive = true
      · simp only [borrow, hb, room, rights, ↓reduceIte, Result.ok.injEq] at accepted
        subst key
        simp [borrow, hb, room, rights, loan?]
      · simp [borrow, hb, room, rights] at accepted
    · simp [borrow, hb, room] at accepted

theorem end_preserves_other_identity (s : StateOf Value) (ended key : LoanId) (different : key ≠ ended) :
    loan? (endLoan s ended).1 key = loan? s key := by
  cases hl : loan? s ended with
  | none => simp [endLoan, hl]
  | some loan =>
    simp only [endLoan, hl]
    change (s.loans.filter (fun loan => loan.identity != ended)).find?
      (fun loan => loan.identity == key) = s.loans.find? (fun loan => loan.identity == key)
    rw [List.find?_filter]
    congr 1
    funext current
    by_cases same : current.identity = key <;> simp [same, different]

theorem borrow_end_roundtrip (s : StateOf Value) (h : Handle) (exclusive : Bool) (block : Storage.Block Value)
    (hv : LoansValid s) (hb : block? s h = some block)
    (room : s.loans.length < s.limits.loans) (rights : borrowable s h exclusive = true) :
    let issued := borrow s h exclusive
    issued.2 = .ok s.nextLoan ∧
    endLoan issued.1 s.nextLoan = ({ s with nextLoan := s.nextLoan + 1 }, .ok ()) := by
  have unchanged : s.loans.filter (fun loan => loan.identity != s.nextLoan) = s.loans := by
    apply List.filter_eq_self.mpr
    intro loan member
    simpa using Nat.ne_of_lt (hv.2.1 loan member)
  simp [borrow, hb, room, rights, endLoan, loan?, unchanged]

/-- One completed borrow consumes an identity but returns the metadata entry. -/
theorem borrow_end_empty (s : StateOf Value) (h : Handle) (exclusive : Bool) (block : Storage.Block Value)
    (hb : block? s h = some block) (empty : s.loans = []) (room : 0 < s.limits.loans) :
    let issued := borrow s h exclusive
    issued.2 = .ok s.nextLoan ∧
    endLoan issued.1 s.nextLoan = ({ s with nextLoan := s.nextLoan + 1 }, .ok ()) := by
  simp [borrow, hb, empty, room, borrowable, active, endLoan, loan?]

/-- A proof driver using the actual borrow/end operations, not an alternative
    runtime. It lets the reclamation law quantify over arbitrary cycle counts. -/
def loanCycles (h : Handle) (exclusive : Bool) : Nat → StateOf Value → StateOf Value × Result Unit
  | 0, s => (s, .ok ())
  | count + 1, s =>
    let issued := borrow s h exclusive
    match issued.2 with
    | .error reason => (issued.1, .error reason)
    | .ok key =>
      let ended := endLoan issued.1 key
      match ended.2 with
      | .error _ => ended
      | .ok () => loanCycles h exclusive count ended.1

theorem arbitrarily_many_borrow_end_cycles (s : StateOf Value) (h : Handle) (exclusive : Bool)
    (block : Storage.Block Value) (count : Nat) (hb : block? s h = some block)
    (empty : s.loans = []) (room : 0 < s.limits.loans) :
    loanCycles h exclusive count s = ({ s with nextLoan := s.nextLoan + count }, .ok ()) := by
  induction count generalizing s with
  | zero => simp [loanCycles]
  | succ count ih =>
    have one := borrow_end_empty s h exclusive block hb empty room
    simp only [loanCycles, one.1, one.2]
    simpa [Nat.add_assoc, Nat.add_comm, Nat.add_left_comm] using
      ih { s with nextLoan := s.nextLoan + 1 } hb empty room

end Witchy.Memory
