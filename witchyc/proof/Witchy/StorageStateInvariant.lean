import Witchy.BorrowRoots
import Witchy.HeapCapabilities
import Witchy.LoanTargets

namespace Witchy.Core

/-- Independent operational storage obligations. This bundle does not assert
    source typing, physical layout, general reference lifetimes or relocation. -/
structure StorageStateInvariant (s : StateOf Value) : Prop where
  inventory : Storage.OwnsHeapResources s.registers s.memory
  structural : Storage.OwnershipWellFounded s.memory
  capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory
  roots : LoanRootsRegistered s.registers s.memory.loans

/-- Root registration and inventory derive live loan targets; this is not an
    additional caller assumption in storage growth contracts. -/
theorem StorageStateInvariant.loan_targets (s : StateOf Value) (known : StorageStateInvariant s) :
    LoanTargetsLive s.memory := by
  intro loan member
  obtain ⟨index, bound, found⟩ := List.mem_iff_getElem.mp (known.roots loan member)
  have positive := present_resource_count_positive s.registers index (.owner loan.target) (.allocation loan.target)
    (List.getElem?_eq_some_iff.mpr ⟨bound, found⟩) rfl
  have inventory := known.inventory (.allocation loan.target)
  have upper := memory_count_at_most_one s.memory (.allocation loan.target)
  omega

end Witchy.Core
