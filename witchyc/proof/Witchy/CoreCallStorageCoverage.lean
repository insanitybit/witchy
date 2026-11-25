import Witchy.CoreCallProcedure
import Witchy.TypedStorageRuntime

namespace Witchy.Core.Call

abbrev StoragePlan := Nat → Nat → Storage.Element

/-- Every physical storage site has exactly one descriptor at its original
    PC; other instructions have none. Indexing prevents duplicate site keys. -/
def storageRequired : Instruction → Bool
  | .core (.allocate ..) | .core (.tryAllocate ..) | .core (.read ..)
  | .core (.readUnchecked ..) | .core (.take ..) | .core (.takeMove ..)
  | .core (.writeEmpty ..) | .core (.storeMove ..) | .core (.release ..)
  | .core (.packMove ..) | .core (.unpackMove ..) => true
  | _ => false

def storageCoverage (code : Array Instruction) (row : Array (Option Storage.Element)) : Bool :=
  decide (code.size = row.size) && (code.zip row).all (fun (instruction, descriptor) => storageRequired instruction == descriptor.isSome &&
    match descriptor with
    | none => true
    | some element => element.wellFormed &&
      match instruction with
      | .core (.packMove _ _ widths) | .core (.unpackMove _ _ widths) => element.widthsFuel 65 == some widths
      | _ => true)

end Witchy.Core.Call
