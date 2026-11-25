import Witchy.KernelProofReplay
import Std.Sync.Mutex
import Lean.Util.ReplaceExpr
import Lean.Util.FindExpr

/-! RAW legacy main-.olean candidate data, never an imported environment.
The caller supplies an original-receipt-validated main object and an exclusive
owned snapshot directory. No companion, import, initializer or extension entry
is loaded/evaluated. The current-kernel replay adapter remains the authority.

SDK lifetime: CompactedRegion.free is EXPLICIT and unsafe; contained objects
have no individual reference counts. We NEVER call it. Every successful mapping
is retained in a process-lifetime arena, including rejected metadata, before any
Expr/Name/Level can escape. Monotonic reservations cap this at eight 8-MiB files
(64 MiB reserved file mappings), NOT an RSS cap. Dropping a Snapshot or Candidate
does not replenish the budget. Reap the worker to reclaim the arena. -/
namespace Witchy.KernelProofReader
open Lean

structure Limits where
  maxFileBytes : Nat := 8 * 1024 * 1024
  maxDeclarations : Nat := 8192
  maxClosure : Nat := 128
  maxDepth : Nat := 64
  maxNodes : Nat := 1000000

private structure Arena where
  reservations : Nat := 0
  regions : Array (ModuleData × CompactedRegion) := #[]
  deriving Inhabited

private initialize arena : Std.Mutex Arena ← Std.Mutex.new {}

structure Snapshot where
  private data : ModuleData

structure OwnedCandidate where
  private owner : Snapshot
  candidate : KernelProofReplay.Candidate

private def nameDepth : Nat → Name → Bool
  | 0, _ => false
  | _ + 1, .anonymous => true
  | fuel + 1, .str parentName component => component.utf8ByteSize ≤ 1024 && nameDepth fuel parentName
  | fuel + 1, .num parentName number => number < 4294967296 && nameDepth fuel parentName

private def nameOK (name : Name) : Bool :=
  nameDepth 64 name && name.toString.utf8ByteSize ≤ 1024

private def saneLimits (limits : Limits) : Bool :=
  limits.maxFileBytes ≤ 8 * 1024 * 1024 && limits.maxDeclarations ≤ 8192 &&
  limits.maxClosure ≤ 128 && limits.maxDepth ≤ 64 && limits.maxNodes ≤ 1000000

private def readBounded (path : System.FilePath) (bound : Nat) : IO (Option ByteArray) :=
  IO.FS.withFile path .read fun handle => do
    let mut bytes := ByteArray.empty
    while bytes.size ≤ bound do
      let chunk ← handle.read (USize.ofNat (min 65536 (bound + 1 - bytes.size)))
      if chunk.isEmpty then return some bytes
      bytes := bytes ++ chunk
    return none

/-- Copy only the supplied main object into a fresh, never-rewritten owned
file before mmap, avoiding aliases to the caller's mutable cache object. The
owned directory must remain exclusive to this request. No other path is read.
Version 2 legacy module data only: split-module proof bodies require sidecars
which this API deliberately never reads. -/
def readOwned? (mainObject ownedDirectory : System.FilePath) (limits : Limits := {}) : IO (Option Snapshot) := do
  try
    if !saneLimits limits || mainObject.extension != some "olean" then return none
    let source ← mainObject.symlinkMetadata
    let directory ← ownedDirectory.symlinkMetadata
    if source.type != .file || directory.type != .dir || source.byteSize.toNat > limits.maxFileBytes then return none
    let some bytes ← readBounded mainObject limits.maxFileBytes | return none
    if bytes.size != source.byteSize.toNat || bytes.size < 7 then return none
    -- The current SDK documents ordinary compacted data as v2; observed current
    -- SDK/legacy writer prefix is 'olean', version 2, little-endian tag 1.
    -- readModuleData still performs its own full version/ABI validation.
    if bytes.extract 0 7 != [111,108,101,97,110,2,1].toByteArray then return none
    let slot? ← arena.atomically do
      let state ← get
      if state.reservations ≥ 8 then return none
      modify fun state => { state with reservations := state.reservations + 1 }
      return some state.reservations
    let some slot := slot? | return none
    let snapshotFile := ownedDirectory / s!"raw-proof-{slot}.olean"
    IO.FS.withFile snapshotFile .writeNew fun handle => handle.write bytes
    let loaded ← readModuleData snapshotFile
    -- Keep rejected data too. No region or any contained root is ever freed.
    arena.atomically do modify fun state => { state with regions := state.regions.push loaded }
    let data := loaded.1
    if loaded.2.size.toNat > limits.maxFileBytes || data.isModule ||
        data.constants.size > limits.maxDeclarations || data.constNames.size != data.constants.size ||
        data.imports.size > 512 || data.extraConstNames.size > 8192 then return none
    let mut names : NameSet := {}
    for index in [:data.constants.size] do
      let name := data.constants[index]!.name
      if !nameOK name || data.constNames[index]! != name || names.contains name then return none
      names := names.insert name
    if data.imports.any (fun entry => !nameOK entry.module) || data.extraConstNames.any (fun name => !nameOK name) then return none
    return some { data }
  catch _ => return none

/-- Reservations are deliberately monotonic, including malformed/read failures. -/
def reservedBytes : IO Nat := arena.atomically do return (← get).reservations * (8 * 1024 * 1024)

private def consume : Nat → List (Expr × Nat) → Option Nat
  | fuel, [] => some fuel
  | 0, _ => none
  | fuel + 1, (expression, depth) :: rest =>
    if depth > 4096 then none else
    match expression with
    | .app f a => consume fuel ((f,depth+1)::(a,depth+1)::rest)
    | .lam name type body _ | .forallE name type body _ =>
      if nameOK name then consume fuel ((type,depth+1)::(body,depth+1)::rest) else none
    | .letE name type value body _ =>
      if nameOK name then consume fuel ((type,depth+1)::(value,depth+1)::(body,depth+1)::rest) else none
    | .mdata _ _ => none -- Unknown metadata payloads are not candidate data.
    | .proj name _ body => if nameOK name then consume fuel ((body,depth+1)::rest) else none
    | .const name _ => if nameOK name then consume fuel rest else none
    | _ => consume fuel rest

private structure Closure where
  ordered : Array (Name × Expr × Expr) := #[]
  visiting : NameSet := {}
  done : NameSet := {}
  fuel : Nat

private def payload? (info : ConstantInfo) : Option (Expr × Expr) :=
  match info with
  | .thmInfo value => if value.levelParams.isEmpty then some (value.type,value.value) else none
  | .defnInfo value => if value.levelParams.isEmpty && value.safety == .safe then some (value.type,value.value) else none
  | _ => none

private def closedMono (e : Expr) : Bool :=
  !e.hasFVar && !e.hasMVar && !e.hasLooseBVars && !e.hasLevelParam

private def visit (snapshot : Snapshot) (current : Environment) (limits : Limits) :
    Nat → Name → Closure → Option Closure
  | 0, _, _ => none
  | depth + 1, name, state => do
    if state.done.contains name then return state
    if state.visiting.contains name || state.ordered.size ≥ limits.maxClosure then none else do
    let some info := snapshot.data.constants.find? (fun info => info.name == name) | none
    let some (type,value) := payload? info | none
    if !(closedMono type && closedMono value) then none else do
    let fuel ← consume state.fuel [(type,0),(value,0)]
    let mut state := { state with visiting := state.visiting.insert name, fuel }
    for dependency in type.getUsedConstants ++ value.getUsedConstants do
      if snapshot.data.constNames.contains dependency then
        state ← visit snapshot current limits depth dependency state
      else if !(current.contains dependency) then none
    if state.ordered.size ≥ limits.maxClosure then none else do
      return { state with
        ordered := state.ordered.push (name,type,value)
        visiting := state.visiting.erase name
        done := state.done.insert name }

/-- Expr.abstract abstracts ONLY fvars/mvars, not arbitrary constants. First
map selected monomorphic owned constants to distinct temporary fvars. They are
raw syntax, never installed in any local context or environment. Since input
expressions are closed, these IDs cannot capture any pre-existing free variable;
SDK abstraction then accounts for every surrounding lambda/forall/let depth. -/
private def abstractOwned? (expression : Expr) (names : Array Name) : Option Expr := do
  if expression.hasFVar || expression.hasMVar then none else do
  if names.isEmpty then return expression
  let bad := Expr.find? (fun node => match node with
    | .const name levels => names.contains name && !levels.isEmpty
    | _ => false) expression
  if bad.isSome then none else do
  let variables := names.mapIdx fun index _ =>
    mkFVar { name := .num (.str .anonymous "_witchyRawProofBinding") index }
  let replaced := Expr.replace (fun node => match node with
    | .const name [] => names.idxOf? name |>.map (fun index => variables[index]!)
    | _ => none) expression
  return replaced.abstract variables

/-- Inline only old-unit SAFE owned dependencies as shared closed lets in BOTH
old type and value. Foreign SDK constants stay references in the CURRENT env.
No old declaration is installed. Names select data, never admission authority. -/
def candidate? (snapshot : Snapshot) (name : Name) (current : Environment)
    (limits : Limits := {}) : Option OwnedCandidate := do
  if !saneLimits limits || !nameOK name then none else do
  let some (.thmInfo _) := snapshot.data.constants.find? (fun info => info.name == name) | none
  let state ← visit snapshot current limits limits.maxDepth name { fuel := limits.maxNodes }
  let some (_,type,value) := state.ordered.back? | none
  let auxiliaries := state.ordered.pop
  let mut names := #[]
  let mut lets := #[]
  for (name,type,value) in auxiliaries do
    let domain ← abstractOwned? type names
    let body ← abstractOwned? value names
    lets := lets.push (name,domain,body)
    names := names.push name
  let mut type ← abstractOwned? type names
  let mut value ← abstractOwned? value names
  for (name,domain,body) in lets.reverse do
    type := mkLet name domain body type (nondep := false)
    value := mkLet name domain body value (nondep := false)
  -- Expose the forall spine required by the replay adapter. These reductions
  -- are search preparation only; the entire value/type is checked again by135.
  -- A forged untyped self-application must not reach WHNF preparation.
  let .ok (.sort .zero) := Kernel.check current {} type | none
  let .ok exposedType := Kernel.whnf current {} type | none
  if !(closedMono exposedType && closedMono value) then none else do
  let _ ← consume limits.maxNodes [(exposedType,0),(value,0)]
  if (exposedType.getUsedConstants ++ value.getUsedConstants).any snapshot.data.constNames.contains then none else
  some { owner := snapshot, candidate := { type := exposedType, value } }

end Witchy.KernelProofReader
