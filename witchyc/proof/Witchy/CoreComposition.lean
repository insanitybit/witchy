import Witchy.CoreStep

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- A finite prefix of actual core execution, valid for every continuation fuel
    amount. This is a logical relation over Core.run, not another interpreter. -/
def Continues (program : Array Instruction) (cost pc : Nat) (s : (StateOf Cell))
    (exit : Nat) (next : (StateOf Cell)) : Prop :=
  ∀ fuel, run program (fuel + cost) pc s = run program fuel exit next

theorem continues_refl (program : Array Instruction) (pc : Nat) (s : (StateOf Cell)) :
    Continues program 0 pc s pc s := by
  intro fuel
  rfl

theorem continues_trans (program : Array Instruction) (firstCost secondCost pc middle exit : Nat)
    (s intermediate next : (StateOf Cell))
    (first : Continues program firstCost pc s middle intermediate)
    (second : Continues program secondCost middle intermediate exit next) :
    Continues program (firstCost + secondCost) pc s exit next := by
  intro fuel
  calc
    run program (fuel + (firstCost + secondCost)) pc s =
        run program ((fuel + secondCost) + firstCost) pc s := by
      congr 1
      omega
    _ = run program (fuel + secondCost) middle intermediate := first _
    _ = run program fuel exit next := second _

/-- The prefix has not silently faulted, halted early or skipped state changes:
    at exactly its cost it reaches the specified state and exhausts fuel. -/
theorem continues_exact_prefix (program : Array Instruction) (cost pc exit : Nat)
    (s next : (StateOf Cell)) (execution : Continues program cost pc s exit next) :
    run program cost pc s = (next, .error .fuel) := by
  simpa only [Nat.zero_add, run] using execution 0

theorem continues_zero_preserves_state (program : Array Instruction) (pc exit : Nat)
    (s next : (StateOf Cell)) (execution : Continues program 0 pc s exit next) : s = next := by
  have prefixState := continues_exact_prefix program 0 pc exit s next execution
  simpa only [run, Prod.mk.injEq, and_true] using prefixState

theorem continues_then_halt (program : Array Instruction) (cost pc exit : Nat)
    (s next : (StateOf Cell)) (execution : Continues program cost pc s exit next)
    (halt : program[exit]? = some .halt) (clean : terminal next = true) :
    run program (cost + 1) pc s = (next, .ok ()) := by
  have continuation := execution 1
  have tail : run program 1 exit next = (next, .ok ()) := by simp [run, halt, clean]
  calc
    run program (cost + 1) pc s = run program 1 exit next := by
      rw [Nat.add_comm cost 1]
      exact continuation
    _ = (next, .ok ()) := tail

/-- A decreasing natural rank composes checked body prefixes into a complete
    finite loop. Clients must establish the body invariant and the zero-rank exit
    against actual Core.run; no assumed loop behavior is installed as an axiom. -/
theorem decreasing_loop_continues (program : Array Instruction) (header exit bodyCost exitCost : Nat)
    (invariant : Nat → (StateOf Cell) → Prop) (postcondition : (StateOf Cell) → Prop)
    (finish : ∀ state, invariant 0 state → ∃ next,
      Continues program exitCost header state exit next ∧ postcondition next)
    (iteration : ∀ remaining state, invariant (remaining + 1) state → ∃ next,
      Continues program bodyCost header state header next ∧ invariant remaining next) :
    ∀ remaining state, invariant remaining state → ∃ next,
      Continues program (remaining * bodyCost + exitCost) header state exit next ∧ postcondition next := by
  intro remaining
  induction remaining with
  | zero =>
    intro state valid
    simpa only [Nat.zero_mul, Nat.zero_add] using finish state valid
  | succ remaining ih =>
    intro state valid
    obtain ⟨middle, body, preserved⟩ := iteration remaining state valid
    obtain ⟨next, rest, post⟩ := ih middle preserved
    have composed := continues_trans program bodyCost (remaining * bodyCost + exitCost)
      header header exit state middle next body rest
    have cost : bodyCost + (remaining * bodyCost + exitCost) =
        (remaining + 1) * bodyCost + exitCost := by
      simp only [Nat.add_mul, Nat.one_mul]
      omega
    exact ⟨next, cost ▸ composed, post⟩

end Witchy.Core
