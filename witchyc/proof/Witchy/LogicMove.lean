import Witchy.LogicUpdates

namespace Witchy.Logic

structure MoveInput (resource : Resource) (dst src : Nat) (value : Core.Value) : Prop where
  destination : resource (.register dst) = some none
  source : resource (.register src) = some (some value)
  movable : Movable resource value

def moved (resource : Resource) (dst src : Nat) (value : Core.Value) : Resource :=
  (resource.set (.register dst) (some (some value))).set (.register src) (some none)

def MoveOutput (dst src : Nat) (value : Core.Value) (before : Resource)
    (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after = moved before dst src value

/-- The same consuming move used by the existing call ABI. A destination
    must be vacant; a borrowed allocation owner cannot be moved. -/
theorem move_rule (dst src : Nat) (value : Core.Value) :
    AtomicRule (.move dst src) (fun resource => MoveInput resource dst src value) (MoveOutput dst src value) := by
  apply AtomicRule.of_effect
  intro resource store input _valid known
  have destination := known (.register dst) _ input.destination
  have source := known (.register src) _ input.source
  change store.registers[dst]? = some none at destination
  change store.registers[src]? = some (some value) at source
  have different : dst ≠ src := by intro same; subst src; rw [destination] at source; cases source
  have bound := (List.getElem?_eq_some_iff.mp destination).1
  have sourceBound := (List.getElem?_eq_some_iff.mp source).1
  have movable := Movable.realize resource store value known input.movable
  let next : Core.StateOf Core.Value :=
    { store with registers := (store.registers.set dst (some value)).set src none }
  have performed : Core.step store (.move dst src) = .ok next := by
    cases value <;> simp_all [Core.step, Core.get, Core.put, Core.vacant, Core.TransferUnborrowed, next, Bind.bind, Pure.pure]
  rw [performed]
  have unchanged : UnchangedOutside [.register dst, .register src] store next := by
    intro location outside
    cases location with
    | register other =>
      have first : dst ≠ other := by intro same; subst other; simp at outside
      have second : src ≠ other := by intro same; subst other; simp at outside
      exact (List.getElem?_set_ne second).trans (List.getElem?_set_ne first)
    | block _ | loan _ | permissions _ | allocator | loanSupply | authority => rfl
  refine ⟨[.register dst, .register src], ?_, unchanged, rfl, ?_⟩
  · intro location member
    simp only [List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl
    · exact .inl (Resource.occupied input.destination)
    · exact .inl (Resource.occupied input.source)
  · let output := resource.patch [.register dst, .register src] (observe next)
    have sameOutside := Resource.patch_same_outside resource (observe next) [.register dst, .register src]
    have atDst : output (.register dst) = some (some value) := by
      dsimp only [output]
      rw [Resource.patch_at _ _ _ _ (by simp)]
      change ((store.registers.set dst (some value)).set src none)[dst]? = some (some value)
      rw [List.getElem?_set_ne different.symm, List.getElem?_set_self bound]
    have atSrc : output (.register src) = some none := by
      dsimp only [output]
      rw [Resource.patch_at _ _ _ _ (by simp)]
      change ((store.registers.set dst (some value)).set src none)[src]? = some none
      exact List.getElem?_set_self (by simpa only [List.length_set] using sourceBound)
    have canonical := sameOutside.two resource output (.register dst) (.register src) _ _ atDst atSrc (by exact fun equality => different (Location.register.inj equality))
    exact canonical

end Witchy.Logic
