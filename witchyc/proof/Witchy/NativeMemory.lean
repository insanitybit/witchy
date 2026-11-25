import Witchy.NativeStorageBlock

namespace Witchy.Memory.Native

def read (s : StateOf Core.Value) (access : Access) (index : Nat) : Result Core.Value :=
  match resolve s access false with
  | .error e => .error e
  | .ok h =>
    match block? s h with
    | none => .error .invalidHandle
    | some b =>
      match Storage.Native.lookup b index with
      | none => .error .bounds
      | some none => .error .uninitialized
      | some (some value) => .ok value


def writeEmpty (s : StateOf Core.Value) (access : Access) (index : Nat) (value : Core.Value) :
    StateOf Core.Value × Result Unit :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match Storage.Native.lookup b index with
      | none => (s, .error .bounds)
      | some (some _) => (s, .error .initialized)
      | some none =>
        ({ s with blocks := s.blocks.set h (some (Storage.Native.replace b index (some value))) }, .ok ())


def take (s : StateOf Core.Value) (access : Access) (index : Nat) : StateOf Core.Value × Result Core.Value :=
  match resolve s access true with
  | .error e => (s, .error e)
  | .ok h =>
    match block? s h with
    | none => (s, .error .invalidHandle)
    | some b =>
      match Storage.Native.lookup b index with
      | none => (s, .error .bounds)
      | some none => (s, .error .uninitialized)
      | some (some value) =>
        ({ s with blocks := s.blocks.set h (some (Storage.Native.replace b index none)) }, .ok value)


def releaseEmpty (s : StateOf Core.Value) (h : Handle)
    (discardable : Core.Value → Bool := fun _ => false) : StateOf Core.Value × Result Unit :=
  match block? s h with
  | none => (s, .error .invalidHandle)
  | some block =>
    if (active s h).isEmpty then
      if Storage.Native.every block (fun cell => cell.elim true discardable) then
        ({ s with blocks := s.blocks.set h none, available := s.available + Storage.Native.count block }, .ok ())
      else (s, .error .initialized)
    else (s, .error .borrowed)


def readUnchecked (s : StateOf Core.Value) (access : Access) (index : Nat) (fallback : Core.Value) : Result Core.Value :=
  match resolve s access false with
  | .error e => .error e
  | .ok handle =>
    match block? s handle with
    | none => .error .invalidHandle
    | some block => .ok (Storage.Native.slotUnchecked block index fallback)

theorem read_eq (s : StateOf Core.Value) (access : Access) (index : Nat) : read s access index = Memory.read s access index := by
  rw [Memory.read_eq_readDense]
  simp only [read, Memory.readDense, Storage.Native.lookup]
  all_goals repeat' first | rfl | (split <;> simp_all)

theorem writeEmpty_eq (s : StateOf Core.Value) (access : Access) (index : Nat) (value : Core.Value) :
    writeEmpty s access index value = Memory.writeEmpty s access index value := by
  rw [Memory.writeEmpty_eq_writeEmptyDense]
  simp only [writeEmpty, Memory.writeEmptyDense, Storage.Native.lookup, Storage.Native.replace]
  all_goals repeat' first | rfl | (split <;> simp_all)

theorem take_eq (s : StateOf Core.Value) (access : Access) (index : Nat) : take s access index = Memory.take s access index := by
  rw [Memory.take_eq_takeDense]
  simp only [take, Memory.takeDense, Storage.Native.lookup, Storage.Native.replace]
  all_goals repeat' first | rfl | (split <;> simp_all)

theorem releaseEmpty_eq (s : StateOf Core.Value) (handle : Handle) (discardable : Core.Value → Bool) :
    releaseEmpty s handle discardable = Memory.releaseEmpty s handle discardable := by
  rw [Memory.releaseEmpty_eq_releaseEmptyDense]
  simp only [releaseEmpty, Memory.releaseEmptyDense, Storage.Native.every, Storage.Native.count]
  all_goals repeat' first | rfl | (split <;> simp_all)

theorem readUnchecked_eq (s : StateOf Core.Value) (access : Access) (index : Nat) (fallback : Core.Value) :
    readUnchecked s access index fallback = Memory.readUnchecked s access index fallback := by
  simp only [readUnchecked, Memory.readUnchecked, Storage.Native.slotUnchecked]
  all_goals repeat' first | rfl | (split <;> simp_all)

/-- Only explicit native observations materialize logical arrays. Packed blocks
    must not reach the ordinary Array-based Eq/Repr instances. No mirror is kept. -/
def normalize (s : StateOf Core.Value) : StateOf Core.Value :=
  { s with blocks := s.blocks.map (fun entry => entry.map (fun block => Storage.Block.mk (Storage.Native.slots block))) }

theorem normalize_eq (s : StateOf Core.Value) : normalize s = s := by
  cases s
  simp [normalize, Storage.Native.slots, Storage.Block.mk_slots]

def same (left right : StateOf Core.Value) : Bool := decide (normalize left = normalize right)

def repr (memory : StateOf Core.Value) : String := reprStr (normalize memory)

end Witchy.Memory.Native
