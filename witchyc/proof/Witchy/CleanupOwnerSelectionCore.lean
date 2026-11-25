import Witchy.CleanupOwnerExpansionReady
import Witchy.CoreHeapDisposal
import Witchy.CleanupWorkStep

namespace Witchy.Core

/-- The runner's actual last-initialized selection derives the source cell for
    a consuming Core extraction. No successful cleanup result is a premise.
    The compiler must still represent the pending worklist and selected index. -/
theorem cleanup_selected_child_core (s : StateOf Value) (root dst src index : Nat) (offset : UInt64)
    (block : Storage.Block Value) (child : Value) (tail : List Value)
    (owner : s.registers[src]? = some (some (.owner root)))
    (empty : s.registers[dst]? = some none) (position : number s index = .ok offset)
    (stored : Memory.block? s.memory root = some block)
    (selected : block.lastInitialized = some (offset.toNat, child))
    (free : (Memory.active s.memory root).isEmpty = true)
    (eligible : TransferUnborrowed s.memory child)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory) :
    ∃ next, step s (.takeMove dst src index) = .ok next ∧
      Storage.cleanupWorkStep s.memory (.owner root :: tail) =
        (next.memory, .ok (), child :: .owner root :: tail) ∧
      next.registers = s.registers.set dst (some child) ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧ next.authority = s.authority := by
  obtain ⟨next, performed, expanded, registers, inventory, graph, caps, authority⟩ :=
    cleanup_owner_expansion_core s root dst src index offset block child tail owner empty position stored free
      (block.last_initialized_present offset.toNat child selected) eligible owned structural capabilities
  exact ⟨next, performed, by simpa only [Storage.cleanupWorkStep, stored, selected] using expanded,
    registers, inventory, graph, caps, authority⟩

/-- Exhausting the runner's selector derives strict owner-disposal readiness.
    The same actual Core release consumes the registered owner and agrees with
    the runner's terminal worklist action. -/
theorem cleanup_empty_owner_core (s : StateOf Value) (root source : Nat)
    (block : Storage.Block Value) (tail : List Value)
    (owner : s.registers[source]? = some (some (.owner root)))
    (stored : Memory.block? s.memory root = some block)
    (selected : block.lastInitialized = none)
    (free : (Memory.active s.memory root).isEmpty = true)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory) :
    ∃ next, step s (.release source) = .ok next ∧
      Storage.cleanupWorkStep s.memory (.owner root :: tail) = (next.memory, .ok (), tail) ∧
      next.registers = s.registers.set source none ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧ next.authority = s.authority := by
  have ready : Storage.HeapDisposalReady s.memory (.owner root) :=
    ⟨block, stored, free, block.last_initialized_none.mp selected⟩
  obtain ⟨next, performed, memory, registers, _vacant, inventory, graph, caps, authority, _runs⟩ :=
    heap_disposal_contract s source (.owner root) owner ready owned structural capabilities
  have accepted := Storage.heap_disposal_ready s.memory (.owner root) ready
  have disposed : Storage.disposeHeapValue s.memory (.owner root) = (next.memory, .ok ()) :=
    Prod.ext memory.symm accepted
  exact ⟨next, performed, by simp only [Storage.cleanupWorkStep, stored, selected,
    Storage.finishCleanupValue, disposed], registers, inventory, graph, caps, authority⟩

/-- The selected borrowed child is refused by both layers. Core reports its
    owner-transfer fault while cleanup reports the memory borrow refusal; neither
    publishes the intermediate take or consumes the original owner cell. -/
theorem cleanup_selected_borrowed_child_core (s : StateOf Value) (root child dst src index : Nat)
    (offset : UInt64) (block : Storage.Block Value) (tail : List Value)
    (owner : s.registers[src]? = some (some (.owner root)))
    (empty : s.registers[dst]? = some none) (position : number s index = .ok offset)
    (stored : Memory.block? s.memory root = some block)
    (selected : block.lastInitialized = some (offset.toNat, .owner child))
    (free : (Memory.active s.memory root).isEmpty = true)
    (borrowed : (Memory.active s.memory child).isEmpty = false) :
    Storage.cleanupWorkStep s.memory (.owner root :: tail) =
      (s.memory, .error .borrowed, .owner root :: tail) ∧
      step s (.takeMove dst src index) = .error .liveOwner := by
  have initialized := block.last_initialized_present offset.toNat (.owner child) selected
  have refused := Storage.expand_cleanup_owner_borrowed_child s.memory root offset.toNat child block tail
    stored free initialized borrowed
  refine ⟨by simpa only [Storage.cleanupWorkStep, stored, selected] using refused, ?_⟩
  have accessed : access s src = .ok (.owner root) := by simp [access, get, owner, Bind.bind, Pure.pure]
  have permission : Memory.resolve s.memory (.owner root) true = .ok root := by simp [Memory.resolve, free]
  have blocked : (s.memory.loans.filter (fun loan => loan.target == child)).isEmpty = false := borrowed
  simp [step, StorageCell.ownedCells, vacant, empty, accessed, position, Memory.take, permission, stored,
    initialized, StorageCell.decodeValue, Memory.active, blocked, Bind.bind, fromMemory]

end Witchy.Core
