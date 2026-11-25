import Witchy.CoreCallRegisterProofs

namespace Witchy.Core.Call

theorem move_shape (s next : StateOf Value) (dst src : Nat)
    (accepted : Core.step s (.move dst src) = .ok next) :
    next.memory = s.memory ∧ next.authority = s.authority ∧
      ∃ value, next.registers = (s.registers.set dst (some value)).set src none := by
  simp only [Core.step, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨_, shape⟩ := put_shape s _ dst _ (by assumption)
    cases shape
    exact ⟨rfl, rfl, _, rfl⟩

/-- Only the declared transfer endpoints can change. In particular an unrelated
    suspended caller register keeps its value, including owners and capabilities. -/
theorem moves_frame (s next : StateOf Value) (transfers : List (Nat × Nat))
    (accepted : moveRegisters s transfers = .ok next) (index : Nat)
    (outside : ∀ pair ∈ transfers, pair.1 ≠ index ∧ pair.2 ≠ index) :
    next.registers[index]? = s.registers[index]? := by
  induction transfers generalizing s with
  | nil => cases accepted; rfl
  | cons pair rest ih =>
    cases performed : Core.step s (.move pair.1 pair.2) with
    | error reason => simp [moveRegisters, performed, Bind.bind] at accepted
    | ok middle =>
      have tail : moveRegisters middle rest = .ok next := by
        simpa only [moveRegisters, performed, Bind.bind] using accepted
      rw [ih middle tail (fun other member => outside other (List.mem_cons_of_mem _ member))]
      obtain ⟨_, _, value, registers⟩ := move_shape s middle _ _ performed
      have different := outside pair (List.mem_cons_self)
      simp only [registers, List.getElem?_set_ne different.2, List.getElem?_set_ne different.1]

theorem moves_preserve (s next : StateOf Value) (transfers : List (Nat × Nat))
    (accepted : moveRegisters s transfers = .ok next) (known : StorageStateInvariant s) :
    StorageStateInvariant next ∧ CapabilityConserved s next ∧
      next.memory = s.memory ∧ next.registers.length = s.registers.length := by
  induction transfers generalizing s with
  | nil => cases accepted; exact ⟨known, .refl _, rfl, rfl⟩
  | cons pair rest ih =>
    cases performed : Core.step s (.move pair.1 pair.2) with
    | error reason => simp [moveRegisters, performed, Bind.bind] at accepted
    | ok middle =>
      have tail : moveRegisters middle rest = .ok next := by
        simpa only [moveRegisters, performed, Bind.bind] using accepted
      obtain ⟨invariant, conserved, memory, length⟩ :=
        ih middle tail (move_preserves_storage s middle _ _ known performed)
      obtain ⟨firstMemory, _, value, registers⟩ := move_shape s middle _ _ performed
      refine ⟨invariant, .trans s middle next (move_step_capabilities s middle _ _ performed) conserved,
        memory.trans firstMemory, ?_⟩
      simpa only [registers, List.length_set] using length

end Witchy.Core.Call
