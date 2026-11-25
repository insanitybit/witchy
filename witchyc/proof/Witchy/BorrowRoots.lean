import Witchy.CoreValueTransfer
import Witchy.HeapOwnershipTransfer

namespace Witchy.Core

/-- A live loan retains a registered owner of its target. This is independent
    of inventory uniqueness, target liveness and location-sensitive relocation. -/
def LoanRootsRegistered (registers : List (Option Value)) (loans : List Memory.Loan) : Prop :=
  ∀ loan ∈ loans, some (.owner loan.target) ∈ registers

private theorem mem_set_kept (values : List α) (index : Nat) (old replacement : α)
    (member : old ∈ values) (different : values[index]? ≠ some old) :
    old ∈ values.set index replacement := by
  obtain ⟨position, bound, found⟩ := List.mem_iff_getElem.mp member
  have separate : index ≠ position := by
    intro same
    subst position
    exact different (List.getElem?_eq_some_iff.mpr ⟨bound, found⟩)
  apply List.mem_of_getElem?
  rw [List.getElem?_set_ne separate]
  exact List.getElem?_eq_some_iff.mpr ⟨bound, found⟩

theorem loan_roots_set (registers : List (Option Value)) (loans : List Memory.Loan)
    (index : Nat) (replacement : Option Value) (known : LoanRootsRegistered registers loans)
    (safe : ∀ loan ∈ loans, registers[index]? ≠ some (some (.owner loan.target))) :
    LoanRootsRegistered (registers.set index replacement) loans := by
  intro loan member
  exact mem_set_kept registers index _ replacement (known loan member) (safe loan member)

theorem loan_roots_vacant (registers : List (Option Value)) (loans : List Memory.Loan)
    (index : Nat) (replacement : Option Value) (known : LoanRootsRegistered registers loans)
    (empty : registers[index]? = some none) :
    LoanRootsRegistered (registers.set index replacement) loans := by
  apply loan_roots_set registers loans index replacement known
  intro loan member
  simp [empty]

theorem loan_roots_move (registers : List (Option Value)) (loans : List Memory.Loan)
    (dst src : Nat) (value : Value) (known : LoanRootsRegistered registers loans)
    (empty : registers[dst]? = some none) (present : registers[src]? = some (some value)) :
    LoanRootsRegistered ((registers.set dst (some value)).set src none) loans := by
  have separate : dst ≠ src := by
    intro same
    subst src
    rw [empty] at present
    cases present
  have kept : (registers.set dst (some value))[src]? = some (some value) := by
    simpa only [List.getElem?_set_ne separate] using present
  intro loan member
  by_cases same : value = .owner loan.target
  · have located : ((registers.set dst (some value)).set src none)[dst]? = some (some (.owner loan.target)) := by
      simp only [List.getElem?_set_ne separate.symm,
        List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1, same]
    exact List.mem_of_getElem? located
  · exact mem_set_kept _ src _ none
      (mem_set_kept registers dst _ (some value) (known loan member) (by simp [empty]))
      (by simp [kept, same])

theorem unborrowed_not_loan_target (memory : Memory.StateOf Value) (value : Value)
    (known : TransferUnborrowed memory value) (loan : Memory.Loan) (member : loan ∈ memory.loans) :
    value ≠ .owner loan.target := by
  intro same
  subst value
  have active : loan ∈ Memory.active memory loan.target := by simp [Memory.active, member]
  have empty := List.isEmpty_iff.mp known
  rw [empty] at active
  cases active

theorem loan_roots_clear (s : StateOf Value) (index : Nat) (value : Value)
    (known : LoanRootsRegistered s.registers s.memory.loans)
    (present : get s index = .ok value) (eligible : TransferUnborrowed s.memory value) :
    LoanRootsRegistered (s.registers.set index none) s.memory.loans := by
  apply loan_roots_set s.registers s.memory.loans index none known
  intro loan member collision
  have actual : get s index = .ok (.owner loan.target) := by simp [get, collision]
  have same : value = .owner loan.target := Result.ok.inj (present.symm.trans actual)
  exact unborrowed_not_loan_target s.memory value eligible loan member same

/-- A registered root cannot simultaneously be a stored payload owner. -/
theorem stored_payload_not_root (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (source receiver target index : Nat) (block : Storage.Block Value) (value : Value)
    (owned : Storage.OwnsHeapResources registers memory)
    (rootOwner : registers[receiver]? = some (some (.owner target)))
    (stored : Memory.block? memory source = some block)
    (initialized : block.slots[index]? = some (some value)) : value ≠ .owner target := by
  intro same
  subst value
  exact (Storage.registered_owner_is_root registers memory receiver target owned rootOwner) source
    ⟨block, stored, List.mem_of_getElem? initialized⟩

/-- Inventory and registered loan roots derive transfer borrow eligibility for
    every initialized heap payload. This does not establish a Move/Relocates law. -/
theorem stored_payload_unborrowed (registers : List (Option Value)) (memory : Memory.StateOf Value)
    (source index : Nat) (block : Storage.Block Value) (value : Value)
    (owned : Storage.OwnsHeapResources registers memory)
    (roots : LoanRootsRegistered registers memory.loans)
    (stored : Memory.block? memory source = some block)
    (initialized : block.slots[index]? = some (some value)) : TransferUnborrowed memory value := by
  cases value with
  | owner handle =>
    have empty : Memory.active memory handle = [] := by
      apply List.filter_eq_nil_iff.mpr
      intro loan member
      have different : loan.target ≠ handle := by
        intro same
        obtain ⟨receiver, bound, found⟩ := List.mem_iff_getElem.mp (roots loan member)
        have rootOwner := List.getElem?_eq_some_iff.mpr ⟨bound, found⟩
        rw [same] at rootOwner
        exact stored_payload_not_root registers memory source receiver handle index block (.owner handle)
          owned rootOwner stored initialized rfl
      simpa using different
    simp [TransferUnborrowed, empty]
  | number _ | boolean _ | reference _ | capability _ | scalarAggregate _ => trivial

end Witchy.Core
