import Witchy.KernelProofReplay
import Witchy.AuditRules
import Lean.Elab.Tactic.Basic

open Lean Meta Elab Tactic
open Witchy.KernelProofReplay

namespace Witchy.KernelReplayExamples

theorem original (x y : Nat) (h : x = y) : x = y := h

def currentMark : Nat := 1

private def originalCandidate : MetaM Candidate := do
  let .thmInfo info ← getConstInfo ``original | throwError "expected the actual old theorem"
  return { type := info.type, value := info.value, levelParams := info.levelParams }

elab "replay_original" : tactic => do
  let goal ← getMainGoal
  unless ← tryReplay (← originalCandidate) goal do
    throwError "current-kernel replay unexpectedly missed"
  replaceMainGoal []

elab "expect_original_miss" : tactic => do
  let goal ← getMainGoal
  if ← tryReplay (← originalCandidate) goal then
    throwError "unexpected replay without the necessary current premise"
  if ← goal.isAssigned then throwError "MISS assigned the goal"

elab "expect_budget_miss" : tactic => do
  let goal ← getMainGoal
  if ← tryReplay (← originalCandidate) goal { maxBinders := 0 } then
    throwError "binder budget was ignored"
  if ← goal.isAssigned then throwError "budget MISS assigned the goal"

/-- Different premise positions/names; the extra premise remains in this type. -/
theorem reordered_extra (noise y x : Nat) (_extra : noise = noise) (equality : x = y) : x = y := by
  replay_original

/-- Current lets are closed as lets, including ones unused by the old proof. -/
theorem current_let (x y : Nat) (h : x = y) : x = y := by
  let extra : Nat := 17
  have _keep : extra = 17 := rfl
  replay_original

/-- Failure leaves the original proving path available. -/
theorem missing_premise_fallback (x y : Nat) (_keep_y : y = y) : x = x := by
  expect_original_miss
  rfl

theorem budget_fallback (x y : Nat) (h : x = y) : x = y := by
  expect_budget_miss
  exact h

elab "replay_malformed_controls" : tactic => do
  let saved ← saveState
  let goal ← getMainGoal
  let good : Candidate := { type := mkConst ``True, value := mkConst ``True.intro }
  let assertMiss (candidate : Candidate) (limits : Limits := {}) : TacticM Unit := do
    if ← tryReplay candidate goal limits then throwError "bad replay candidate succeeded"
    if ← goal.isAssigned then throwError "bad replay candidate assigned on MISS"
  -- Forged old type must not be accepted from Meta.inferType alone.
  assertMiss { good with type := mkConst ``False }
  -- Unknown old private helper cannot be loaded or silently admitted.
  assertMiss { good with value := mkConst `_private.MissingReplayModule.missingProof }
  assertMiss { good with levelParams := [`u] }
  -- Existing forbidden axiom supplied ONLY as raw untrusted input data. This
  -- introduces no declaration/axiom and never enters the resulting proof.
  -- It is kernel-typable, so this test specifically reaches the AX rejection.
  let forbidden : Candidate := { good with value := mkApp2 (mkConst ``sorryAx [Level.zero]) (mkConst ``True) (mkConst ``Bool.false) }
  let .ok _ := Kernel.check (← getEnv) {} forbidden.value
    | throwError "bad AX-control fixture: input must be kernel-typable"
  assertMiss forbidden
  assertMiss good { maxNodes := 0 }
  let unresolved ← mkFreshExprMVar (mkConst ``True)
  assertMiss { good with value := unresolved }
  -- Simulate an old proof of mark = 0 after CURRENT mark has become 1.
  -- The raw old term is refl 0, not a reference to an old environment.
  let oldType := mkApp3 (mkConst ``Eq [Level.succ Level.zero]) (mkConst ``Nat)
    (mkConst ``currentMark) (mkNatLit 0)
  let oldValue := mkApp2 (mkConst ``Eq.refl [Level.succ Level.zero]) (mkConst ``Nat) (mkNatLit 0)
  assertMiss { type := oldType, value := oldValue }
  let falseGoal ← mkFreshExprMVar (mkConst ``False)
  if ← tryReplay good falseGoal.mvarId! then throwError "replayed a false current target"
  if ← falseGoal.mvarId!.isAssigned then throwError "false target assigned on MISS"
  saved.restore
  evalTactic (← `(tactic| trivial))

/-- Malformed/unavailable/metavariable/universe/budget/changed-definition and
false-target attempts do not publish any theorem from their candidate data. -/
theorem malformed_and_false_controls : True := by
  replay_malformed_controls

end Witchy.KernelReplayExamples

#audit_witchy
