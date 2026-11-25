import Witchy.MemoryAccounting

namespace Witchy.Memory

theorem lookup_append_boundary (front : List α) (value : α) (back : List α) :
    (front ++ value :: back)[front.length]? = some value := by
  induction front with
  | nil => rfl
  | cons head tail _ih => simp

theorem set_append_boundary (front : List α) (old value : α) (back : List α) :
    (front ++ old :: back).set front.length value = front ++ value :: back := by
  induction front with
  | nil => rfl
  | cons head tail _ih => simp

theorem block_after_set (s : StateOf Value) (handle : Handle) (old replacement : Storage.Block Value)
    (found : block? s handle = some old) :
    block? { s with blocks := s.blocks.set handle (some replacement) } handle = some replacement := by
  have lookup := block_lookup s handle old found
  have bound := (List.getElem?_eq_some_iff.mp lookup).1
  simp [block?, List.getElem?_set_self bound]

theorem block_after_set_other (s : StateOf Value) (changed handle : Handle) (replacement : Option (Storage.Block Value))
    (different : changed ≠ handle) :
    block? { s with blocks := s.blocks.set changed replacement } handle = block? s handle := by
  simp only [block?, List.getElem?_set_ne different]

theorem write_preserves_other_block (s : StateOf Value) (access : Access) (changed handle : Handle)
    (index : Nat) (value : Value) (permission : resolve s access true = .ok changed)
    (different : changed ≠ handle) :
    block? (writeEmpty s access index value).1 handle = block? s handle := by
  simp only [writeEmpty, permission]
  repeat' first
    | rfl
    | exact block_after_set_other s changed handle _ different
    | split

theorem take_preserves_other_block (s : StateOf Value) (access : Access) (changed handle : Handle)
    (index : Nat) (permission : resolve s access true = .ok changed)
    (different : changed ≠ handle) :
    block? (take s access index).1 handle = block? s handle := by
  simp only [take, permission]
  repeat' first
    | rfl
    | exact block_after_set_other s changed handle _ different
    | split

theorem resolution_loan_frame (s next : StateOf Value) (access : Access) (write : Bool)
    (sameLoans : next.loans = s.loans) : resolve next access write = resolve s access write := by
  cases access <;> simp [resolve, active, loan?, sameLoans]

theorem write_preserves_resolution (s : StateOf Value) (changed access : Access)
    (index : Nat) (value : Value) (write : Bool) :
    resolve (writeEmpty s changed index value).1 access write = resolve s access write := by
  apply resolution_loan_frame s _ access write
  unfold writeEmpty
  repeat' first | rfl | split

theorem take_preserves_resolution (s : StateOf Value) (changed access : Access)
    (index : Nat) (write : Bool) :
    resolve (take s changed index).1 access write = resolve s access write := by
  apply resolution_loan_frame s _ access write
  unfold take
  repeat' first | rfl | split

theorem end_preserves_block (s : State) (identity : LoanId) (handle : Handle) :
    block? (endLoan s identity).1 handle = block? s handle := by
  unfold endLoan
  split <;> rfl

theorem write_preserves_loans (s : StateOf Value) (access : Access) (index : Nat) (value : Value) :
    (writeEmpty s access index value).1.loans = s.loans := by
  unfold writeEmpty
  repeat' first | rfl | split

theorem write_preserves_available (s : StateOf Value) (access : Access) (index : Nat) (value : Value) :
    (writeEmpty s access index value).1.available = s.available := by
  unfold writeEmpty
  repeat' first | rfl | split

theorem release_preserves_other_block (s : State) (changed handle : Handle)
    (different : changed ≠ handle) : block? (release s changed).1 handle = block? s handle := by
  unfold release
  repeat' first | rfl | exact block_after_set_other s changed handle _ different | split

theorem release_preserves_loans (s : State) (handle : Handle) : (release s handle).1.loans = s.loans := by
  unfold release
  repeat' first | rfl | split

theorem borrow_preserves_block (s : State) (handle other : Handle) (exclusive : Bool) :
    block? (borrow s handle exclusive).1 other = block? s other := by
  unfold borrow
  repeat' first | rfl | split

theorem borrow_preserves_available (s : State) (handle : Handle) (exclusive : Bool) :
    (borrow s handle exclusive).1.available = s.available := by
  unfold borrow
  repeat' first | rfl | split

theorem take_preserves_loans (s : StateOf Value) (access : Access) (index : Nat) :
    (take s access index).1.loans = s.loans := by
  unfold take
  repeat' first | rfl | split

theorem take_preserves_available (s : StateOf Value) (access : Access) (index : Nat) :
    (take s access index).1.available = s.available := by
  unfold take
  repeat' first | rfl | split

/-- Execute the actual initialization primitive at the first empty slot. -/
theorem write_at_boundary (s : StateOf Value) (access : Access) (handle : Handle)
    (front back : List (Option Value)) (value : Value)
    (permission : resolve s access true = .ok handle)
    (found : block? s handle = some ⟨front ++ none :: back⟩) :
    (writeEmpty s access front.length value).2 = .ok () ∧
    block? (writeEmpty s access front.length value).1 handle =
      some ⟨front ++ some value :: back⟩ := by
  simp only [writeEmpty, permission, found, lookup_append_boundary, Storage.Block.slots_mk]
  constructor
  · trivial
  · rw [set_append_boundary]
    exact block_after_set s handle _ _ found

/-- Execute the actual take primitive at the last initialized slot. -/
theorem take_at_boundary (s : StateOf Value) (access : Access) (handle : Handle)
    (front back : List (Option Value)) (value : Value)
    (permission : resolve s access true = .ok handle)
    (found : block? s handle = some ⟨front ++ some value :: back⟩) :
    (take s access front.length).2 = .ok value ∧
    block? (take s access front.length).1 handle = some ⟨front ++ none :: back⟩ := by
  simp only [take, permission, found, lookup_append_boundary, Storage.Block.slots_mk]
  constructor
  · trivial
  · rw [set_append_boundary]
    exact block_after_set s handle _ _ found

end Witchy.Memory
