import Witchy.LogicResource

namespace Witchy.Logic

/-- A projection of the actual resource-valued Core store, not another heap
    interpreter. All service counters agree with their actual global values. -/
def observe (store : Core.StateOf Core.Value) : Resource
  | .register index => store.registers[index]?
  | .block handle => Memory.block? store.memory handle
  | .loan identity => Memory.loan? store.memory identity
  | .permissions handle => (Memory.block? store.memory handle).map (fun _ => Memory.active store.memory handle)
  | .allocator => some (store.memory.available, store.memory.limits.slots,
      store.memory.limits.allocations, store.memory.blocks.length)
  | .loanSupply => some (store.memory.nextLoan, store.memory.loans.length, store.memory.limits.loans)
  | .authority => some store.authority

def Agrees (store : Core.StateOf Core.Value) (resource : Resource) : Prop :=
  ∀ location value, resource location = some value → observe store location = some value

theorem absent_permission (store : Core.StateOf Core.Value) (handle : Nat)
    (absent : Memory.block? store.memory handle = none) : observe store (.permissions handle) = none := by
  change (Memory.block? store.memory handle).map (fun _ => Memory.active store.memory handle) = none
  rw [absent]
  rfl

theorem agrees_absent (store : Core.StateOf Core.Value) (resource : Resource) (location : Location)
    (known : Agrees store resource) (absent : observe store location = none) : resource location = none := by
  cases found : resource location with
  | none => rfl
  | some value =>
    have present := known location value found
    rw [absent] at present
    cases present

theorem permissions_observed (store : Core.StateOf Core.Value) (handle : Nat) (loans : List Memory.Loan)
    (known : observe store (.permissions handle) = some loans) : Memory.active store.memory handle = loans := by
  change (Memory.block? store.memory handle).map (fun _ => Memory.active store.memory handle) = some loans at known
  cases found : Memory.block? store.memory handle with
  | none => rw [found] at known; cases known
  | some block => rw [found] at known; exact Option.some.inj known

theorem agrees_empty (store : Core.StateOf Core.Value) : Agrees store Resource.empty := by
  intro location value found
  cases found

theorem agrees_join (store : Core.StateOf Core.Value) (left right : Resource)
    (apart : Resource.Disjoint left right) :
    Agrees store (Resource.join left right) ↔ Agrees store left ∧ Agrees store right := by
  constructor
  · intro known
    constructor
    · intro location value found
      exact known location value (by simp [Resource.join, found])
    · intro location value found
      have empty : left location = none := by
        rcases apart location with empty | empty
        · exact empty
        · rw [found] at empty
          cases empty
      exact known location value (by simp [Resource.join, empty, found])
  · rintro ⟨leftKnown, rightKnown⟩ location value found
    cases first : left location with
    | none => exact rightKnown location value (by simpa [Resource.join, first] using found)
    | some original =>
      have same : original = value := by simpa [Resource.join, first] using found
      subst original
      exact leftKnown location value first

end Witchy.Logic
