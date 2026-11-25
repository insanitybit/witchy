import Lean.Elab.Import
import Lean.Elab.BuiltinCommand
import Lean.Meta.Match.MatchEqs
import Witchy.DependencyHeader

/-!
The dependency-discovery command of the compiler's closure helper, compiled once
with the foundation instead of being elaborated in every helper run.

It decides nothing by itself and holds no graph: every run reads the CURRENT
environment of the helper file that calls it, the current exact generated inputs
and the current search path, and prints the module graph Lean resolved for that
run. The compiler binds this module's own source and compiled files like any
other module of that graph. The checks it shares with the program form of the
query live in `Witchy.DependencyHeader`.
-/

namespace Witchy.DependencyClosure

open Lean Elab Command

/-- `helperImports` is the import header the calling helper file itself adds after
the generated headers, exactly as written there. -/
def report (helperImports : String) : CommandElabM Unit := do
  /- The frontend has already imported the exact generated header. Read its
     current environment; do not import a second copy of that environment. -/
  let env ← getEnv
  let expected ← liftIO <| expectedImports helperImports
  -- BEq Import compares module, isMeta, importAll AND isExported. In particular,
  -- the implicit normal/meta Init imports must both remain represented.
  let actual := env.header.imports
  unless expected.all (fun imp => actual.contains imp) && actual.all (fun imp => expected.contains imp) do
    throwError "actual helper imports differ from original imports and helper policy"
  liftIO checkSearchPath
  liftIO <| printGraph env.allImportedModuleNames

end Witchy.DependencyClosure
