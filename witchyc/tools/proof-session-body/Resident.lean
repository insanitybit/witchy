module
public import Lean
public meta import Lean
import all Lean.Environment
open Lean

-- Diagnostic only: no immutable-SDK capability or production admission.
@[extern "lean_internal_set_max_memory"]
opaque setRuntimeMemory (bytes : USize) : BaseIO Unit
@[extern "witchy_resident_deadline"]
opaque deadline (seconds : UInt32) : BaseIO Unit
private def imports : Array Name := #[
  `Lean.Elab.BuiltinCommand, `Lean.Elab.BuiltinTerm, `Lean.Elab.Declaration,
  `Lean.Elab.ElabRules, `Lean.Elab.BuiltinEvalCommand, `Lean.Elab.StructInst,
  `Lean.Elab.Tactic.Omega, `Lean.Elab.Tactic.Match, `Lean.Elab.Tactic.Simp,
  `Lean.Elab.Tactic.Unfold, `Lean.Util.CollectAxioms, `Init.Omega]
private def header := String.join (imports.toList.map fun n => s!"import {n}\n")
private def prelude (scope : String) : String :=
  "set_option autoImplicit false\nset_option debug.skipKernelTC false\nset_option linter.unusedSimpArgs false\nnamespace " ++ scope ++ "\nelab \"witchy_inline_omega\" : tactic =>\n  Lean.Elab.Tactic.liftMetaFinishingTactic fun goal => do\n    let some goal ← goal.falseOrByContra | return ()\n    goal.withContext do\n      let facts := (← Lean.getLocalHyps).toList\n      Lean.Elab.Tactic.Omega.omega facts goal {}\n"
private def options := Options.empty |>.setBool `debug.skipKernelTC false
  |>.set `max_memory (2048 : Nat) |>.setBool `Elab.async true
private def zero := String.ofList (List.replicate 64 '0')
private def hex (s : String) (n : Nat) : Bool :=
  s.length == n && s.toList.all (fun c => c.isDigit || ('a' ≤ c && c ≤ 'f'))
private def checked (r : Except String α) : IO α :=
  match r with
  | .ok x => pure x
  | .error e => throw (IO.userError e)
private def require (b : Bool) (message : String) : IO Unit :=
  unless b do throw (IO.userError message)
private def readExact (input : IO.FS.Stream) (size : Nat) : IO ByteArray := do
  let mut bytes := ByteArray.empty
  while bytes.size < size do
    let chunk ← input.read ((min 4096 (size - bytes.size)).toUSize)
    if chunk.isEmpty then throw (IO.userError "truncated frame")
    bytes := bytes ++ chunk
  return bytes
private def keys := ["v", "op", "binding", "nonce", "seq", "context", "source", "module", "body", "bytes", "marker"]
private def readFrame (input : IO.FS.Stream) : IO Json := do
  let mut bytes := ByteArray.empty
  for _ in [:4096] do
    let b ← readExact input 1
    if b[0]! == 10 then
      let some text := String.fromUTF8? bytes | throw (IO.userError "non-UTF8 frame")
      require (text.startsWith "{" && text.endsWith "}") "flat frame required"
      let fields := (String.ofList (text.toList.drop 1 |>.take (text.length - 2))).splitOn ","
      let mut names : List String := []
      for field in fields do
        match field.splitOn "\":" with
        | [key, _] =>
          let name ← checked ((← checked (Json.parse (key ++ "\""))).getStr?)
          require (keys.contains name && !names.contains name) "duplicate/unknown field"
          names := name :: names
        | _ => throw (IO.userError "non-flat metadata")
      require (names.length == keys.length) "missing field"
      return ← checked (Json.parse text)
    bytes := bytes ++ b
  throw (IO.userError "frame exceeds 4096 bytes")
private structure Request where
  op : String
  seq : Nat
  context : String
  source : String
  module : String
  body : String
  bytes : Nat
  marker : String
  deriving BEq
private def request (input : IO.FS.Stream) (binding nonce : String) (seq : Nat) (nextQueue : Option Nat := none) : IO Request := do
  let j ← readFrame input
  let op ← checked (j.getObjValAs? String "op")
  let seq := if op == "QUEUE" then nextQueue.getD 0 else seq
  require ((← checked (j.getObjValAs? Nat "v")) == 2 &&
    (← checked (j.getObjValAs? String "binding")) == binding &&
    (← checked (j.getObjValAs? String "nonce")) == nonce &&
    (← checked (j.getObjValAs? Nat "seq")) == seq) "version/identity/sequence mismatch"
  let r : Request := {
    op := op, seq := seq,
    context := ← checked (j.getObjValAs? String "context"),
    source := ← checked (j.getObjValAs? String "source"),
    module := ← checked (j.getObjValAs? String "module"),
    body := ← checked (j.getObjValAs? String "body"),
    bytes := ← checked (j.getObjValAs? Nat "bytes"),
    marker := ← checked (j.getObjValAs? String "marker") }
  require ([r.context, r.source, r.module, r.body].all (hex · 64)) "invalid digest"
  return r
private def reply (binding nonce : String) (r : Request) (status : String) : IO Unit := do
  let j := Json.mkObj [("v", toJson (2 : Nat)), ("phase", toJson r.op),
    ("status", toJson status), ("binding", toJson binding), ("nonce", toJson nonce),
    ("seq", toJson r.seq), ("context", toJson r.context), ("source", toJson r.source),
    ("module", toJson r.module), ("body", toJson r.body), ("marker", toJson r.marker),
    ("mode", toJson "BODY_INTERFACE_REUSE_DIAGNOSTIC")]
  let output ← IO.getStdout
  output.putStr s!"WITCHY_RESIDENT_V2:{j.compress}\n"
  output.flush
private def emptyPayload (r : Request) : Bool :=
  r.source == zero && r.module == zero && r.body == zero && r.bytes == 0 && r.marker.isEmpty
private def auditMessages (messages : MessageLog) (scope marker : String) (install : Bool) : IO Bool := do
  let mut markers := 0
  let mut axioms : List String := []
  let mut total := 0
  for message in messages.toList do
    let data ← message.data.toString
    for line in data.splitOn "\n" do
      if line == marker then markers := markers + 1
      if line.startsWith "WITCHY_SCALAR_AXIOMS:" then axioms := line :: axioms
    let raw ← message.toString
    -- Preserve every warning header/location/name, without repeating generated
    -- unused-binder hint paragraphs. All other diagnostics remain complete.
    let first := (data.splitOn "\n").headD data
    let rendered := if message.severity == .warning && first.startsWith "Variable name " &&
        first.endsWith " is not explicitly referenced." then (raw.splitOn "\n").headD raw else raw
    let text := String.join ((rendered.splitOn "\n").map fun line => s!"BODY_INTERFACE_REUSE_DIAGNOSTIC:{line}\n")
    total := total + text.utf8ByteSize
    require (total ≤ 32768) "diagnostic budget exceeded"
    let output ← IO.getStdout
    output.putStr text
    output.flush
  let mut seen : List String := []
  let mut valid := !messages.hasErrors && markers == 1 && !axioms.isEmpty
  for line in axioms do
    match line.splitOn ":" with
    | ["WITCHY_SCALAR_AXIOMS", name, bits] =>
      let goalPrefix := scope ++ ".goal_"
      let suffix := String.ofList (name.toList.drop goalPrefix.length)
      valid := valid && name.startsWith goalPrefix && !suffix.isEmpty &&
        suffix.toList.all Char.isDigit && !seen.contains name && bits.length == 3 &&
        bits.toList.all (fun c => c == '0' || c == '1')
      if install then valid := valid && name == scope ++ ".goal_0"
      seen := name :: seen
    | _ => valid := false
  return valid && (!install || axioms.length == 1)
-- Recognize only the exact compiler audit. Other diagnostic modules execute
-- their own original Lean text. The propositions and proof terms always go
-- through elaboration and the kernel in the current query environment.
private def auditTemplate : String := include_str "proof_audit.lean.in"
private def auditPlan (body scope marker : String) : Option (String × Array (Name × Name)) := do
  let n ← (marker.splitOn ":").getLast? >>= String.toNat?
  guard (0 < n && n ≤ 1024)
  let ns := Name.str .anonymous scope
  let pairs := (Array.range n).map fun i => (ns.str s!"goal_{i}", ns.str s!"expected_{i}")
  let entries := String.intercalate ", " (pairs.toList.map fun (g, e) => s!"(`{g}, `{e})")
  let suffix := (((auditTemplate.replace "`WitchyScalar" ("`" ++ scope)).replace "__GOALS__" entries).replace "__MARKER__" marker)
  guard (body.endsWith suffix)
  -- Check the *final command's* options inside the actual module, just as the
  -- original audit does. CoreM below receives the completed kernel environment.
  let optionGuard := "open Lean\n#eval ((do\n  if debug.skipKernelTC.get (← getOptions) then throwError \"kernel type checking disabled\"\n) : Lean.CoreM Unit)\n"
  return ((body.dropEnd suffix.length).toString ++ optionGuard, pairs)

-- Specialize the same audit source used by the standalone Lean entry point.
-- Only the surrounding #eval and the three compiler-filled arguments differ.
elab "compile_witchy_audit" : command => do
  let opening := "open Lean\n#eval ((do\n"
  let suffix := ") : Lean.CoreM Unit)\n"
  let inventory := "  let selected : Array (Name × Name) := #[__GOALS__]\n"
  let template : String := include_str "proof_audit.lean.in"
  let some body := template.dropPrefix? opening | throwError "unexpected audit header"
  let some body := body.dropSuffix? suffix | throwError "unexpected audit trailer"
  let mut body := body.toString
  for (fromText, toText) in [(inventory, ""), ("(`WitchyScalar)", "scope"),
                              ("m!\"__MARKER__\"", "marker")] do
    unless (body.splitOn fromText).length == 2 do throwError "unexpected audit argument {fromText}"
    body := body.replace fromText toText
  let text := "private def compiledAudit (scope : Name) (selected : Array (Name × Name)) " ++
              "(marker : String) : CoreM Unit := do\n" ++ body
  let stx ← ofExcept <| Parser.runParserCategory (← getEnv) `command text
  Elab.Command.elabCommand stx

compile_witchy_audit

private def installInventory (env : Environment) (scope : String) : Bool := Id.run do
  let ns := Name.str .anonymous scope
  let expected := Name.str ns "expected_0"
  let goal := Name.str ns "goal_0"
  let some (.defnInfo d) := env.find? expected | return false
  let some (.thmInfo t) := env.find? goal | return false
  if d.type != mkSort .zero || t.type != mkConst expected then return false
  for (name, info) in env.constants.toList do
    if ns.isPrefixOf name then
      match info with
      | .axiomInfo _ => return false
      | .thmInfo _ => if name != goal then return false
      | _ => pure ()
  return true

private unsafe def fixedImports (root : String) : IO Environment := do
  searchPathRef.set [System.FilePath.mk (root ++ "/lib/lean")]
  enableInitializersExecution
  let (parsedHeader, _, headerMessages) ← Parser.parseHeader
    (Parser.mkInputContext header "resident_fixed_header.lean")
  require (!headerMessages.hasErrors) "fixed header parse failed"
  let imported ← importModules (Elab.HeaderSyntax.imports parsedHeader) options
    (trustLevel := 0) (loadExts := true)
  require (imported.header.trustLevel == 0) "nonzero import trust"
  return imported

-- Installation accepts no query or source file. Only the fixed import header
-- is loaded in this process; no current program proof exists in the image.
private structure ImportedHeader where
  env : Environment
  initModules : Array Nat

@[extern "witchy_use_fixed_header"]
opaque useFixedHeader : BaseIO Bool

-- ModuleData has no region handles. Store the fixed SDK import graph in one
-- ordinary (closure-free) compacted region. Rebuild Lean's normal import
-- tables over those exact objects. The second file holds only the derived
-- environment; all pointers into module data relocate against its real region.
private structure PackedModule where
  imp : EffectiveImport
  parts : Array ModuleData
  irParts : Array ModuleData
  needsIRTrans : Bool

private def pack (s : ImportState) : Except String (Array PackedModule) :=
  s.moduleNames.mapM fun name => do
    let some m := s.moduleNameMap[name]? | throw "missing transitive module"
    return ⟨m.toEffectiveImport, m.parts.map Prod.fst, m.irParts.map Prod.fst, m.needsIRTrans⟩

private def unpack (modules : Array PackedModule) (region : CompactedRegion) : ImportState := Id.run do
  let mut table := {}
  for m in modules do
    let attach := fun (d : ModuleData) => (d, region)
    table := table.insert m.imp.module (ImportedModule.mk m.imp (m.parts.map attach)
      (m.irParts.map attach) m.needsIRTrans)
  return ⟨table, modules.map (·.imp.module)⟩

@[export witchy_save_header]
unsafe def saveHeader (root dest : String) : IO UInt32 := do
  setRuntimeMemory (2048 * 1024 * 1024)
  searchPathRef.set [System.FilePath.mk root / "lib/lean"]
  enableInitializersExecution
  let (parsed, _, messages) ← Parser.parseHeader (Parser.mkInputContext header "<fixed imports>")
  require (!messages.hasErrors) "invalid fixed header"
  let requested := Elab.HeaderSyntax.imports parsed
  let packedFile := System.FilePath.mk dest |>.withExtension "modules"
  let (_, s) ← withImporting <| importModulesCore (globalLevel := .private) requested {} |>.run
  let data ← checked (pack s)
  let writer ← CompactedRegion.save packedFile `WitchyModuleData data #[] none
  Runtime.forget writer
  let (modules, region) ← CompactedRegion.read (α := Array PackedModule) packedFile #[]
  enableInitializersExecution
  let env ← withImporting <| finalizeImport (unpack modules region) requested options 0
    (leakEnv := true) (loadExts := true)
  require (env.header.trustLevel == 0) "nonzero fixed import trust"
  -- Own the one mapping for process lifetime. Each part in the reconstructed
  -- import table borrows it. Never call Environment.freeRegions on this env.
  let h : ImportedHeader := { env, initModules := getRegularInitAttrModIdxs env }
  let writer ← CompactedRegion.save dest `WitchyFixedHeader h #[region] none (allowClosures := true)
  Runtime.forget writer
  return 0

private def readModule (input : IO.FS.Stream) (r : Request) (scope : String) : IO String := do
  require (r.bytes > 0 && r.bytes ≤ 65536 && !r.marker.isEmpty &&
    r.marker.utf8ByteSize ≤ 512 && r.source != zero && r.module != zero) "payload bounds refused"
  let bytes ← readExact input r.bytes
  let modulePrefix := (header ++ prelude scope).toUTF8
  require (bytes.size ≥ modulePrefix.size && bytes.extract 0 modulePrefix.size == modulePrefix) "fixed module boundary mismatch"
  let some body := String.fromUTF8? (bytes.extract header.utf8ByteSize bytes.size)
    | throw (IO.userError "non-UTF8 module")
  return body

private def elaborate (base : Environment) (nonce : String) (seq : Nat)
    (scope marker body : String) : IO (Environment × MessageLog) := do
  let mainModule := Name.str .anonymous s!"Resident_{nonce}_{seq}"
  let plan := auditPlan body scope marker
  let inputBody := (plan.map Prod.fst).getD body
  let (env, messages) ← Elab.process inputBody (base.setMainModule mainModule) options
    (some s!"resident_{nonce}_{seq}.lean")
  let mut messages := messages
  if let some (_, pairs) := plan then
    if !messages.hasErrors then
      let ctx : Core.Context := { fileName := "<witchy-audit>", fileMap := default, options := options }
      let (_, state) ← (compiledAudit (.str .anonymous scope) pairs marker).run ctx { env := env } |>.toIO (fun _ => IO.userError "kernel inventory audit refused")
      messages := messages ++ state.messages
  return (env, messages)

-- The task manager is bounded; process collects diagnostics from all command snapshots.
@[export witchy_resident_main]
unsafe def residentMain (root binding nonce : String) : IO UInt32 := do
  setRuntimeMemory (2048 * 1024 * 1024)
  require (Lean.versionString == "4.36.0" && hex binding 64 && hex nonce 32) "entry identity refused"
  let input ← IO.getStdin
  deadline 10
  let load ← request input binding nonce 0
  require (load.op == "LOAD" && emptyPayload load) "LOAD required"
  let file := System.FilePath.mk root / "fixed-header.snap"
  let imported ← if ← useFixedHeader then do
    searchPathRef.set [System.FilePath.mk (root ++ "/lib/lean")]
    enableInitializersExecution
    let (_, modules) ← CompactedRegion.read (α := Array PackedModule) (file.withExtension "modules") #[]
    let (data, _region) ← CompactedRegion.read (α := ImportedHeader) file #[modules]
    require (data.env.header.trustLevel == 0) "nonzero snapshot import trust"
    runInitAttrsForModules data.env data.initModules options
    pure data.env
  else fixedImports root
  reply binding nonce load "loaded-untrusted"
  deadline 0
  let mut installed : Option (String × Environment) := none
  let mut queued : Array (Request × String × Task (Except IO.Error (Environment × MessageLog))) := #[]
  let operationLimit := 16
  for seq in [1:operationLimit + 2] do
    let mut r ← request input binding nonce seq (some (queued.size + 1))
    while r.op == "QUEUE" do
      deadline 10
      require ((← useFixedHeader) && seq == 1 && queued.size < 5 &&
        r.context == load.context && r.body == zero && installed.isNone) "queued check state refused"
      let body ← readModule input r "WitchyScalar"
      let ticket := r
      let start ← IO.monoMsNow
      let task ← IO.asTask do
        let result ← elaborate imported nonce ticket.seq "WitchyScalar" ticket.marker body
        require ((← IO.monoMsNow) - start ≤ 10000) "queued check deadline"
        return result
      queued := queued.push (ticket, body, task)
      reply binding nonce r "queued"
      -- An unresponsive client must not leave a speculative task running.
      deadline 10
      r ← request input binding nonce seq (some (queued.size + 1))

    require (r.context == load.context) "context mismatch"
    if r.op == "STOP" then
      require (emptyPayload r && seq > queued.size) "STOP payload or outstanding checks refused"
      reply binding nonce r "closed"
      return 0
    require (seq ≤ operationLimit && (r.op == "INSTALL_BODY" || r.op == "CHECK")) "operation/STOP reservation refused"
    deadline 10
    let isInstall := r.op == "INSTALL_BODY"
    let mut base := imported
    if isInstall then
      require (installed.isNone && r.body != zero) "INSTALL slot/key refused"
    else if r.body != zero then
      let some (key, library) := installed | throw (IO.userError "body not installed")
      require (key == r.body) "selected body mismatch"
      base := library
    let scope := if isInstall then "WitchyBody_" ++ r.body else "WitchyScalar"
    let body ← readModule input r scope
    reply binding nonce r "begin"
    let (env, messages) ← if let some (q, original, task) := queued[seq - 1]? then do
      require ({q with op := "CHECK"} == r && original == body) "queued check identity mismatch"
      IO.ofExcept task.get
    else elaborate base nonce seq scope r.marker body
    let okay ← auditMessages messages scope r.marker isInstall
    let okay := okay && env.header.trustLevel == 0 && (!isInstall || installInventory env scope)
    reply binding nonce r (if okay then "ok" else "error")
    if seq == queued.size then queued := #[]
    deadline (if queued.isEmpty then 0 else 10)
    if !okay then return 1 -- sticky terminal failure; no later request/fallback
    if isInstall then installed := some (r.body, env)
    -- CHECK env deliberately discarded. ZERO always used imported, not library.
  return 1
