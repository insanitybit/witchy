import Lean.CoreM
import Lean.Util.CollectAxioms

open Lean

namespace WitchyAudit

inductive Failure where
  | axiom (name : Name)
  | dependency (theoremName axiomName : Name)
  | empty
  deriving BEq

def scan [Monad m] [MonadEnv m] : m (Except Failure Nat) := do
  let mut count : Nat := 0
  for (name, info) in (← getEnv).constants.toList do
    if (`Witchy).isPrefixOf name then
      match info with
      | .axiomInfo _ => return .error (.axiom name)
      | .thmInfo _ =>
        for axiomName in (← collectAxioms name) do
          unless #[`propext, `Classical.choice, `Quot.sound].contains axiomName do
            return .error (.dependency name axiomName)
        count := count + 1
      | _ => pure ()
  if count == 0 then return .error .empty
  return .ok count

end WitchyAudit

macro "#audit_witchy" : command =>
  `(command|
    #eval ((do
      match ← WitchyAudit.scan with
      | .error (.axiom name) => throwError "WPROOF001: untrusted axiom {name}"
      | .error (.dependency name axiomName) => throwError "WPROOF001: theorem {name} depends on {axiomName}"
      | .error .empty => throwError "WPROOF002: no Witchy theorems audited"
      | .ok count => logInfo m!"Audited {count} Witchy theorems; only standard logical foundations allowed."
    ) : Lean.CoreM Unit))
