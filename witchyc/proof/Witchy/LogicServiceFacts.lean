import Witchy.LogicAtomic

namespace Witchy.Logic

theorem loan_supply_observed (store : Core.StateOf Core.Value) (resource : Resource)
    (counter live limit : Nat) (known : Agrees store resource)
    (entry : resource .loanSupply = some (counter, live, limit)) :
    store.memory.nextLoan = counter ∧ store.memory.loans.length = live ∧ store.memory.limits.loans = limit := by
  have same : (store.memory.nextLoan, store.memory.loans.length, store.memory.limits.loans) =
      (counter, live, limit) := Option.some.inj (known .loanSupply _ entry)
  exact ⟨congrArg Prod.fst same, congrArg (fun triple => triple.2.1) same,
    congrArg (fun triple => triple.2.2) same⟩

theorem fresh_loan_absent (store : Core.StateOf Core.Value) (valid : ValidStore store) :
    observe store (.loan store.memory.nextLoan) = none := by
  change store.memory.loans.find? (fun loan => loan.identity == store.memory.nextLoan) = none
  apply List.find?_eq_none.mpr
  intro loan member
  have below := valid.loans.2.1 loan member
  simp [Nat.ne_of_lt below]

theorem allocator_observed (store : Core.StateOf Core.Value) (resource : Resource)
    (available slots limit issued : Nat) (known : Agrees store resource)
    (entry : resource .allocator = some (available, slots, limit, issued)) :
    store.memory.available = available ∧ store.memory.limits.slots = slots ∧
      store.memory.limits.allocations = limit ∧ store.memory.blocks.length = issued := by
  have same : (store.memory.available, store.memory.limits.slots,
      store.memory.limits.allocations, store.memory.blocks.length) = (available, slots, limit, issued) :=
    Option.some.inj (known .allocator _ entry)
  exact ⟨congrArg Prod.fst same, congrArg (fun quad => quad.2.1) same,
    congrArg (fun quad => quad.2.2.1) same, congrArg (fun quad => quad.2.2.2) same⟩

theorem fresh_block_absent (store : Core.StateOf Core.Value) :
    Memory.block? store.memory store.memory.blocks.length = none := by
  unfold Memory.block?
  rw [List.getElem?_eq_none (Nat.le_refl store.memory.blocks.length)]
  rfl

theorem fresh_permissions_free (store : Core.StateOf Core.Value) (valid : ValidStore store) :
    Memory.active store.memory store.memory.blocks.length = [] := by
  apply List.filter_eq_nil_iff.mpr
  intro loan member
  have live := valid.storage.loan_targets store loan member
  have different : loan.target ≠ store.memory.blocks.length := by
    intro same
    rw [same] at live
    simp [Core.memoryCount, fresh_block_absent store] at live
  simp [different]

theorem bool_slot_observed (store : Core.StateOf Core.Value) (resource : Resource) (index : Nat)
    (old : Option Bool) (known : Agrees store resource)
    (entry : resource (.register index) = some (old.map Core.Value.boolean)) : Core.BoolSlot store index := by
  have actual := known (.register index) _ entry
  cases old with
  | none => exact .inl actual
  | some value => exact .inr ⟨value, actual⟩

theorem number_slot_observed (store : Core.StateOf Core.Value) (resource : Resource) (index : Nat)
    (old : Option UInt64) (known : Agrees store resource)
    (entry : resource (.register index) = some (old.map Core.Value.number)) : Core.NumberSlot store index := by
  have actual := known (.register index) _ entry
  cases old with
  | none => exact .inl actual
  | some value => exact .inr ⟨value, actual⟩

end Witchy.Logic
