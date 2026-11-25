import Witchy.CoreStep

namespace Witchy.Core

theorem owner_not_copyable (s : State) (dst h : Nat) :
    putCopy s dst (.owner h) = .error .type := by
  cases hd : s.registers[dst]? with
  | none => simp [putCopy, hd]
  | some cell => cases cell <;> simp [putCopy, hd]

theorem reference_not_copyable (s : State) (dst id : Nat) :
    putCopy s dst (.reference id) = .error .type := by
  cases hd : s.registers[dst]? with
  | none => simp [putCopy, hd]
  | some cell => cases cell <;> simp [putCopy, hd]

theorem capability_not_implicitly_copyable (s : State) (dst id : Nat) :
    putCopy s dst (.capability id) = .error .type := by
  cases hd : s.registers[dst]? with
  | none => simp [putCopy, hd]
  | some cell => cases cell <;> simp [putCopy, hd]

theorem copy_owner_rejected (s : State) (dst src h : Nat) (hv : get s src = .ok (.owner h)) :
    step s (.copy dst src) = .error .type := by
  simpa only [step, hv, instMonadResult, Bind.bind] using owner_not_copyable s dst h

theorem number_is_not_capability (s : State) (src : Nat) (n : UInt64)
    (hv : get s src = .ok (.number n)) : cap s src = .error .type := by
  simp [cap, hv, Bind.bind]

end Witchy.Core
