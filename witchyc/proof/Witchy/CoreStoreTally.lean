import Witchy.PayloadTally
import Witchy.CoreMemoryEvidence
import Witchy.CleanupCoreTally

namespace Witchy.Core

/-- Accepted consuming initialization transfers the exact physical contribution
    from register to heap, for every payload weighting and without Copy. -/
theorem store_move_step_tally (s next : StateOf Value) (dst index source : Nat)
    (accepted : step s (.storeMove dst index source) = .ok next) :
    next.authority = s.authority ∧ ∀ weight, statePayloadTally weight next = statePayloadTally weight s := by
  simp only [step, StorageCell.ownedCells, StorageCell.encodeValue, ↓reduceIte,
    Bind.bind, Pure.pure, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    refine ⟨rfl, ?_⟩
    intro weight
    have registers := Storage.payload_tally_set weight s.registers source (some _) none
      (get_register_lookup s source _ (by assumption))
    simp only [Option.elim_some, Option.elim_none, Nat.add_zero] at registers
    have heap := Storage.write_payload_tally weight s.memory _ _ _ (from_memory_ok _ _ (by assumption))
    dsimp only [statePayloadTally]
    omega

end Witchy.Core
