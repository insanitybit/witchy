import Witchy.CoreCallControl
import Witchy.CoreCallBoundaryProofs

namespace Witchy.Core.Call

theorem enter_preserves (program : Array Procedure) (machine next : Machine)
    (target : Nat) (arguments destinations : List Nat)
    (accepted : enter program machine target arguments destinations = .ok next)
    (known : StorageStateInvariant machine.store) :
    StorageStateInvariant next.store ∧ CapabilityConserved machine.store next.store ∧
      next.store.memory = machine.store.memory ∧ next.windowValid := by
  simp only [enter, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨invariant, capabilities, memory, length⟩ :=
      enter_store_preserves machine.store _ _ _ (by assumption) known
    exact ⟨invariant, capabilities, memory, length.symm⟩

theorem leave_preserves (program : Array Procedure) (machine next : Machine)
    (accepted : leave program machine = .ok next) (known : StorageStateInvariant machine.store) :
    StorageStateInvariant next.store ∧ CapabilityConserved machine.store next.store ∧
      next.store.memory = machine.store.memory ∧ next.windowValid := by
  simp only [leave, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨invariant, capabilities, memory, length⟩ :=
      leave_store_preserves machine.store _ _ _ (by assumption) known
    refine ⟨invariant, capabilities, memory, ?_⟩
    dsimp only [Machine.windowValid]
    rw [length]
    simp_all only [ReturnPolicy]

end Witchy.Core.Call
