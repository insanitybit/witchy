import Witchy.CoreStep

namespace Witchy.Core

/-- An accepted Core conversion reflects the same accepted memory result. -/
theorem from_memory_ok {α : Type} (outcome : Memory.Result α) (value : α)
    (accepted : fromMemory outcome = .ok value) : outcome = .ok value := by
  cases outcome <;> simp_all [fromMemory]

end Witchy.Core
