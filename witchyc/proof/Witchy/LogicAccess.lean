import Witchy.LogicAtomic
import Witchy.CoreValueTransfer

namespace Witchy.Logic

/-- All facts needed to authorize a write are local resources: the source
    value and either the owner's permission view or an exclusive live loan. -/
def WritableAccess (resource : Resource) (source handle : Nat) : Prop :=
  (resource (.register source) = some (some (.owner handle)) ∧ resource (.permissions handle) = some []) ∨
  ∃ identity loan, resource (.register source) = some (some (.reference identity)) ∧
    resource (.loan identity) = some loan ∧ loan.target = handle ∧ loan.exclusive = true

theorem WritableAccess.realize (resource : Resource) (store : Core.StateOf Core.Value) (source handle : Nat)
    (known : Agrees store resource) (access : WritableAccess resource source handle) :
    ∃ permission, Core.access store source = .ok permission ∧
      Memory.resolve store.memory permission true = .ok handle := by
  rcases access with owner | ⟨identity, loan, reference, entry, target, exclusive⟩
  · have value := known (.register source) (some (.owner handle)) owner.1
    have permissions := known (.permissions handle) [] owner.2
    have empty := permissions_observed store handle [] permissions
    change store.registers[source]? = some (some (.owner handle)) at value
    exact ⟨.owner handle, by simp [Core.access, Core.get, value, Bind.bind, Pure.pure],
      by simp [Memory.resolve, empty]⟩
  · have value := known (.register source) (some (.reference identity)) reference
    have found := known (.loan identity) loan entry
    change store.registers[source]? = some (some (.reference identity)) at value
    refine ⟨.loan identity, ?_, ?_⟩
    · simp [Core.access, Core.get, value, Bind.bind, Pure.pure]
    · change Memory.loan? store.memory identity = some loan at found
      simp [Memory.resolve, found, exclusive, target]

def Movable (resource : Resource) : Core.Value → Prop
  | .owner handle => resource (.permissions handle) = some []
  | _ => True

theorem Movable.realize (resource : Resource) (store : Core.StateOf Core.Value) (value : Core.Value)
    (known : Agrees store resource) (movable : Movable resource value) :
    Core.TransferUnborrowed store.memory value := by
  cases value with
  | owner handle =>
    have free := known (.permissions handle) [] movable
    have empty := permissions_observed store handle [] free
    simp [Core.TransferUnborrowed, empty]
  | number _ | boolean _ | scalarAggregate _ | reference _ | capability _ => trivial

end Witchy.Logic
