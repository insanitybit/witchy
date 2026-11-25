import Witchy.HeapCapabilities

namespace Witchy.Storage

theorem authority_tokens_monotone (authority : Authority.State) (command : Authority.Command) :
    authority.tokens.length ≤ (Authority.step authority command).1.tokens.length := by
  cases command with
  | narrow parent requested =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : requested ⊆ token <;> simp [Authority.step, found, allowed]
  | emit parent value =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : .consoleWrite ∈ token <;> simp [Authority.step, found, allowed]
  | emitByte parent value =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : .consoleWrite ∈ token <;> simp [Authority.step, found, allowed]

/-- Authority operations never replace an existing token's rights. -/
theorem authority_existing_token (authority : Authority.State) (command : Authority.Command) (index : Nat)
    (bound : index < authority.tokens.length) :
    (Authority.step authority command).1.tokens[index]? = authority.tokens[index]? := by
  cases command with
  | narrow parent requested =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : requested ⊆ token
      · simp [Authority.step, found, allowed, List.getElem?_append_left bound]
      · simp [Authority.step, found, allowed]
  | emit parent value =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : .consoleWrite ∈ token <;> simp [Authority.step, found, allowed]
  | emitByte parent value =>
    cases found : authority.tokens[parent]? with
    | none => simp [Authority.step, found]
    | some token =>
      by_cases allowed : .consoleWrite ∈ token <;> simp [Authority.step, found, allowed]

theorem narrowing_created_valid (authority : Authority.State) (parent : Nat) (requested : Authority.Token) (created : Nat)
    (accepted : (Authority.step authority (.narrow parent requested)).2 = .ok (some created)) :
    valueCapabilitiesValid (Authority.step authority (.narrow parent requested)).1 (.capability created) := by
  cases found : authority.tokens[parent]? with
  | none => simp [Authority.step, found] at accepted
  | some token =>
    by_cases allowed : requested ⊆ token
    · simp only [Authority.step, found, allowed, ↓reduceIte, Authority.Result.ok.injEq, Option.some.injEq] at accepted
      subst created
      simp [valueCapabilitiesValid, Authority.step, found, allowed]
    · simp [Authority.step, found, allowed] at accepted

/-- Install only an actually issued narrowed token. Existing stored tokens
    remain valid and all rights still descend from the host roots. -/
theorem narrow_register_capabilities (roots : List Authority.Token) (authority : Authority.State)
    (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (parent destination created : Nat) (requested : Authority.Token)
    (granted : Authority.Valid roots authority) (valid : allCapabilitiesValid authority registers memory)
    (accepted : (Authority.step authority (.narrow parent requested)).2 = .ok (some created)) :
    let next := (Authority.step authority (.narrow parent requested)).1
    allCapabilitiesValid next (registers.set destination (some (.capability created))) memory ∧ Authority.Valid roots next := by
  have old := all_caps_monotone authority _ registers memory (authority_tokens_monotone authority (.narrow parent requested)) valid
  have issued := narrowing_created_valid authority parent requested created accepted
  constructor
  · exact ⟨cells_caps_set _ registers destination (some (.capability created)) old.1
      (by intro other same; cases same; exact issued), old.2⟩
  · exact Authority.step_valid roots authority (.narrow parent requested) granted

end Witchy.Storage
