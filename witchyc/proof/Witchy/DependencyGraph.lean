import Witchy.DependencyHeader
import Witchy.DependencyGraphRead

/-!
The dependency-graph query as a small program, linked by Lake as
`witchy-import-graph`. It imports only Lean's header parser and module reader, not
the elaborator, and it stops before any environment is built.

Lean 4.35's own import loader (`Lean.importModulesCore`) is public, but the state
it leaves (`Lean.ImportState`) keeps its module list and module map in private
fields that only `Lean.finalizeImport` reads, and that is the step that builds the
whole environment. This program therefore does not run the loader. It walks the
import graph itself with the public pieces the loader is made of (`Lean.findOLean`,
`Lean.CompactedRegion.read`, `Lean.OLeanLevel.adjustFileName`,
`Lean.ModuleData.imports`), following the loader's rule for a root outside the
module system, which is what the generated files are: every transitive import is
loaded whatever its `public`/`meta`/`all` flags, and the imports followed from a
module are those of the data the loader would use for it, the private part
(`.olean.private`, read after `.olean` and `.olean.server`) when the module takes
part in the module system and the single `.olean` otherwise.

It is a second reading of that rule, not the loader's own state. The compiler
treats it accordingly: it runs this program only while every dependency pass
frames the program's bytes, the frontend helper answers whenever this program is
absent or does not answer, and both byte barriers and all proof checking are
unchanged. It holds no graph: every run reads the current inputs, search path and
object files.
-/

namespace Witchy.DependencyClosure

open Lean

/-- The imports Lean's loader follows from `name` for a root outside the module
system. The regions read are deliberately never freed: the names returned live in
them and the program ends with its answer. -/
private def followedImports (name : Name) : IO (Array Import) := do
  let file ← findOLean name
  unless (← file.pathExists) do
    throw <| IO.userError s!"object file '{file}' of module {name} does not exist"
  let main ← unsafe CompactedRegion.read (α := ModuleData) file #[]
  if !main.1.isModule then
    return main.1.imports
  let server ← unsafe CompactedRegion.read (α := ModuleData) (OLeanLevel.server.adjustFileName file) #[main.2]
  let priv ← unsafe CompactedRegion.read (α := ModuleData) (OLeanLevel.private.adjustFileName file) #[main.2, server.2]
  return priv.1.imports

/-- `helperImports` is the fixed import header the compiler adds after the generated
headers, exactly as it frames it. -/
def graph (helperImports : String) : IO Unit := do
  let direct ← expectedImports helperImports
  initSearchPath (← checkerSysroot)
  checkSearchPath
  -- Search path is initialized/checked before any tasks, then only read.
  -- Each reader owns its main/server/private chain; no region is ever freed.
  let current ← GraphRead.collect followedImports direct
  let modules ← GraphRead.ordered current direct
  printGraph modules

end Witchy.DependencyClosure

/-- One argument: the fixed helper import header. Any failure is a non-zero exit. -/
def main (arguments : List String) : IO UInt32 := do
  match arguments with
  | [helperImports] =>
    try
      Witchy.DependencyClosure.graph helperImports
      return 0
    catch error =>
      IO.eprintln s!"{error}"
      return 1
  | _ =>
    IO.eprintln "expected exactly the helper import header"
    return 2
