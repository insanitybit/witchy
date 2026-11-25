import Witchy.KernelProofReader
import Lean.Elab.Tactic.Basic
import Witchy.SourceObligationSyntax

/-! Search-only old proof candidates. An absent, malformed, unsupported or
inapplicable candidate always leaves ordinary CURRENT proof search in charge.
No old module is imported and no old declaration enters the environment. -/
namespace Witchy.SourceProofReplay
open Lean Elab Tactic
open KernelProofReader

-- Bounded process-local raw snapshots, never checked theorem/environment caches.
-- The reader independently retains its capped compacted-region arena until exit.
private initialize snapshots : IO.Ref (Array (String × Option Snapshot)) ← IO.mkRef #[]

private def snapshot? : TacticM (Option Snapshot) := do
  let module := (← getMainModule).toString
  if module.isEmpty || !module.toList.all (fun c => c.isAlphanum || c == '_') then return none
  let file : System.FilePath := (← readThe Core.Context).fileName
  let some directory := file.parent | return none
  let key := file.toString
  let cached ← snapshots.get
  if let some entry := cached.find? (fun entry => entry.1 == key) then return entry.2
  if cached.size ≥ 8 then return none
  let input := directory / ".witchy-replay" / (module ++ ".olean")
  let owned := directory / ".witchy-replay" / ("snapshot-" ++ module)
  let candidate ← readOwned? input owned
  snapshots.set (cached.push (key,candidate))
  return candidate

@[tactic Witchy.sourceObligationReplay]
private def evalSourceObligationReplay : Tactic := fun _ => do
  let saved ← saveState
  let replayed ← try
    let some data ← snapshot? | pure false
    let some name ← Term.getDeclName? | pure false
    let some owned := candidate? data name (← getEnv) | pure false
    KernelProofReplay.tryReplay owned.candidate (← getMainGoal)
  catch _ => pure false
  if replayed then
    replaceMainGoal []
  else
    saved.restore
    throwError "no applicable current-kernel-checked source proof candidate"

end Witchy.SourceProofReplay
