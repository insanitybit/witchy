import Lean.Elab.Import

/-!
The checks every form of the compiler's dependency-graph query makes before it
names a graph, kept apart from the elaborator so that the program form
(`Witchy.DependencyGraph`) links only Lean's header parser and module reader.

Nothing here holds a graph or a remembered answer: each call reads the CURRENT
exact generated inputs, the current environment variables and the current search
path.
-/

namespace Witchy.DependencyClosure

open Lean Elab

/-- The direct imports of the helper for the current exact generated inputs:
`helperImports` (the import header the compiler adds after the generated headers,
exactly as written) followed by the imports of the Code and Admission headers as
Lean's own header parser reads them, implicit `Init` included. A `module` or
`prelude` header is refused. -/
def expectedImports (helperImports : String) : IO (Array Import) := do
  let (extras, _, extraMessages) ← parseImports helperImports
  if extraMessages.hasErrors then throw <| IO.userError "invalid helper import policy"
  let mut expected := extras
  for environmentName in ["WITCHY_DEPENDENCY_CODE", "WITCHY_DEPENDENCY_ADMISSION"] do
    let some file ← IO.getEnv environmentName
      | throw <| IO.userError "missing exact dependency input path"
    let input ← IO.FS.readFile (System.FilePath.mk file)
    let (imports, _, messages) ← parseImports input (some file)
    if messages.hasErrors then
      throw <| IO.userError "invalid generated module import header"
    let (header, _, headerMessages) ← Parser.parseHeader (Parser.mkInputContext input file)
    if headerMessages.hasErrors then throw <| IO.userError "invalid generated module header"
    match header with
    | `(Parser.Module.header| $[module%$moduleTk]? $[prelude%$preludeTk]? $_importsStx*) =>
      if moduleTk.isSome || preludeTk.isSome then
        throw <| IO.userError "unsupported module/prelude header; fresh verification required"
    | _ => throw <| IO.userError "unsupported module header"
    for imp in imports do
      if imp.module != `ExecutableProgram then expected := expected.push imp
  return expected

/-- The checker sysroot the compiler named for this run. -/
def checkerSysroot : IO System.FilePath := do
  let some sysroot ← IO.getEnv "WITCHY_DEPENDENCY_SYSROOT"
    | throw <| IO.userError "missing checker sysroot"
  return System.FilePath.mk sysroot

/-- The search path in force must be the checker's: the built-in path of the
named sysroot extended by the current `LEAN_PATH`, and nothing else. -/
def checkSearchPath : IO Unit := do
  let expectedSearch ← getBuiltinSearchPath (← checkerSysroot) >>= addSearchPathFromEnv
  let actualSearch ← searchPathRef.get
  unless actualSearch == expectedSearch do
    throw <| IO.userError "actual helper search path differs from checker policy"

/-- The answer protocol: every module with the object file the current search path
resolves it to, then the count. -/
def printGraph (modules : Array Name) : IO Unit := do
  IO.println "WITCHY_CLOSURE_V1"
  for name in modules do
    let path ← Lean.findOLean name
    IO.println s!"WITCHY_OLEAN {name}\t{path}"
  IO.println s!"WITCHY_CLOSURE_END {modules.size}"

end Witchy.DependencyClosure
