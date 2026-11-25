import Witchy.Authority

namespace Witchy.Authority

theorem initial_valid (roots : List Token) : Valid roots (initial roots) := by
  intro token ht
  exact ⟨token, ht, fun _ hr => hr⟩

theorem step_valid (roots : List Token) (s : State) (command : Command)
    (hv : Valid roots s) : Valid roots (step s command).1 := by
  cases command with
  | narrow index requested =>
    cases hp : s.tokens[index]? with
    | none => simpa [step, hp] using hv
    | some parent =>
      by_cases hs : requested ⊆ parent
      · simp only [step, hp, hs, ↓reduceIte, Valid]
        intro token ht
        rcases List.mem_append.mp ht with old | new
        · exact hv token old
        · have heq : token = requested := by simpa using new
          subst token
          obtain ⟨root, hroot, hparent⟩ := hv parent (List.mem_of_getElem? hp)
          exact ⟨root, hroot, fun _ hr => hparent (hs hr)⟩
      · simpa [step, hp, hs] using hv
  | emit index value =>
    cases hp : s.tokens[index]? with
    | none => simpa [step, hp] using hv
    | some token =>
      by_cases hw : .consoleWrite ∈ token <;> simpa [step, hp, hw, Valid] using hv
  | emitByte index value =>
    cases hp : s.tokens[index]? with
    | none => simpa [step, hp] using hv
    | some token =>
      by_cases hw : .consoleWrite ∈ token <;> simpa [step, hp, hw, Valid] using hv

theorem emit_requires_root (roots : List Token) (s : State) (index : Nat) (value : Int)
    (hv : Valid roots s) (hs : (step s (.emit index value)).2 = .ok none) :
    ∃ root ∈ roots, .consoleWrite ∈ root := by
  cases hp : s.tokens[index]? with
  | none => simp [step, hp] at hs
  | some token =>
    by_cases hw : .consoleWrite ∈ token
    · obtain ⟨root, hr, ht⟩ := hv token (List.mem_of_getElem? hp)
      exact ⟨root, hr, ht hw⟩
    · simp [step, hp, hw] at hs

theorem failure_atomic (s : State) (command : Command) (e : Error)
    (h : (step s command).2 = .error e) : (step s command).1 = s := by
  cases command with
  | narrow index requested =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some parent =>
      by_cases hs : requested ⊆ parent
      · simp [step, hp, hs] at h
      · simp [step, hp, hs]
  | emit index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      by_cases hw : .consoleWrite ∈ token
      · simp [step, hp, hw] at h
      · simp [step, hp, hw]
  | emitByte index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      by_cases hw : .consoleWrite ∈ token
      · simp [step, hp, hw] at h
      · simp [step, hp, hw]

theorem no_write_preserves_output (roots : List Token) (s : State) (command : Command)
    (hv : Valid roots s) (hn : ∀ root ∈ roots, .consoleWrite ∉ root) :
    (step s command).1.output = s.output := by
  cases command with
  | narrow index requested =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some parent =>
      by_cases hs : requested ⊆ parent <;> simp [step, hp, hs]
  | emit index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      have hw : .consoleWrite ∉ token := by
        obtain ⟨root, hr, ht⟩ := hv token (List.mem_of_getElem? hp)
        intro hwrite
        exact hn root hr (ht hwrite)
      simp [step, hp, hw]
  | emitByte index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      by_cases hw : .consoleWrite ∈ token <;> simp [step, hp, hw]

theorem emit_byte_requires_root (roots : List Token) (s : State) (index : Nat) (value : UInt8)
    (hv : Valid roots s) (hs : (step s (.emitByte index value)).2 = .ok none) :
    ∃ root ∈ roots, .consoleWrite ∈ root := by
  cases hp : s.tokens[index]? with
  | none => simp [step, hp] at hs
  | some token =>
    by_cases hw : .consoleWrite ∈ token
    · obtain ⟨root, hr, ht⟩ := hv token (List.mem_of_getElem? hp)
      exact ⟨root, hr, ht hw⟩
    · simp [step, hp, hw] at hs

/-- Without a write root no byte is ever appended either. -/
theorem no_write_preserves_bytes (roots : List Token) (s : State) (command : Command)
    (hv : Valid roots s) (hn : ∀ root ∈ roots, .consoleWrite ∉ root) :
    (step s command).1.bytes = s.bytes := by
  cases command with
  | narrow index requested =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some parent =>
      by_cases hs : requested ⊆ parent <;> simp [step, hp, hs]
  | emit index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      by_cases hw : .consoleWrite ∈ token <;> simp [step, hp, hw]
  | emitByte index value =>
    cases hp : s.tokens[index]? with
    | none => simp [step, hp]
    | some token =>
      have hw : .consoleWrite ∉ token := by
        obtain ⟨root, hr, ht⟩ := hv token (List.mem_of_getElem? hp)
        intro hwrite
        exact hn root hr (ht hwrite)
      simp [step, hp, hw]

end Witchy.Authority
