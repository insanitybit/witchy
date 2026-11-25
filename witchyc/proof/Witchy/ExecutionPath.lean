import Witchy.CoreComposition

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- A proof witness for one actual control-flow edge. Branch destinations come
    from the observed Boolean; no alternative executor or instruction semantics. -/
inductive ExecutionEdge (program : Array Instruction) : Nat → (StateOf Cell) → Nat → (StateOf Cell) → Prop where
  | jump {pc target : Nat} {s : (StateOf Cell)}
      (code : program[pc]? = some (.jump target)) : ExecutionEdge program pc s target s
  | branch {pc condition target : Nat} {s : (StateOf Cell)} {flag : Bool}
      (code : program[pc]? = some (.branchFalse condition target))
      (value : get s condition = .ok (.boolean flag)) :
      ExecutionEdge program pc s (if flag then pc + 1 else target) s
  | instruction {pc : Nat} {s next : (StateOf Cell)} {instruction : Instruction}
      (code : program[pc]? = some instruction) (accepted : step s instruction = .ok next) :
      ExecutionEdge program pc s (pc + 1) next

theorem ExecutionEdge.continues (program : Array Instruction) (pc target : Nat) (s next : (StateOf Cell))
    (edge : ExecutionEdge program pc s target next) : Continues program 1 pc s target next := by
  intro fuel
  cases edge with
  | jump code => simp only [run, code]
  | branch code value => simp only [run, code, value]
  | @instruction _ _ instruction code accepted =>
    cases instruction <;> simp only [run, code]
    all_goals first
      | (simp only [step] at accepted; cases accepted)
      | (rw [accepted])

/-- Actual finite prefixes may revisit any address. Unlike a freshness witness,
    this relation places no numerical restriction on intermediate code addresses. -/
inductive ExecutionPath (program : Array Instruction) : Nat → Nat → (StateOf Cell) → Nat → (StateOf Cell) → Prop where
  | refl (pc : Nat) (s : (StateOf Cell)) : ExecutionPath program 0 pc s pc s
  | advance {cost pc middle target : Nat} {s next final : (StateOf Cell)}
      (edge : ExecutionEdge program pc s middle next)
      (rest : ExecutionPath program cost middle next target final) :
      ExecutionPath program (cost + 1) pc s target final

theorem ExecutionPath.continues (program : Array Instruction) (cost pc target : Nat) (s final : (StateOf Cell))
    (path : ExecutionPath program cost pc s target final) : Continues program cost pc s target final := by
  induction path with
  | refl pc state => exact continues_refl program pc state
  | advance edge rest ih =>
    have combined := continues_trans program 1 _ _ _ _ _ _ _ (edge.continues program _ _ _ _) ih
    simpa only [Nat.add_comm 1] using combined

theorem ExecutionPath.append (program : Array Instruction) (firstCost secondCost pc middle target : Nat)
    (s intermediate final : (StateOf Cell)) (first : ExecutionPath program firstCost pc s middle intermediate)
    (second : ExecutionPath program secondCost middle intermediate target final) :
    ExecutionPath program (firstCost + secondCost) pc s target final := by
  induction first with
  | refl address state => simpa only [Nat.zero_add] using second
  | advance edge rest ih =>
    have combined := ExecutionPath.advance edge (ih second)
    simpa only [Nat.add_assoc, Nat.add_comm 1] using combined

end Witchy.Core
