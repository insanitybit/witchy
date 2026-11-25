import Witchy.CoreMoveInvariant
import Witchy.CoreScalarInvariant

namespace Witchy.Core

theorem move_preserves_storage (s next : StateOf Value) (dst source : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.move dst source) = .ok next) :
    StorageStateInvariant next := by
  have original := accepted
  simp only [step, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨empty, _⟩ := put_shape s _ dst _ (by assumption)
    obtain ⟨produced, performed, invariant, _⟩ := move_storage_ready s dst source _ empty
      (get_register_lookup s source _ (by assumption)) (by
        intro handle same
        subst_vars
        simp_all) known
    have same := original.symm.trans performed
    cases same
    exact invariant

private theorem nonresource_weight (value : Value) (resource : Resource)
    (noOwner : ∀ handle, value ≠ .owner handle) (noReference : ∀ identity, value ≠ .reference identity) :
    weight resource (some value) = 0 := by
  cases value <;> simp_all [weight, valueResource]

theorem clear_preserves_storage (s next : StateOf Value) (source : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.clear source) = .ok next) :
    StorageStateInvariant next := by
  have roots := step_loan_roots s next (.clear source) known.roots accepted
  simp only [step, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    refine ⟨?_, known.structural, ⟨?_, known.capabilities.2⟩, roots⟩
    · intro resource
      have count := register_set_nonresource s.registers source _ none resource
        (get_register_lookup s source _ (by assumption))
        (nonresource_weight _ resource (by assumption) (by assumption)) (by rfl)
      simpa only [count] using known.inventory resource
    · exact Storage.cells_caps_set s.authority s.registers source none known.capabilities.1
        (by intro value impossible; cases impossible)

end Witchy.Core
