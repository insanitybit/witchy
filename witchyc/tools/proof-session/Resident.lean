import Lean

/- Source-only native diagnostic. LOAD replies are observations, not a
   verified immutable-snapshot capability or production admission. -/
open Lean

@[extern "lean_internal_set_max_memory"]
opaque setRuntimeMemory (bytes : USize) : BaseIO Unit
@[extern "witchy_resident_deadline"]
opaque deadline (seconds : UInt32) : BaseIO Unit

private def imports : Array Name := #[
  `Lean.Elab.BuiltinCommand, `Lean.Elab.BuiltinTerm, `Lean.Elab.Declaration,
  `Lean.Elab.ElabRules, `Lean.Elab.BuiltinEvalCommand, `Lean.Elab.StructInst,
  `Lean.Elab.Tactic.Omega, `Lean.Elab.Tactic.Match, `Lean.Elab.Tactic.Simp,
  `Lean.Elab.Tactic.Unfold, `Lean.Util.CollectAxioms, `Init.Omega]
private def header : String := String.join (imports.toList.map fun n => s!"import {n}\n")
private def options : Options := Options.empty |>.setBool `debug.skipKernelTC false
  |>.set `max_memory (2048 : Nat) |>.setBool `Elab.async false

private def checked (result : Except String α) : IO α :=
  match result with
  | .ok value => pure value
  | .error message => throw (IO.userError message)

private def readExact (input : IO.FS.Stream) (size : Nat) : IO ByteArray := do
  let mut bytes := ByteArray.empty
  while bytes.size < size do
    let chunk ← input.read ((min 4096 (size - bytes.size)).toUSize)
    if chunk.isEmpty then throw (IO.userError "truncated frame")
    bytes := bytes ++ chunk
  return bytes

private def readFrame (input : IO.FS.Stream) : IO Json := do
  let mut bytes := ByteArray.empty
  for _ in [:4096] do
    let b ← readExact input 1
    if b[0]! == 10 then
      let some text := String.fromUTF8? bytes | throw (IO.userError "non-UTF8 frame")
      return ← checked (Json.parse text)
    bytes := bytes ++ b
  throw (IO.userError "frame exceeds 4096 bytes")

private def reply (binding nonce : String) (seq : Nat) (phase status : String) (claim : String := "") : IO Unit := do
  let json := Json.mkObj [("binding", toJson binding), ("nonce", toJson nonce),
    ("seq", toJson seq), ("phase", toJson phase), ("status", toJson status),
    ("mode", toJson "UNSEALED_DIAGNOSTIC"), ("claim", toJson claim)]
  let output ← IO.getStdout
  let text := s!"WITCHY_RESIDENT_V1:{json.compress}\n"
  if text.utf8ByteSize > 16384 then throw (IO.userError "reply frame bound refused")
  output.putStr text
  output.flush

private def expect (input : IO.FS.Stream) (binding nonce op : String) (seq : Nat) : IO Json := do
  let request ← readFrame input
  let actualOp ← checked (request.getObjValAs? String "op")
  if actualOp == "SEAL" then
    reply binding nonce seq "SEAL" "Unsupported"
    throw (IO.userError "UNSEALED_DIAGNOSTIC: SEAL unsupported")
  unless (← checked (request.getObjValAs? String "binding")) == binding &&
      (← checked (request.getObjValAs? String "nonce")) == nonce &&
      (actualOp == op || (op == "CHECK_DIAG" && actualOp == "STOP")) &&
      (← checked (request.getObjValAs? Nat "seq")) == seq do
    throw (IO.userError "binding/nonce/state/sequence mismatch")
  return request

-- Native helper main establishes the task manager; this entry sets the actual
-- runtime cap before LOAD, independently of command-line shell parsing.
@[export witchy_resident_main]
unsafe def residentMain (root binding nonce : String) : IO UInt32 := do
  setRuntimeMemory (2048 * 1024 * 1024)
  unless Lean.versionString == "4.36.0" && binding.utf8ByteSize ≤ 3072 &&
      nonce.utf8ByteSize == 32 && nonce.toList.all (fun c => c.isAlphanum) do
    throw (IO.userError "version/binding/nonce refused")
  let input ← IO.getStdin
  let _ ← expect input binding nonce "LOAD" 0
  searchPathRef.set [System.FilePath.mk (root ++ "/lib/lean")]
  enableInitializersExecution
  -- Match the ordinary legacy header: implicit Init AND meta Init plus the
  -- explicit imports' export flags. Bare {module := n} silently omits these.
  let (parsedHeader, _, headerMessages) ← Parser.parseHeader
    (Parser.mkInputContext header "resident_fixed_header.lean")
  if headerMessages.hasErrors then throw (IO.userError "fixed header parse failed")
  let base ← importModules (Elab.HeaderSyntax.imports parsedHeader) options
    (trustLevel := 0) (loadExts := true)
  unless base.header.trustLevel == 0 do throw (IO.userError "nonzero import trust")
  -- Retaining regions does not make mutable mmap backings immutable. Root must
  -- perform full SDK A/B around EVERY request. No production reuse is exposed.
  reply binding nonce 0 "LOAD" "loaded-untrusted"
  for seq in [1:9] do
    let request ← expect input binding nonce "CHECK_DIAG" seq
    if (← checked (request.getObjValAs? String "op")) == "STOP" then
      reply binding nonce seq "STOP" "closed"
      return 0
    deadline 10
    let count ← checked (request.getObjValAs? Nat "bytes")
    let marker ← checked (request.getObjValAs? String "marker")
    let claim ← checked (request.getObjValAs? String "claim")
    unless count > 0 && count ≤ 65536 && !marker.isEmpty && marker.utf8ByteSize ≤ 512 &&
        !claim.isEmpty && claim.utf8ByteSize ≤ 2048 do
      throw (IO.userError "source/marker bound refused")
    let bytes ← readExact input count
    let importBytes := header.toUTF8
    unless bytes.size ≥ importBytes.size && bytes.extract 0 importBytes.size == importBytes do
      throw (IO.userError "fixed import header mismatch")
    let some body := String.fromUTF8? (bytes.extract importBytes.size bytes.size)
      | throw (IO.userError "source is not UTF8")
    reply binding nonce seq "CHECK_DIAG" "begin" claim
    let mainModule := Name.str .anonymous s!"Resident_{nonce}_{seq}"
    let (env, messages) ← Elab.process body (base.setMainModule mainModule) options
      (some s!"resident_{nonce}_{seq}.lean")
    let mut outputBytes := 0
    let mut markers := 0
    for message in messages.toList do
      let data ← message.data.toString
      markers := markers + ((data.splitOn "\n").filter (· == marker)).length
      let raw ← message.toString
      let text := String.join ((raw.splitOn "\n").map fun line => s!"UNSEALED_DIAGNOSTIC:{line}\n")
      outputBytes := outputBytes + text.utf8ByteSize + 1
      if outputBytes > 32768 then throw (IO.userError "diagnostics exceed output budget")
      let output ← IO.getStdout
      output.putStr text
      output.flush
    let okay := !messages.hasErrors && env.header.trustLevel == 0 && markers == 1
    reply binding nonce seq "CHECK_DIAG" (if okay then "ok" else "error") claim
    deadline 0
    if !okay then return 1 -- poison: no later request or fallback can hide failure
  return 0
