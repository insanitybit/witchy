import Lean.Environment

open Lean

-- These are UNTRUSTED ModuleData fixtures, never declarations in any env.
-- They deliberately cannot be accepted as current theorem proofs.
def main (args : List String) : IO Unit := do
  let [directory] := args | throw (IO.userError "expected only owned fixture directory")
  let directory : System.FilePath := directory
  let write (file : String) (name : Name) (value : Expr) : IO Unit := do
    let info : ConstantInfo := .thmInfo { name, levelParams := [], type := mkConst ``True, value }
    let data : ModuleData := {
      isModule := false
      imports := #[]
      constNames := #[name]
      constants := #[info]
      extraConstNames := #[]
      entries := #[] }
    saveModuleData (directory / file) name data
  write "cycle.olean" `RawReaderCycle.proof (mkConst `RawReaderCycle.proof)
  write "mvar.olean" `RawReaderMVar.proof (mkMVar { name := `rawUnresolved })
