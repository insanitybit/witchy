import Witchy.RegisterWrites

namespace Witchy.Core

variable {Cell : Type}

def Instruction.checkRegisterBounds (count : Nat) (instruction : Instruction) : Bool :=
  instruction.registerWrites.all (fun index => decide (index < count))

theorem put_register_length (s next : StateOf Cell) (index : Nat) (value : Value)
    (accepted : put s index value = .ok next) : next.registers.length = s.registers.length := by
  unfold put vacant at accepted
  split at accepted <;> simp only [Bind.bind, Pure.pure] at accepted
  all_goals cases accepted
  simp only [List.length_set]

theorem put_copy_register_length (s next : StateOf Cell) (index : Nat) (value : Value)
    (accepted : putCopy s index value = .ok next) : next.registers.length = s.registers.length := by
  unfold putCopy at accepted
  split at accepted <;> cases accepted <;> simp only [List.length_set]

theorem step_register_length [StorageCell Cell] (s next : StateOf Cell) (instruction : Instruction)
    (accepted : step s instruction = .ok next) : next.registers.length = s.registers.length := by
  cases instruction
  case packMove dst sources widths => exact pack_length s next dst sources widths accepted
  case unpackMove destinations src widths => exact unpack_length s next destinations src widths accepted
  all_goals simp only [step, Bind.bind, Pure.pure, fromMemory, fromAuthority] at accepted
  all_goals repeat' first | split at accepted | cases accepted
  all_goals try simp only [List.length_set]
  all_goals first
    | rfl
    | (have length := put_register_length _ _ _ _ accepted; exact length)
    | (have length := put_copy_register_length _ _ _ _ accepted; exact length)
    | exact put_register_length _ _ _ _ (by assumption)
    | (have last := put_copy_register_length _ _ _ _ accepted
       have first := put_register_length _ _ _ _ (by assumption)
       exact last.trans first)
    | (have last := put_copy_register_length _ _ _ _ accepted
       exact last.trans (put_copy_register_length _ _ _ _ (by assumption)))

theorem initial_register_length (limits : Memory.Limits) (roots : List Authority.Token)
    (count : Nat) (s : State) (accepted : initial limits roots count = .ok s) : s.registers.length = count := by
  unfold initial initialOf at accepted
  split at accepted
  · rename_i condition
    cases accepted
    simp only [List.length_append, List.length_mapIdx, List.length_replicate]
    omega
  · cases accepted

theorem initial_of_register_length (limits : Memory.Limits) (roots : List Authority.Token)
    (count : Nat) (s : StateOf Cell) (accepted : initialOf Cell limits roots count = .ok s) : s.registers.length = count := by
  unfold initialOf at accepted
  split at accepted
  · rename_i condition
    cases accepted
    simp only [List.length_append, List.length_mapIdx, List.length_replicate]
    omega
  · cases accepted

end Witchy.Core
