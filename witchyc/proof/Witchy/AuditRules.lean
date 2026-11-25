import Lean.Elab.ElabRules
import Lean.Elab.Deriving.BEq
import Lean.Elab.GuardMsgs
import Lean.Elab.Do
import Lean.Util.CollectAxioms
import Lean.AddDecl
import Lean.Linter
import Witchy.AuditCore

open Lean

open Lean Elab Command in
def WitchyAudit.expectCommandReject : CommandElabM Unit := do
  let saved ← get
  let messages ← try
    Lean.Elab.Tactic.GuardMsgs.runAndCollectMessages (← `(#audit_witchy))
  finally
    set saved
  let errors := messages.toList.filter (·.severity == .error)
  let [error] := errors | throwError "WPROOF003: production audit command did not reject exactly once"
  unless (← error.data.toString).startsWith "WPROOF001:" do
    throwError "WPROOF003: production audit command rejected with an unrelated error"

/- Dependency-tracked adversarial checks use the same scan as the production
    command. Injected declarations exist only in temporary environments and are
    restored even if a check fails; no test axiom is exported. -/
open Lean Elab Command in
elab "#test_witchy_audit" : command => do
  let saved ← getEnv
  let direct := `Witchy.AuditProbe.direct
  try
    liftCoreM <| Lean.addDecl (.axiomDecl {
      name := direct, levelParams := [], type := mkConst ``False, isUnsafe := false })
    match ← WitchyAudit.scan with
    | .error (.axiom name) =>
      unless name == direct do throwError "WPROOF003: wrong direct axiom rejection"
    | _ => throwError "WPROOF003: audit accepted a direct axiom or reported the wrong cause"
    WitchyAudit.expectCommandReject
  finally
    setEnv saved

  let assumption := `WitchyAuditProbe.assumption
  let theoremName := `Witchy.AuditProbe.indirect
  try
    liftCoreM <| Lean.addDecl (.axiomDecl {
      name := assumption, levelParams := [], type := mkConst ``False, isUnsafe := false })
    liftCoreM <| Lean.addDecl (.thmDecl {
      name := theoremName, levelParams := [], type := mkConst ``False, value := mkConst assumption })
    match ← WitchyAudit.scan with
    | .error (.dependency name axiomName) =>
      unless name == theoremName && axiomName == assumption do
        throwError "WPROOF003: wrong external axiom rejection"
    | _ => throwError "WPROOF003: audit accepted a theorem backed by an external axiom or reported the wrong cause"
    WitchyAudit.expectCommandReject
  finally
    setEnv saved

  for name in [direct, assumption, theoremName] do
    if (← getEnv).contains name then
      throwError "WPROOF003: audit probe leaked a declaration"
  match ← WitchyAudit.scan with
  | .error _ => throwError "WPROOF003: production audit failed after probes restored the environment"
  | .ok _ => logInfo "Audit rejected direct and indirect axioms; temporary environments restored."
