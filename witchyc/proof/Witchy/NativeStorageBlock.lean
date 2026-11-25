import Witchy.Core

namespace Witchy.Storage.Native

/-- Explicit native-only block ABI. Pure definitions retain the logical model;
    these externs are never used by the interpreter's ordinary Memory API. -/
@[extern "witchy_block_slots"]
def slots (block : @& Block Core.Value) : List (Option Core.Value) := Block.slots block

@[extern "witchy_block_lookup"]
def lookup (block : @& Block Core.Value) (index : @& Nat) : Option (Option Core.Value) := Block.lookup block index

@[extern "witchy_block_set"]
def replace (block : Block Core.Value) (index : @& Nat) (cell : Option Core.Value) : Block Core.Value :=
  Block.replace block index cell

@[extern "witchy_block_size"]
def count (block : @& Block Core.Value) : Nat := Block.count block

@[extern "witchy_block_all"]
def every (block : @& Block Core.Value) (accepted : Option Core.Value → Bool) : Bool := Block.every block accepted

@[extern "witchy_slot_unchecked"]
def slotUnchecked (block : @& Block Core.Value) (index : @& Nat) (fallback : Core.Value) : Core.Value :=
  Memory.slotUnchecked block index fallback

def take (block : Block Core.Value) (index : Nat) : Block Core.Value × Option Core.Value :=
  match lookup block index with
  | some (some value) => (replace block index none, some value)
  | _ => (block, none)

theorem take_eq (block : Block Core.Value) (index : Nat) : take block index = block.take index := by
  simp only [take, lookup, replace, Block.take, Block.lookup, Block.replace,
    Block.slots_getElem?, Block.mk_slots_set]
  cases block[index]? with
  | none => rfl
  | some cell => cases cell <;> rfl

end Witchy.Storage.Native
