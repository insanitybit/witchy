import Witchy.CoreMoveInvariant
import Witchy.RecursiveCleanupTally

namespace Witchy.Core

/-- A consuming physical register move conserves every payload weighting,
    including the multiplicity of any chosen capability token. -/
theorem move_register_payload_tally (weight : Value → Nat) (registers : List (Option Value))
    (destination source : Nat) (value : Value)
    (empty : registers[destination]? = some none) (present : registers[source]? = some (some value)) :
    Storage.payloadTally weight ((registers.set destination (some value)).set source none) =
      Storage.payloadTally weight registers := by
  have different : destination ≠ source := by
    intro same
    rw [same, present] at empty
    cases empty
  have retained : (registers.set destination (some value))[source]? = some (some value) := by
    simpa only [List.getElem?_set_ne different] using present
  have inserted := Storage.payload_tally_set weight registers destination none (some value) empty
  have consumed := Storage.payload_tally_set weight (registers.set destination (some value)) source (some value) none retained
  simp only [Option.elim_none, Option.elim_some, Nat.add_zero] at inserted consumed
  omega

/-- Conservation is tied to the accepted Core step, not an assumed transfer
    model. Heap payloads and authority are unchanged by that same step. -/
theorem move_accepted_payload_tally (s next : StateOf Value) (destination source : Nat) (value : Value)
    (empty : s.registers[destination]? = some none) (present : s.registers[source]? = some (some value))
    (movable : ∀ root, value = .owner root → (Memory.active s.memory root).isEmpty = true)
    (invariant : StorageStateInvariant s) (accepted : step s (.move destination source) = .ok next) :
    next.authority = s.authority ∧ ∀ weight : Value → Nat,
      Storage.payloadTally weight next.registers + Storage.heapPayloadTally weight next.memory =
        Storage.payloadTally weight s.registers + Storage.heapPayloadTally weight s.memory := by
  obtain ⟨actual, performed, _, registers, _, _, memory, authority, _⟩ :=
    move_storage_ready s destination source value empty present movable invariant
  have same : actual = next := by
    have equality := performed.symm.trans accepted
    cases equality
    rfl
  subst next
  refine ⟨authority, ?_⟩
  intro weight
  rw [registers, memory, move_register_payload_tally weight s.registers destination source value empty present]

/-- Exact token multiplicity and the authority table (hence its policies) are
    preserved together. This strengthens validity-only move contracts. -/
theorem move_accepted_capability_multiplicity (s next : StateOf Value) (destination source : Nat) (value : Value)
    (empty : s.registers[destination]? = some none) (present : s.registers[source]? = some (some value))
    (movable : ∀ root, value = .owner root → (Memory.active s.memory root).isEmpty = true)
    (invariant : StorageStateInvariant s) (accepted : step s (.move destination source) = .ok next) :
    next.authority = s.authority ∧ ∀ token,
      Storage.payloadTally (Storage.capabilityWeight token) next.registers +
        Storage.heapPayloadTally (Storage.capabilityWeight token) next.memory =
      Storage.payloadTally (Storage.capabilityWeight token) s.registers +
        Storage.heapPayloadTally (Storage.capabilityWeight token) s.memory := by
  obtain ⟨authority, conserved⟩ := move_accepted_payload_tally s next destination source value empty present movable invariant accepted
  exact ⟨authority, fun token => conserved (Storage.capabilityWeight token)⟩

end Witchy.Core
