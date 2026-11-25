import Witchy.HeapResources
import Witchy.MemoryBufferInsertion
import Witchy.MemoryCellTransfer

namespace Witchy.Storage

theorem write_heap_inventory (memory : Memory.StateOf Core.Value) (access : Memory.Access)
    (index : Nat) (value : Core.Value) (accepted : (Memory.writeEmpty memory access index value).2 = .ok ())
    (resource : Core.Resource) :
    heapResources (Memory.writeEmpty memory access index value).1 resource =
      heapResources memory resource + Core.weight resource (some value) ∧
    Core.memoryCount (Memory.writeEmpty memory access index value).1 resource = Core.memoryCount memory resource := by
  let next := (Memory.writeEmpty memory access index value).1
  have complete : Memory.writeEmpty memory access index value = (next, .ok ()) := by
    rw [← accepted]
  obtain ⟨handle, block, _permission, found, empty, _buffer, effect⟩ :=
    memory_write_buffer_push memory next access index value complete
  have storage := replace_block_inventory memory handle block ⟨block.slots.set index (some value)⟩ found resource
  dsimp only [Storage.Block.slots_mk] at storage
  have cell := Core.register_set_accounting block.slots index none (some value) resource empty
  have declarations := replace_block_live_count memory handle block ⟨block.slots.set index (some value)⟩ found resource
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at cell
  change heapResources next resource = _ ∧ Core.memoryCount next resource = _
  rw [effect]
  exact ⟨by omega, declarations⟩

theorem take_heap_inventory (memory : Memory.StateOf Core.Value) (access : Memory.Access)
    (index : Nat) (value : Core.Value) (accepted : (Memory.take memory access index).2 = .ok value)
    (resource : Core.Resource) :
    heapResources (Memory.take memory access index).1 resource + Core.weight resource (some value) =
      heapResources memory resource ∧
    Core.memoryCount (Memory.take memory access index).1 resource = Core.memoryCount memory resource := by
  let next := (Memory.take memory access index).1
  have complete : Memory.take memory access index = (next, .ok value) := by
    rw [← accepted]
  obtain ⟨handle, block, _permission, found, initialized, effect⟩ :=
    Memory.take_cell_effect memory next access index value complete
  have storage := replace_block_inventory memory handle block ⟨block.slots.set index none⟩ found resource
  dsimp only [Storage.Block.slots_mk] at storage
  have cell := Core.register_set_accounting block.slots index (some value) none resource initialized
  have declarations := replace_block_live_count memory handle block ⟨block.slots.set index none⟩ found resource
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at cell
  change heapResources next resource + _ = _ ∧ Core.memoryCount next resource = _
  rw [effect]
  exact ⟨by omega, declarations⟩

theorem write_from_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (access : Memory.Access) (index source : Nat) (value : Core.Value)
    (owned : OwnsHeapResources registers memory) (present : registers[source]? = some (some value))
    (accepted : (Memory.writeEmpty memory access index value).2 = .ok ()) :
    OwnsHeapResources (registers.set source none) (Memory.writeEmpty memory access index value).1 := by
  intro resource
  have prior := owned resource
  obtain ⟨stored, live⟩ := write_heap_inventory memory access index value accepted resource
  have moved := Core.register_set_accounting registers source (some value) none resource present
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at moved
  rw [live]
  omega

theorem take_to_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (access : Memory.Access) (index destination : Nat) (value : Core.Value)
    (owned : OwnsHeapResources registers memory) (vacant : registers[destination]? = some none)
    (accepted : (Memory.take memory access index).2 = .ok value) :
    OwnsHeapResources (registers.set destination (some value)) (Memory.take memory access index).1 := by
  intro resource
  have prior := owned resource
  obtain ⟨stored, live⟩ := take_heap_inventory memory access index value accepted resource
  have moved := Core.register_set_accounting registers destination none (some value) resource vacant
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at moved
  rw [live]
  omega

end Witchy.Storage
