import Lean.Meta.Check
import Lean.Util.CollectAxioms

/-! A raw old term is a search candidate, never admission authority. This helper
uses only the CURRENT environment. It neither imports an old module nor installs
an old declaration. A miss leaves the goal and meta state available for the
ordinary tactic. No compiler/cache integration is enabled by this module. -/
namespace Witchy.KernelProofReplay
open Lean Meta

structure Candidate where
  type : Expr
  value : Expr
  levelParams : List Name := []

structure Limits where
  maxNodes : Nat := 65536
  maxBinders : Nat := 256
  maxConstants : Nat := 1024

/-- Structural work cap, independent of ordinary Lean limits (never raised). -/
private def fits : Nat → List Expr → Bool
  | _, [] => true
  | 0, _ => false
  | fuel + 1, e :: rest =>
    match e with
    | .app f a => fits fuel (f :: a :: rest)
    | .lam _ t b _ | .forallE _ t b _ => fits fuel (t :: b :: rest)
    | .letE _ t v b _ => fits fuel (t :: v :: b :: rest)
    | .mdata _ b | .proj _ _ b => fits fuel (b :: rest)
    | _ => fits fuel rest

private def closed (e : Expr) : Bool :=
  !e.hasMVar && !e.hasFVar && !e.hasLooseBVars && !e.hasLevelParam

private def standardAxioms (value type : Expr) (limits : Limits) : MetaM Bool := do
  let constants := value.getUsedConstants ++ type.getUsedConstants
  if constants.size > limits.maxConstants then return false
  for name in constants do
    -- Kernel checking has already rejected unknown constants. Collect from the
    -- current checked dependency closure, including types of unused premises.
    for axiomName in (← collectAxioms name) do
      unless #[`propext, `Classical.choice, `Quot.sound].contains axiomName do
        return false
  return true

private def argument? (name : Name) (type : Expr) (locals : Array Expr) : MetaM (Option Expr) := do
  let env ← getEnv
  let lctx ← getLCtx
  let mut named := #[]
  for fvar in locals do
    let declaration ← fvar.fvarId!.getDecl
    if declaration.userName == name then named := named.push fvar
  -- Runtime operands use exact binder identity. Ambiguous names never select
  -- a different same-typed runtime operand by accident.
  if named.size == 1 then
    let fvar := named[0]!
    if Kernel.isDefEqGuarded env lctx (← inferType fvar) type then
      return some fvar
  if !(← isProp type) then return none
  -- Premise positions/names may change. A current proof of the exact required
  -- proposition is enough; duplicate proofs of that proposition are harmless.
  for fvar in locals do
    let actual ← inferType fvar
    if (← isProp actual) && Kernel.isDefEqGuarded env lctx actual type then
      return some fvar
  return none

private def build? (old : Candidate) (goal : MVarId) (limits : Limits) : MetaM (Option Expr) := do
  if !(old.levelParams.isEmpty && closed old.type && closed old.value)
      || !fits limits.maxNodes [old.type, old.value] then return none
  if !(← isProp old.type) then return none
  let env ← getEnv
  -- First reject forged old types/values, changed current definitions and
  -- missing old private auxiliaries BEFORE using the claimed forall shape.
  let .ok actualOldType := Kernel.check env {} old.value | return none
  if !Kernel.isDefEqGuarded env {} actualOldType old.type then return none
  let mut locals := #[]
  for declaration in (← getLCtx) do
    locals := locals.push (mkFVar declaration.fvarId)
  if locals.size > limits.maxBinders then return none
  let target ← instantiateMVars (← goal.getDecl).type
  if target.hasMVar || target.hasLevelParam || !(← isProp target) then return none
  let mut current := old.type
  let mut proof := old.value
  for _ in [:limits.maxBinders] do
    match current with
    | .forallE name domain body _ =>
      let some argument ← argument? name domain locals | return none
      proof := mkApp proof argument
      current := body.instantiate1 argument
    | _ => break
  if current.isForall then return none
  if !Kernel.isDefEqGuarded env (← getLCtx) current target then return none
  let closedType ← instantiateMVars (← mkForallFVars locals target (usedOnly := false)
    (usedLetOnly := false) (generalizeNondepLet := false))
  let closedProof ← instantiateMVars (← mkLambdaFVars locals proof (usedOnly := false)
    (usedLetOnly := false) (etaReduce := false) (generalizeNondepLet := false))
  if !(closed closedType && closed closedProof)
      || !fits limits.maxNodes [closedType, closedProof] then return none
  -- Kernel.check infers a type; this explicit identity application forces the
  -- kernel itself to compare the proof with the exact CURRENT closed claim.
  -- Meta inference/defeq and the claimed old type are not the final authority.
  let checked := mkApp (mkLambda `_replayed .default closedType (mkBVar 0)) closedProof
  let .ok _ := Kernel.check env {} checked | return none
  if !(← standardAxioms closedProof closedType limits) then return none
  return some proof

/-- Replay only after explicit current closed-term kernel checking and the
standard-axiom scan. All current binders/lets/premises remain in the claim.
Unsupported universes, malformed terms, missing premises, budgets and every
kernel/meta exception are clean misses with complete state rollback. -/
def tryReplay (old : Candidate) (goal : MVarId) (limits : Limits := {}) : MetaM Bool := do
  let saved ← saveState
  try
    if ← goal.isAssignedOrDelayedAssigned then return false
    let proof? ← goal.withContext do
      -- Normalize CURRENT assigned metavariables before abstracting let values;
      -- otherwise instantiation after abstraction could expose free variables.
      let lctx ← instantiateLCtxMVars (← getLCtx)
      withLCtx lctx (← getLocalInstances) (build? old goal limits)
    match proof? with
    | none =>
      saved.restore
      return false
    | some proof =>
      goal.assign proof
      return true
  catch _ =>
    saved.restore
    return false

end Witchy.KernelProofReplay
