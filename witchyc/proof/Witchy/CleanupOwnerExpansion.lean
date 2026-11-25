import Witchy.CoreValueTransfer
import Witchy.HeapResourceTransfer

namespace Witchy.Storage

/-- The owner-loan guard only; location-sensitive relocation is separate. -/
def cleanupPayloadUnborrowed (memory : Memory.StateOf Core.Value) : Core.Value → Bool
  | .owner handle => (Memory.active memory handle).isEmpty
  | _ => true

/-- One atomic expansion step for recursive cleanup. Extract one initialized
    child and schedule it before retrying its parent. Refusal returns the entire
    original worklist and memory. Intermediate take results are not published.
    Selection of the next initialized cell and traversal remain separate. -/
def expandCleanupOwner (memory : Memory.StateOf Core.Value) (root index : Nat) (tail : List Core.Value) :
    Memory.StateOf Core.Value × Memory.Result Unit × List Core.Value :=
  match Memory.take memory (.owner root) index with
  | (_, .error reason) => (memory, .error reason, .owner root :: tail)
  | (next, .ok child) =>
    if cleanupPayloadUnborrowed next child then
      (next, .ok (), child :: .owner root :: tail)
    else (memory, .error .borrowed, .owner root :: tail)

/-- Expansion preserves the complete resource inventory across heap and pending
    work, and does not consume any live declaration. Applies to every outcome. -/
theorem expand_cleanup_owner_accounting (memory : Memory.StateOf Core.Value) (root index : Nat)
    (tail : List Core.Value) (resource : Core.Resource) :
    let result := expandCleanupOwner memory root index tail
    Core.registerCount (result.2.2.map some) resource + heapResources result.1 resource =
      Core.registerCount ((Core.Value.owner root :: tail).map some) resource + heapResources memory resource ∧
    Core.memoryCount result.1 resource = Core.memoryCount memory resource := by
  cases taken : Memory.take memory (.owner root) index with
  | mk next outcome =>
    cases outcome with
    | error reason => simp [expandCleanupOwner, taken]
    | ok child =>
      have accepted : (Memory.take memory (.owner root) index).2 = .ok child := by rw [taken]
      have one := take_heap_inventory memory (.owner root) index child accepted resource
      rw [taken] at one
      by_cases guard : cleanupPayloadUnborrowed next child = true
      · simp only [expandCleanupOwner, taken, guard, ↓reduceIte]
        simp only [List.map_cons, Core.registerCount, List.map_cons, List.sum_cons] at *
        constructor <;> omega
      · simp [expandCleanupOwner, taken, guard]

/-- Caller storage and pending cleanup values retain exact linear ownership. -/
theorem expand_cleanup_owner_owns (memory : Memory.StateOf Core.Value) (root index : Nat)
    (tail : List Core.Value) (registers : List (Option Core.Value))
    (owned : OwnsHeapResources (registers ++ (Core.Value.owner root :: tail).map some) memory) :
    let result := expandCleanupOwner memory root index tail
    OwnsHeapResources (registers ++ result.2.2.map some) result.1 := by
  dsimp only
  intro resource
  have prior := owned resource
  have accounting := expand_cleanup_owner_accounting memory root index tail resource
  simp only [Core.registerCount, List.map_append, List.sum_append] at *
  omega

/-- Failure retains the exact unprocessed parent and its tail. -/
theorem expand_cleanup_owner_refused (memory : Memory.StateOf Core.Value) (root index : Nat)
    (tail : List Core.Value) (reason : Memory.Error)
    (refused : (expandCleanupOwner memory root index tail).2.1 = .error reason) :
    (expandCleanupOwner memory root index tail).1 = memory ∧
      (expandCleanupOwner memory root index tail).2.2 = .owner root :: tail := by
  cases taken : Memory.take memory (.owner root) index with
  | mk next outcome =>
    cases outcome with
    | error failure => simp [expandCleanupOwner, taken]
    | ok child =>
      by_cases guard : cleanupPayloadUnborrowed next child = true
      · simp [expandCleanupOwner, taken, guard] at refused
      · simp [expandCleanupOwner, taken, guard]

end Witchy.Storage
