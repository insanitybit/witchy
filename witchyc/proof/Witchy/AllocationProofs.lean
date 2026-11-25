import Witchy.CoreProofs
import Witchy.MemoryProofs

namespace Witchy.Core

theorem putCopy_preserves_memory (s next : State) (dst : Nat) (value : Value)
    (h : putCopy s dst value = .ok next) : next.memory = s.memory := by
  unfold putCopy at h
  split at h <;> cases h <;> rfl

/-- A recoverable allocation refusal changes only result registers, not the
    memory containing the old vector, its contents, quota or live loans. -/
theorem refused_try_allocation_preserves_memory (s next : State) (tag dst error count : Nat)
    (size : UInt64) (reason : Memory.Error)
    (hn : number s count = .ok size)
    (hm : (Memory.allocate s.memory size.toNat).2 = .error reason)
    (h : step s (.tryAllocate tag dst error count) = .ok next) : next.memory = s.memory := by
  simp only [step, hn, hm, Bind.bind, putCopy] at h
  repeat' first
    | (cases h <;> rfl)
    | (split at * <;> try simp_all)
  all_goals (subst_vars; rfl)

theorem try_allocation_memory_cases (s next : State) (tag dst error count : Nat)
    (h : step s (.tryAllocate tag dst error count) = .ok next) :
    next.memory = s.memory ∨ ∃ size : Nat, next.memory = (Memory.allocate s.memory size).1 := by
  simp only [step, Bind.bind, Pure.pure, put, putCopy] at h
  repeat' first
    | (cases h <;> first | exact Or.inl rfl | exact Or.inr ⟨_, rfl⟩)
    | (split at * <;> try simp_all)
  all_goals
    subst_vars
    first | exact Or.inl rfl | exact Or.inr ⟨_, rfl⟩

theorem try_allocation_preserves_quota (s next : State) (tag dst error count : Nat)
    (h : step s (.tryAllocate tag dst error count) = .ok next) :
    Memory.quota next.memory = Memory.quota s.memory := by
  rcases try_allocation_memory_cases s next tag dst error count h with same | ⟨size, allocated⟩
  · rw [same]
  · rw [allocated]
    exact Memory.allocation_preserves_quota s.memory size

/-- The fixed-width multiplication used by the source vector cannot overflow
    after its explicit half-maximum check. Source/core correspondence is separate. -/
theorem checked_doubling (capacity : UInt64) (h : capacity.toNat ≤ 9223372036854775807) :
    arithmetic .multiply capacity 2 = .ok (.number (UInt64.ofNat (capacity.toNat * 2))) := by
  have bounded : capacity.toNat * 2 < 2^64 := by omega
  simp [arithmetic, bounded]

theorem checked_doubling_refuses_overflow (capacity : UInt64)
    (h : 9223372036854775807 < capacity.toNat) :
    arithmetic .multiply capacity 2 = .error .arithmetic := by
  have overflow : ¬capacity.toNat * 2 < 2^64 := by omega
  simp [arithmetic, overflow]

theorem checked_push_increment (length capacity : UInt64) (h : length.toNat < capacity.toNat) :
    arithmetic .add length 1 = .ok (.number (UInt64.ofNat (length.toNat + 1))) := by
  have limit := UInt64.toNat_lt capacity
  have bounded : length.toNat + 1 < 2^64 := by omega
  simp [arithmetic, bounded]

theorem checked_pop_decrement (length : UInt64) (h : 0 < length.toNat) :
    arithmetic .subtract length 1 = .ok (.number (UInt64.ofNat (length.toNat - 1))) := by
  have limit := UInt64.toNat_lt length
  have bounded : length.toNat - 1 < 2^64 := by omega
  have atLeastOne : 1 ≤ length.toNat := by omega
  have positive : (1 : UInt64) ≤ length := by simpa [UInt64.le_iff_toNat_le] using atLeastOne
  simp [arithmetic, bounded, positive]

end Witchy.Core
