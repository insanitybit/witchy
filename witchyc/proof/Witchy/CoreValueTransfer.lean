import Witchy.CoreStep

namespace Witchy.Core

/-- Moving an allocation owner is forbidden while that allocation is borrowed.
    Location-sensitive relocation and reference lifetimes are separate duties. -/
def TransferUnborrowed (memory : Memory.StateOf Value) : Value → Prop
  | .owner handle => (Memory.active memory handle).isEmpty = true
  | _ => True

def TransferNotSelf (target : Memory.Handle) : Value → Prop
  | .owner handle => handle ≠ target
  | _ => True

/-- The consuming instruction implements the actual checked initialization and
    clears exactly the source register after success. -/
theorem store_move_effect (s : StateOf Value) (dst index source : Nat) (offset : UInt64)
    (permission : Memory.Access) (target : Memory.Handle) (value : Value)
    (present : s.registers[source]? = some (some value))
    (accessed : access s dst = .ok permission) (position : number s index = .ok offset)
    (resolved : Memory.resolve s.memory permission true = .ok target)
    (unborrowed : TransferUnborrowed s.memory value) (different : TransferNotSelf target value)
    (accepted : (Memory.writeEmpty s.memory permission offset.toNat value).2 = .ok ()) :
    step s (.storeMove dst index source) = .ok
      { s with
        memory := (Memory.writeEmpty s.memory permission offset.toNat value).1
        registers := s.registers.set source none } := by
  have sourceGet : get s source = .ok value := by simp [get, present]
  cases value <;>
    simp_all [step, StorageCell.ownedCells, StorageCell.encodeValue, TransferUnborrowed,
      TransferNotSelf, Bind.bind, Pure.pure, fromMemory]

/-- Extraction uses a vacant destination and transfers the actual payload,
    including owners/references/capabilities, without numeric conversion. -/
theorem take_move_effect (s : StateOf Value) (dst src index : Nat) (offset : UInt64)
    (permission : Memory.Access) (value : Value)
    (empty : s.registers[dst]? = some none)
    (accessed : access s src = .ok permission) (position : number s index = .ok offset)
    (accepted : (Memory.take s.memory permission offset.toNat).2 = .ok value)
    (unborrowed : TransferUnborrowed (Memory.take s.memory permission offset.toNat).1 value) :
    step s (.takeMove dst src index) = .ok
      { s with
        memory := (Memory.take s.memory permission offset.toNat).1
        registers := s.registers.set dst (some value) } := by
  cases value <;>
    simp_all [step, StorageCell.ownedCells, StorageCell.decodeValue, TransferUnborrowed,
      Bind.bind, Pure.pure, fromMemory, put, vacant]

end Witchy.Core
