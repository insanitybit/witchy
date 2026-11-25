import Witchy.LogicMove
import Witchy.RegisterBounds
import Witchy.CoreCallBoundaryProofs

namespace Witchy.Logic

/-- Local total store transition with explicit ABI register lengths. This is
    used for the existing register-window operations, not a second executor. -/
def StoreTriple (beforeCount afterCount : Nat)
    (transition : Core.StateOf Core.Value → Core.Result (Core.StateOf Core.Value))
    (pre : Resource → Prop) (post : Resource → Resource → Prop) : Prop :=
  ∀ resource frame store, pre resource → Resource.Disjoint resource frame →
    StoreRealizes store (Resource.join resource frame) → store.registers.length = beforeCount →
    ∃ next output, transition store = .ok next ∧ next.registers.length = afterCount ∧
      post resource output ∧ Resource.Disjoint output frame ∧ StoreRealizes next (Resource.join output frame)

theorem StoreTriple.atomic (count : Nat) (instruction : Core.Instruction) (pre : Resource → Prop)
    (post : Resource → Core.Result Unit → Resource → Prop)
    (rule : AtomicRule instruction pre post)
    (nofault : ∀ resource reason, pre resource → post resource (.error reason) resource → False) :
    StoreTriple count count (fun store => Core.step store instruction) pre (fun before after => post before (.ok ()) after) := by
  intro resource frame store input apart realized size
  have known := rule resource frame store input apart realized
  cases performed : Core.step store instruction with
  | error reason => rw [performed] at known; exact False.elim (nofault resource reason input known)
  | ok next =>
    rw [performed] at known
    obtain ⟨output, result, resultApart, final⟩ := known
    exact ⟨next, output, performed, (Core.step_register_length store next instruction performed).trans size, result, resultApart, final⟩

/-- ABI transfers are a local derivation, using the existing consuming move.
    This is a logical premise, not a generated certificate family. -/
inductive Transfers : Resource → List (Nat × Nat) → Resource → Prop where
  | nil (resource : Resource) : Transfers resource [] resource
  | cons {before middle after : Resource} {dst src : Nat} {value : Core.Value} {rest : List (Nat × Nat)}
      (ready : MoveInput before dst src value) (effect : middle = moved before dst src value)
      (tail : Transfers middle rest after) : Transfers before ((dst, src) :: rest) after

theorem Transfers.execute (before after : Resource) (transfers : List (Nat × Nat))
    (proof : Transfers before transfers after) (frame : Resource) (store : Core.StateOf Core.Value)
    (apart : Resource.Disjoint before frame) (realized : StoreRealizes store (Resource.join before frame)) :
    ∃ next, Core.Call.moveRegisters store transfers = .ok next ∧
      next.registers.length = store.registers.length ∧ Resource.Disjoint after frame ∧
      StoreRealizes next (Resource.join after frame) := by
  induction proof generalizing store with
  | nil resource => exact ⟨store, rfl, rfl, apart, realized⟩
  | @cons before middle after dst src value rest ready effect tail ih =>
    obtain ⟨next, output, performed, count, result, resultApart, final⟩ :=
      StoreTriple.atomic store.registers.length (.move dst src) _ _ (move_rule dst src value)
        (fun _ _ _ impossible => by cases impossible.1) before frame store ready apart realized rfl
    have same : output = middle := result.2.trans effect.symm
    subst output
    obtain ⟨last, executed, lastCount, lastApart, lastRealized⟩ := ih next resultApart final
    exact ⟨last, by simp only [Core.Call.moveRegisters, performed, executed, Bind.bind],
      lastCount.trans count, lastApart, lastRealized⟩

theorem StoreTriple.of_effect (beforeCount afterCount : Nat)
    (transition : Core.StateOf Core.Value → Core.Result (Core.StateOf Core.Value))
    (pre : Resource → Prop) (post : Resource → Resource → Prop)
    (preserves : ∀ store next, ValidStore store → transition store = .ok next → ValidStore next)
    (derive : ∀ resource store, pre resource → ValidStore store → Agrees store resource →
      store.registers.length = beforeCount →
      ∃ next written, transition store = .ok next ∧ next.registers.length = afterCount ∧
        AllowsWrites store resource written ∧ UnchangedOutside written store next ∧
        post resource (resource.patch written (observe next))) :
    StoreTriple beforeCount afterCount transition pre post := by
  intro resource frame store input apart realized count
  have agreement := StoreRealizes.agrees store _ realized
  have localKnown := ((agrees_join store resource frame apart).mp agreement).1
  obtain ⟨next, written, performed, length, allowed, effect, output⟩ :=
    derive resource store input realized.1 localKnown count
  exact ⟨next, _, performed, length, output,
    (patch_locality store next resource frame written apart agreement allowed effect).1,
    preserves store next realized.1 performed,
    patch_complete store next resource frame written apart realized.2 allowed effect⟩

end Witchy.Logic
