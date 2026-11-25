import Witchy.CoreBorrowRoots
import Witchy.StorageStateInvariant
import Witchy.ExecutionPath

namespace Witchy.Core

/-- Moving a register transfers its resource inventory and capabilities.
    Owners additionally require the operational no-active-loans condition. -/
theorem move_storage_ready (s : StateOf Value) (dst src : Nat) (value : Value)
    (empty : s.registers[dst]? = some none) (present : s.registers[src]? = some (some value))
    (movable : ∀ handle, value = .owner handle → (Memory.active s.memory handle).isEmpty = true)
    (invariant : StorageStateInvariant s) :
    ∃ next, step s (.move dst src) = .ok next ∧ StorageStateInvariant next ∧
      next.registers = (s.registers.set dst (some value)).set src none ∧
      next.registers[dst]? = some (some value) ∧ next.registers[src]? = some none ∧
      next.memory = s.memory ∧ next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (.move dst src) → Continues program 1 pc s (pc + 1) next) := by
  have different : dst ≠ src := by
    intro same
    rw [same, present] at empty
    cases empty
  let next : StateOf Value := { s with registers := (s.registers.set dst (some value)).set src none }
  have performed : step s (.move dst src) = .ok next := by
    cases value <;> simp_all [step, get, put, vacant, Bind.bind, Pure.pure, next]
  have owned : Storage.OwnsHeapResources next.registers next.memory := by
    intro resource
    simpa only [next, move_register_count s.registers dst src value resource empty present] using invariant.inventory resource
  have valueValid : Storage.valueCapabilitiesValid s.authority value :=
    invariant.capabilities.1 value (List.mem_of_getElem? present)
  have capabilities : Storage.allCapabilitiesValid next.authority next.registers next.memory := by
    constructor
    · apply Storage.cells_caps_set s.authority _ src none
      · exact Storage.cells_caps_set s.authority s.registers dst (some value) invariant.capabilities.1
          (by intro other same; cases same; exact valueValid)
      · intro other same; cases same
    · exact invariant.capabilities.2
  have roots := step_loan_roots s next (.move dst src) invariant.roots performed
  refine ⟨next, performed, ⟨owned, invariant.structural, capabilities, roots⟩, rfl, ?_, ?_, rfl, rfl, ?_⟩
  · simp [next, List.getElem?_set_ne different.symm,
      List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1]
  · have bound : src < (s.registers.set dst (some value)).length := by
      simpa only [List.length_set] using (List.getElem?_eq_some_iff.mp present).1
    exact List.getElem?_set_self bound
  · intro program pc code
    exact ExecutionEdge.continues program pc (pc + 1) s next (.instruction code performed)

end Witchy.Core
