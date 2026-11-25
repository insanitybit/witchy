import Witchy.LogicAtomic
import Witchy.CoreBooleanEffects

namespace Witchy.Logic

/-- Scalar slots cannot overwrite owners, references, or capabilities. -/
def ScalarSlot (resource : Resource) (dst : Nat) (value : Core.Value) : Prop :=
  match value with
  | .number _ => resource (.register dst) = some none ∨
      ∃ old, resource (.register dst) = some (some (.number old))
  | .boolean _ => resource (.register dst) = some none ∨
      ∃ old, resource (.register dst) = some (some (.boolean old))
  | _ => False

def ScalarOutput (dst : Nat) (value : Core.Value) (before : Resource)
    (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after (.register dst) = some (some value) ∧
    Resource.SameOutside [.register dst] before after

theorem ScalarSlot.realize (resource : Resource) (store : Core.StateOf Core.Value)
    (dst : Nat) (value : Core.Value) (known : Agrees store resource)
    (slot : ScalarSlot resource dst value) :
    Core.putCopy store dst value = .ok { store with registers := store.registers.set dst (some value) } ∧
      dst < store.registers.length ∧ resource (.register dst) ≠ none := by
  cases value with
  | owner _ | reference _ | capability _ | scalarAggregate _ => cases slot
  | number n | boolean n =>
    rcases slot with empty | ⟨old, present⟩
    · have actual := known (.register dst) _ empty
      change store.registers[dst]? = some none at actual
      exact ⟨by simp [Core.putCopy, actual], (List.getElem?_eq_some_iff.mp actual).1,
        Resource.occupied empty⟩
    · have actual := known (.register dst) _ present
      change store.registers[dst]? = some _ at actual
      exact ⟨by simp [Core.putCopy, actual], (List.getElem?_eq_some_iff.mp actual).1,
        Resource.occupied present⟩

/-- Common register locality proof; operation wrappers only discharge evaluation. -/
theorem scalar_rule (instruction : Core.Instruction) (dst : Nat) (value : Core.Value)
    (pre : Resource → Prop)
    (ready : ∀ resource, pre resource → ScalarSlot resource dst value)
    (evaluate : ∀ resource store, pre resource → Agrees store resource →
      Core.step store instruction = Core.putCopy store dst value) :
    AtomicRule instruction pre (ScalarOutput dst value) := by
  apply AtomicRule.of_effect
  intro resource store input _valid known
  obtain ⟨written, bound, owned⟩ := ScalarSlot.realize resource store dst value known (ready resource input)
  rw [evaluate resource store input known, written]
  refine ⟨[.register dst], ?_, ?_, rfl, ?_, Resource.patch_same_outside _ _ _⟩
  · intro location member
    simp only [List.mem_cons, List.not_mem_nil, or_false] at member
    subst location
    exact .inl owned
  · intro location outside
    cases location with
    | register other =>
      have different : dst ≠ other := by intro same; subst other; simp at outside
      exact List.getElem?_set_ne different
    | block _ | loan _ | permissions _ | allocator | loanSupply | authority => rfl
  · exact (Resource.patch_at _ _ _ _ (by simp)).trans (List.getElem?_set_self bound)

theorem constant_rule (dst : Nat) (value : UInt64) :
    AtomicRule (.constant dst value) (fun resource => ScalarSlot resource dst (.number value))
      (ScalarOutput dst (.number value)) := by
  exact scalar_rule _ dst _ _ (fun _ input => input) (fun _ _ _ _ => rfl)

theorem copy_number_rule (dst src : Nat) (value : UInt64) :
    AtomicRule (.copy dst src)
      (fun resource => ScalarSlot resource dst (.number value) ∧
        resource (.register src) = some (some (.number value)))
      (ScalarOutput dst (.number value)) := by
  apply scalar_rule _ dst _ _ (fun _ input => input.1)
  intro resource store input known
  have actual := known (.register src) _ input.2
  change store.registers[src]? = some (some (.number value)) at actual
  simp [Core.step, Core.get, actual, Bind.bind]

theorem calculate_rule (dst left right : Nat) (a b : UInt64) (op : Core.Arithmetic)
    (value : Core.Value) (computed : Core.arithmetic op a b = .ok value) :
    AtomicRule (.calculate dst left right op)
      (fun resource => ScalarSlot resource dst value ∧
        resource (.register left) = some (some (.number a)) ∧
        resource (.register right) = some (some (.number b)))
      (ScalarOutput dst value) := by
  apply scalar_rule _ dst _ _ (fun _ input => input.1)
  intro resource store input known
  have actualLeft := known (.register left) _ input.2.1
  have actualRight := known (.register right) _ input.2.2
  change store.registers[left]? = some (some (.number a)) at actualLeft
  change store.registers[right]? = some (some (.number b)) at actualRight
  simp [Core.step, Core.number, Core.get, actualLeft, actualRight, computed, Bind.bind, Pure.pure]

end Witchy.Logic
