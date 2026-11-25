import Witchy.StorageStateInvariant
import Witchy.HeapAllocationInvariant
import Witchy.CoreOwnership

namespace Witchy.Core

/-- Trusted host initialization contains capability roots and vacant registers,
    an empty heap and no loans. Each part of storage validity follows from that
    actual state, independently of any source program. -/
theorem initial_storage_invariant (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s : StateOf Value) (host : initialOf Value limits roots registers = .ok s) :
    StorageStateInvariant s := by
  unfold initialOf at host
  split at host
  · cases host
    constructor
    · intro resource
      cases resource <;>
        simp [registerCount, Storage.heapResources, memoryCount, Memory.initialOf,
          Memory.block?, Memory.loan?, List.mapIdx_eq_zipIdx_map, List.map_map, Function.comp_def,
          weight, valueResource, sum_zero_map]
    · exact Storage.initial_ownership_wellFounded limits
    · constructor
      · intro value member
        obtain ⟨index, bound, lookup⟩ := List.mem_iff_getElem.mp member
        have found := List.getElem?_eq_some_iff.mpr ⟨bound, lookup⟩
        by_cases root : index < roots.length
        · rw [List.getElem?_append_left (by simpa using root), List.getElem?_mapIdx,
            List.getElem?_eq_getElem root] at found
          have same : value = .capability index := by simpa using found.symm
          subst value
          exact root
        · rw [List.getElem?_append_right (by simpa using Nat.le_of_not_gt root)] at found
          by_cases range : index - roots.length < registers - roots.length
          · simp [range] at found
          · simp [range] at found
      · intro block member
        simp [Memory.initialOf] at member
    · simp [LoanRootsRegistered, Memory.initialOf]
  · cases host

end Witchy.Core
