import Witchy.CoreCallBoundary
import Witchy.CoreCallTransferProofs

namespace Witchy.Core.Call

theorem enter_store_preserves (s next : StateOf Value) (width : Nat) (transfers : List (Nat × Nat))
    (accepted : enterStore s width transfers = .ok next) (known : StorageStateInvariant s) :
    StorageStateInvariant next ∧ CapabilityConserved s next ∧ next.memory = s.memory ∧
      next.registers.length = s.registers.length + width := by
  obtain ⟨invariant, capabilities, memory, length⟩ :=
    moves_preserve (openRegisters s width) next transfers accepted (open_inventory s width known)
  refine ⟨invariant, .trans s _ next (open_capabilities s width) capabilities, memory, ?_⟩
  simpa only [openRegisters, List.length_append, List.length_replicate] using length

theorem leave_store_preserves (s next : StateOf Value) (base : Nat) (transfers : List (Nat × Nat))
    (accepted : leaveStore s base transfers = .ok next) (known : StorageStateInvariant s) :
    StorageStateInvariant next ∧ CapabilityConserved s next ∧ next.memory = s.memory ∧
      next.registers.length = base := by
  cases moved : moveRegisters s transfers with
  | error reason => simp [leaveStore, moved, Bind.bind] at accepted
  | ok middle =>
    have closed : closeRegisters middle base = .ok next := by
      simpa only [leaveStore, moved, Bind.bind] using accepted
    obtain ⟨invariant, capabilities, memory, _⟩ := moves_preserve s middle transfers moved known
    refine ⟨close_inventory middle next base closed invariant,
      .trans s middle next capabilities (close_capabilities middle next base closed), ?_, ?_⟩
    all_goals
      unfold closeRegisters at closed
      split at closed
      next allowed =>
        cases closed
        first | exact memory | exact (List.length_take_of_le allowed.1)
      next => cases closed

theorem leave_store_frame (s next : StateOf Value) (base : Nat) (transfers : List (Nat × Nat))
    (accepted : leaveStore s base transfers = .ok next) (index : Nat) (inside : index < base)
    (outside : ∀ pair ∈ transfers, pair.1 ≠ index ∧ pair.2 ≠ index) :
    next.registers[index]? = s.registers[index]? := by
  cases moved : moveRegisters s transfers with
  | error reason => simp [leaveStore, moved, Bind.bind] at accepted
  | ok middle =>
    have closed : closeRegisters middle base = .ok next := by
      simpa only [leaveStore, moved, Bind.bind] using accepted
    have frame := moves_frame s middle transfers moved index outside
    unfold closeRegisters at closed
    split at closed
    next => cases closed; exact (List.getElem?_take_of_lt inside).trans frame
    next => cases closed

end Witchy.Core.Call
