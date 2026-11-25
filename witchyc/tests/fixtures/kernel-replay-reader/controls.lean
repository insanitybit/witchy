import Witchy.KernelProofReader
import Witchy.AuditRules
import Lean.Elab.Tactic.Basic

open Lean Meta Elab Tactic
open Witchy.KernelProofReader

private def ownedPaths : IO (System.FilePath × System.FilePath × System.FilePath) := do
  let some input ← IO.getEnv "WITCHY_READER_INPUT" | throw (IO.userError "missing owned input")
  let some directory ← IO.getEnv "WITCHY_READER_OWNED" | throw (IO.userError "missing owned directory")
  let some sentinel ← IO.getEnv "WITCHY_READER_SENTINEL" | throw (IO.userError "missing owned sentinel")
  return (input,directory,sentinel)

private def getSnapshot : TacticM Snapshot := do
  let (input,directory,sentinel) ← ownedPaths
  if ← sentinel.pathExists then throwError "old initializer ran before raw reading"
  let some data ← readOwned? input directory | throwError "legacy main object unexpectedly missed"
  if ← sentinel.pathExists then throwError "raw reader executed an old initializer"
  if (← getEnv).contains `RawReaderOld.original then throwError "raw reader imported the old environment"
  return data

elab "reader_replay_original" : tactic => do
  let data ← getSnapshot
  let some owned := candidate? data `RawReaderOld.original (← getEnv) | throwError "raw auxiliary closure missed"
  -- No fixture-global Snapshot cache: the reader arena must retain every raw
  -- value after this local handle dies and through current serialization.
  unless ← Witchy.KernelProofReplay.tryReplay owned.candidate (← getMainGoal) do
    throwError "raw auxiliary closure did not pass current kernel replay"
  replaceMainGoal []

namespace Witchy.KernelReaderExamples

theorem owned_auxiliary_extra (noise y x : Nat) (_extra : noise = noise) (current_equality : x = y) : x = y := by
  reader_replay_original

def currentMark : Nat := 1

elab "reader_negative_controls" : tactic => do
  let saved ← saveState
  let data ← getSnapshot
  let current ← getEnv
  let some owned := candidate? data `RawReaderOld.mark_zero current | throwError "local alias closure missed"
  if current.contains `RawReaderOld.localMark then throwError "reader installed an old local definition"
  let falseType ← mkEq (mkConst ``currentMark) (mkNatLit 0)
  let falseGoal ← mkFreshExprMVar falseType
  if ← Witchy.KernelProofReplay.tryReplay owned.candidate falseGoal.mvarId! then
    throwError "old data alias proved a false current definition"
  if ← falseGoal.mvarId!.isAssigned then throwError "false goal assigned on MISS"
  if (candidate? data `RawReaderOld.unsupported current).isSome then throwError "opaque old declaration accepted"
  if (candidate? data `RawReaderOld.universe_polymorphic current).isSome then throwError "old universes accepted"
  if (candidate? data `RawReaderOld.original current { maxNodes := 0 }).isSome then throwError "node budget ignored"
  if (candidate? data `RawReaderOld.original current { maxDepth := 0 }).isSome then throwError "depth budget ignored"
  if (candidate? data `RawReaderOld.missing current).isSome then throwError "selector became authority"
  let (_,directory,sentinel) ← ownedPaths
  let some cyclic ← readOwned? (directory / "cycle.olean") directory | throwError "raw cycle fixture unreadable"
  if (candidate? cyclic `RawReaderCycle.proof current).isSome then throwError "cyclic closure accepted"
  let some unresolved ← readOwned? (directory / "mvar.olean") directory | throwError "raw mvar fixture unreadable"
  if (candidate? unresolved `RawReaderMVar.proof current).isSome then throwError "unresolved old metavariable accepted"
  let truncated := directory / "truncated.olean"
  IO.FS.writeBinFile truncated "olean".toUTF8
  if (← readOwned? truncated directory).isSome then throwError "truncated file accepted"
  if ← sentinel.pathExists then throwError "old initializer executed"
  saved.restore
  evalTactic (← `(tactic| trivial))

theorem aliases_false_unknown_universe_and_budgets_miss : True := by
  reader_negative_controls

-- This command runs LAST: dropping handles cannot reset monotonic reservations.
run_cmd do
  let (input,directory,sentinel) ← ownedPaths
  for _ in [:8] do
    let _ ← readOwned? input directory
    pure ()
  if (← reservedBytes) != 64 * 1024 * 1024 then throwError "arena reservation budget not monotonic"
  if (← readOwned? input directory).isSome then throwError "exhausted arena accepted another mapping"
  if ← sentinel.pathExists then throwError "old initializer executed while exhausting arena"
  if (← getEnv).contains `RawReaderOld.original then throwError "old environment leaked into current environment"

end Witchy.KernelReaderExamples

#audit_witchy
