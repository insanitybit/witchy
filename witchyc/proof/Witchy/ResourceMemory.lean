import Witchy.Resources
import Witchy.MemoryAccounting
import Witchy.LoanProofs

namespace Witchy.Core

theorem fresh_loan_absent (s : Memory.StateOf α) (valid : Memory.LoansValid s) :
    Memory.loan? s s.nextLoan = none := by
  apply List.find?_eq_none.mpr
  intro loan member
  have different := Nat.ne_of_lt (valid.2.1 loan member)
  simpa using different

theorem borrow_resource_accounting (s : Memory.StateOf α) (handle : Memory.Handle)
    (exclusive : Bool) (identity : Memory.LoanId) (valid : Memory.LoansValid s)
    (accepted : (Memory.borrow s handle exclusive).2 = .ok identity) (resource : Resource) :
    memoryCount (Memory.borrow s handle exclusive).1 resource =
      memoryCount s resource + weight resource (some (.reference identity)) := by
  cases found : Memory.block? s handle with
  | none => simp [Memory.borrow, found] at accepted
  | some block =>
    by_cases room : s.loans.length < s.limits.loans
    · by_cases rights : Memory.borrowable s handle exclusive = true
      · simp only [Memory.borrow, found, room, rights, ↓reduceIte, Memory.Result.ok.injEq] at accepted
        subst identity
        simp only [Memory.borrow, found, room, rights, ↓reduceIte]
        cases resource with
        | allocation key =>
          simp [memoryCount, Memory.block?, weight, valueResource]
          rfl
        | loan key =>
          by_cases equal : key = s.nextLoan
          · subst key
            have absent := fresh_loan_absent s valid
            simp [memoryCount, Memory.loan?, weight, valueResource]
            simpa [Memory.loan?] using absent
          · simp [memoryCount, Memory.loan?, weight, valueResource, Ne.symm equal]
      · simp [Memory.borrow, found, room, rights] at accepted
    · simp [Memory.borrow, found, room] at accepted

theorem release_resource_accounting (s : Memory.State) (handle : Memory.Handle)
    (accepted : (Memory.release s handle).2 = .ok ()) (resource : Resource) :
    memoryCount (Memory.release s handle).1 resource + weight resource (some (.owner handle)) =
      memoryCount s resource := by
  cases found : Memory.block? s handle with
  | none => simp [Memory.release, found] at accepted
  | some block =>
    by_cases free : (Memory.active s handle).isEmpty = true
    · simp only [Memory.release, found, free, ↓reduceIte]
      cases resource with
      | loan identity => simp [memoryCount, Memory.loan?, weight, valueResource]
      | allocation key =>
        by_cases equal : handle = key
        · subst key
          have lookup := Memory.block_lookup s handle block found
          have bound := (List.getElem?_eq_some_iff.mp lookup).1
          simp [memoryCount, Memory.block?, List.getElem?_set_self bound, lookup,
            weight, valueResource]
        · simp only [memoryCount, Memory.block?, List.getElem?_set_ne equal]
          simp [weight, valueResource, equal]
          rfl
    · simp [Memory.release, found, free] at accepted

theorem end_resource_accounting (s : Memory.StateOf α) (identity : Memory.LoanId)
    (accepted : (Memory.endLoan s identity).2 = .ok ()) (resource : Resource) :
    memoryCount (Memory.endLoan s identity).1 resource + weight resource (some (.reference identity)) =
      memoryCount s resource := by
  cases found : Memory.loan? s identity with
  | none => simp [Memory.endLoan, found] at accepted
  | some loan =>
    cases resource with
    | allocation handle =>
      simp [Memory.endLoan, found, memoryCount, Memory.block?, weight, valueResource]
      rfl
    | loan key =>
      by_cases equal : key = identity
      · subst key
        simp [memoryCount, Memory.ended_loan_is_invalid s identity loan found,
          found, weight, valueResource]
      · simp only [memoryCount, Memory.end_preserves_other_identity s identity key equal]
        simp [weight, valueResource, Ne.symm equal]

theorem lookup_append_one (items : List α) (value : α) (index : Nat) :
    (items ++ [value])[index]? =
      if index = items.length then some value else items[index]? := by
  induction items generalizing index with
  | nil => cases index <;> simp
  | cons head tail ih =>
    cases index with
    | zero => simp
    | succ index => simpa using ih index

/-- Successful allocation adds exactly its fresh owner resource. The equation
    includes zero-slot blocks; allocation quota alone cannot establish this. -/
theorem allocation_resource_accounting (s : Memory.StateOf α) (count : Nat)
    (handle : Memory.Handle) (accepted : (Memory.allocate s count).2 = .ok handle)
    (resource : Resource) :
    memoryCount (Memory.allocate s count).1 resource =
      memoryCount s resource + weight resource (some (.owner handle)) := by
  unfold Memory.allocate at accepted ⊢
  split at accepted <;> rename_i metadata
  · split at accepted <;> rename_i quota
    · simp only [metadata, quota, ↓reduceIte]
      cases accepted
      cases resource with
      | loan identity => simp [memoryCount, Memory.loan?, weight, valueResource]
      | allocation key =>
        by_cases equal : key = s.blocks.length
        · subst key
          simp [memoryCount, Memory.block?, weight, valueResource]
        · simp [memoryCount, Memory.block?, lookup_append_one, equal, weight, valueResource,
            Ne.symm equal]
          rfl
    · cases accepted
  · cases accepted

/-- Changing initialized scalar cells cannot create or destroy an allocation
    identity, even for a zero-sized allocation. -/
theorem replace_block_preserves_resources (s : Memory.StateOf α) (handle : Memory.Handle)
    (old replacement : Storage.Block α) (found : Memory.block? s handle = some old)
    (resource : Resource) :
    memoryCount { s with blocks := s.blocks.set handle (some replacement) } resource =
      memoryCount s resource := by
  cases resource with
  | loan identity => rfl
  | allocation key =>
    by_cases equal : handle = key
    · subst key
      have lookup := Memory.block_lookup s handle old found
      have bound := (List.getElem?_eq_some_iff.mp lookup).1
      simp [memoryCount, Memory.block?, List.getElem?_set_self bound, lookup]
    · simp only [memoryCount, Memory.block?, List.getElem?_set_ne equal]
      rfl

theorem write_preserves_resources (s : Memory.StateOf α) (access : Memory.Access)
    (index : Nat) (value : α) (resource : Resource) :
    memoryCount (Memory.writeEmpty s access index value).1 resource = memoryCount s resource := by
  cases permission : Memory.resolve s access true with
  | error reason => simp [Memory.writeEmpty, permission]
  | ok handle =>
    cases found : Memory.block? s handle with
    | none => simp [Memory.writeEmpty, permission, found]
    | some block =>
      cases cell : block.slots[index]? with
      | none => simp [Memory.writeEmpty, permission, found, cell]
      | some slot =>
        cases slot with
        | some old => simp [Memory.writeEmpty, permission, found, cell]
        | none =>
          simpa only [Memory.writeEmpty, permission, found, cell] using
            replace_block_preserves_resources s handle block
              ⟨block.slots.set index (some value)⟩ found resource

theorem take_preserves_resources (s : Memory.StateOf α) (access : Memory.Access)
    (index : Nat) (resource : Resource) :
    memoryCount (Memory.take s access index).1 resource = memoryCount s resource := by
  cases permission : Memory.resolve s access true with
  | error reason => simp [Memory.take, permission]
  | ok handle =>
    cases found : Memory.block? s handle with
    | none => simp [Memory.take, permission, found]
    | some block =>
      cases cell : block.slots[index]? with
      | none => simp [Memory.take, permission, found, cell]
      | some slot =>
        cases slot with
        | none => simp [Memory.take, permission, found, cell]
        | some value =>
          simpa only [Memory.take, permission, found, cell] using
            replace_block_preserves_resources s handle block
              ⟨block.slots.set index none⟩ found resource

end Witchy.Core
