import Witchy.RecursiveCleanupTally
import Witchy.CleanupCoreTrace

namespace Witchy.Core

def statePayloadTally (weight : Value → Nat) (state : StateOf Value) : Nat :=
  Storage.payloadTally weight state.registers + Storage.heapPayloadTally weight state.memory

/-- Each actual extraction/disposition respects arbitrary payload weights. -/
theorem CleanupCoreAction.tally_le (weight : Value → Nat) (state next : StateOf Value)
    (pending rest : List Value) (instruction : Instruction)
    (action : CleanupCoreAction state pending instruction next rest) :
    statePayloadTally weight next ≤ statePayloadTally weight state := by
  cases action with
  | dispose source value tail present ready =>
    have registers := Storage.payload_tally_set weight state.registers source (some value) none present
    simp only [Option.elim_some, Option.elim_none, Nat.add_zero] at registers
    have heap := Storage.dispose_payload_tally_le weight state.memory value
    change Storage.payloadTally weight (state.registers.set source none) +
      Storage.heapPayloadTally weight (Storage.disposeHeapValue state.memory value).1 ≤ _
    unfold statePayloadTally
    omega
  | expand root dst src index offset block child tail owner empty position stored selected free eligible =>
    have permission : Memory.resolve state.memory (.owner root) true = .ok root := by simp [Memory.resolve, free]
    have initialized := block.last_initialized_present offset.toNat child selected
    have taken : Memory.take state.memory (.owner root) offset.toNat =
        ({ state.memory with blocks := state.memory.blocks.set root (some ⟨block.slots.set offset.toNat none⟩) }, .ok child) := by
      simp [Memory.take, permission, stored, initialized]
    have accepted : (Memory.take state.memory (.owner root) offset.toNat).2 = .ok child := by rw [taken]
    have heap := Storage.take_payload_tally weight state.memory (.owner root) offset.toNat child accepted
    rw [taken] at heap
    dsimp only at heap
    have registers := Storage.payload_tally_set weight state.registers dst none (some child) empty
    simp only [Option.elim_some, Option.elim_none, Nat.add_zero] at registers
    change Storage.payloadTally weight (state.registers.set dst (some child)) +
      Storage.heapPayloadTally weight { state.memory with blocks := state.memory.blocks.set root (some ⟨block.slots.set offset.toNat none⟩) } ≤ _
    unfold statePayloadTally
    dsimp only
    omega

/-- Complete emitted cleanup cannot amplify any weighting that assigns zero
    to administrative numeric constants. No initial ownership assumption is
    needed: multiplicities are counted directly in physical registers and heap. -/
theorem CleanupCoreTrace.tally_le (weight : Value → Nat) (numbers : ∀ value, weight (.number value) = 0)
    (program : Array Instruction) (cost pc exit : Nat) (state final : StateOf Value) (pending : List Value)
    (trace : CleanupCoreTrace program cost pc state pending exit final) :
    statePayloadTally weight final ≤ statePayloadTally weight state := by
  induction trace with
  | done address state => exact Nat.le_refl _
  | @advance count address target before next after work rest instruction action code tail ih =>
    exact Nat.le_trans ih (action.tally_le weight before next work rest instruction)
  | jump code tail ih => exact ih
  | branch code value tail ih => exact ih
  | @constant count address dst exit before after work value code writable tail ih =>
    have registers : Storage.payloadTally weight (before.registers.set dst (some (.number value))) =
        Storage.payloadTally weight before.registers := by
      rcases writable with empty | ⟨old, present⟩
      · have equation := Storage.payload_tally_set weight before.registers dst none (some (.number value)) empty
        simpa only [Option.elim_none, Option.elim_some, numbers, Nat.add_zero] using equation
      · have equation := Storage.payload_tally_set weight before.registers dst (some (.number old)) (some (.number value)) present
        simpa only [Option.elim_some, numbers, Nat.add_zero] using equation
    simpa only [statePayloadTally, registers] using ih

/-- Exact capability identity counts never increase through checked Core
    cleanup, even when the token occurs in several initialized payloads. -/
theorem CleanupCoreTrace.capability_multiplicity (program : Array Instruction) (cost pc exit : Nat)
    (state final : StateOf Value) (pending : List Value)
    (trace : CleanupCoreTrace program cost pc state pending exit final) (token : Nat) :
    statePayloadTally (Storage.capabilityWeight token) final ≤ statePayloadTally (Storage.capabilityWeight token) state :=
  trace.tally_le (Storage.capabilityWeight token) (by intro value; simp [Storage.capabilityWeight])
    program cost pc exit state final pending

end Witchy.Core
