import Witchy.HeapCapabilities
import Witchy.MemoryBufferInsertion
import Witchy.MemoryCellTransfer

namespace Witchy.Storage

theorem write_from_register_capabilities (authority : Authority.State) (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (access : Memory.Access) (index source : Nat) (value : Core.Value)
    (valid : allCapabilitiesValid authority registers memory) (present : registers[source]? = some (some value))
    (accepted : (Memory.writeEmpty memory access index value).2 = .ok ()) :
    allCapabilitiesValid authority (registers.set source none) (Memory.writeEmpty memory access index value).1 := by
  have valueValid := valid.1 value (List.mem_of_getElem? present)
  let next := (Memory.writeEmpty memory access index value).1
  have complete : Memory.writeEmpty memory access index value = (next, .ok ()) := by rw [← accepted]
  obtain ⟨handle, block, _permission, found, _empty, _buffer, effect⟩ :=
    memory_write_buffer_push memory next access index value complete
  have blockValid := valid.2 block (List.mem_of_getElem? (Memory.block_lookup memory handle block found))
  have cellsValid := cells_caps_set authority block.slots index (some value) blockValid
    (by intro other same; cases same; exact valueValid)
  constructor
  · exact cells_caps_set authority registers source none valid.1 (by intro other impossible; cases impossible)
  · change heapCapabilitiesValid authority next
    rw [effect]
    exact heap_caps_set authority memory handle (some ⟨block.slots.set index (some value)⟩) valid.2
      (by intro other same; cases same; exact cellsValid)

theorem take_to_register_capabilities (authority : Authority.State) (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (access : Memory.Access) (index destination : Nat) (value : Core.Value)
    (valid : allCapabilitiesValid authority registers memory)
    (accepted : (Memory.take memory access index).2 = .ok value) :
    allCapabilitiesValid authority (registers.set destination (some value)) (Memory.take memory access index).1 := by
  let next := (Memory.take memory access index).1
  have complete : Memory.take memory access index = (next, .ok value) := by rw [← accepted]
  obtain ⟨handle, block, _permission, found, initialized, effect⟩ :=
    Memory.take_cell_effect memory next access index value complete
  have blockValid := valid.2 block (List.mem_of_getElem? (Memory.block_lookup memory handle block found))
  have valueValid := blockValid value (List.mem_of_getElem? initialized)
  have cellsValid := cells_caps_set authority block.slots index none blockValid
    (by intro other impossible; cases impossible)
  constructor
  · exact cells_caps_set authority registers destination (some value) valid.1
      (by intro other same; cases same; exact valueValid)
  · change heapCapabilitiesValid authority next
    rw [effect]
    exact heap_caps_set authority memory handle (some ⟨block.slots.set index none⟩) valid.2
      (by intro other same; cases same; exact cellsValid)

/-- Clearing/deleting storage cannot introduce a new capability. -/
theorem release_heap_capabilities (authority : Authority.State) (memory : Memory.StateOf Core.Value)
    (handle : Nat) (valid : heapCapabilitiesValid authority memory) :
    heapCapabilitiesValid authority { memory with blocks := memory.blocks.set handle none } := by
  exact heap_caps_set authority memory handle none valid (by intro block impossible; cases impossible)

end Witchy.Storage
