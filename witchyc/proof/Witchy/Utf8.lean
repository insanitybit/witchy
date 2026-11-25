import Init

namespace Witchy.Utf8

def Continuation (byte : Nat) : Prop :=
  128 ≤ byte ∧ byte ≤ 191

/-- The possible encodings of one Unicode scalar, including the restricted
    second-byte ranges for overlong encodings, surrogates and U+10FFFF. -/
inductive Scalar : List Nat → Prop where
  | ascii {a : Nat} : a < 128 → Scalar [a]
  | two {a b : Nat} : 194 ≤ a → a ≤ 223 → Continuation b → Scalar [a, b]
  | three {a b c : Nat} : 224 ≤ a → a ≤ 239 → Continuation b → Continuation c →
      (a = 224 → 160 ≤ b) → (a = 237 → b ≤ 159) → Scalar [a, b, c]
  | four {a b c d : Nat} : 240 ≤ a → a ≤ 244 → Continuation b → Continuation c → Continuation d →
      (a = 240 → 144 ≤ b) → (a = 244 → b ≤ 143) → Scalar [a, b, c, d]

/-- A sequence of complete scalar encodings. -/
inductive Valid : List Nat → Prop where
  | empty : Valid []
  | next {scalar rest : List Nat} : Scalar scalar → Valid rest → Valid (scalar ++ rest)

theorem Valid.append {first second : List Nat}
    (left : Valid first) (right : Valid second) : Valid (first ++ second) := by
  induction left with
  | empty => exact right
  | next scalar _ suffix =>
    rw [List.append_assoc]
    exact Valid.next scalar suffix

theorem Scalar.valid {bytes : List Nat} (scalar : Scalar bytes) : Valid bytes := by
  simpa using Valid.next scalar Valid.empty

theorem Scalar.continuation_inside {bytes : List Nat} (encoding : Scalar bytes)
    {index byte : Nat} (positive : 0 < index) (inside : index < bytes.length)
    (atIndex : bytes[index]? = some byte) : Continuation byte := by
  cases encoding with
  | ascii => simp at inside; omega
  | two _ _ hb =>
    have one : index = 1 := by simp at inside; omega
    subst index
    simpa using (show Continuation byte from by
      simp at atIndex
      simpa [← atIndex] using hb)
  | three _ _ hb hc _ _ =>
    have positions : index = 1 ∨ index = 2 := by simp at inside; omega
    rcases positions with rfl | rfl
    · simp at atIndex
      simpa [← atIndex] using hb
    · simp at atIndex
      simpa [← atIndex] using hc
  | four _ _ hb hc hd _ _ =>
    have positions : index = 1 ∨ index = 2 ∨ index = 3 := by simp at inside; omega
    rcases positions with rfl | rfl | rfl
    · simp at atIndex
      simpa [← atIndex] using hb
    · simp at atIndex
      simpa [← atIndex] using hc
    · simp at atIndex
      simpa [← atIndex] using hd

theorem Valid.takeAtBoundary {bytes : List Nat} (valid : Valid bytes) {count : Nat}
    (boundary : ∀ byte, bytes[count]? = some byte → ¬ Continuation byte) :
    Valid (bytes.take count) := by
  induction valid generalizing count with
  | empty => simpa using Valid.empty
  | @next scalar rest encoding _ ih =>
    by_cases past : scalar.length ≤ count
    · rw [List.take_append, List.take_of_length_le past]
      apply Valid.next encoding
      apply ih
      intro byte atIndex
      apply boundary byte
      simpa [List.getElem?_append_right past] using atIndex
    · have countZero : count = 0 := by
        apply Classical.byContradiction
        intro hn
        have inside : count < scalar.length := by omega
        let byte := scalar[count]
        have found : scalar[count]? = some byte := by simp [byte, inside]
        exact boundary byte (by simpa [List.getElem?_append_left inside] using found)
          (encoding.continuation_inside (by omega) inside found)
      subst count
      exact Valid.empty

theorem Valid.dropAtBoundary {bytes : List Nat} (valid : Valid bytes) {count : Nat}
    (boundary : ∀ byte, bytes[count]? = some byte → ¬ Continuation byte) :
    Valid (bytes.drop count) := by
  induction valid generalizing count with
  | empty => simpa using Valid.empty
  | @next scalar rest encoding tail ih =>
    by_cases past : scalar.length ≤ count
    · rw [List.drop_append, List.drop_eq_nil_of_le past, List.nil_append]
      apply ih
      intro byte atIndex
      exact boundary byte (by simpa [List.getElem?_append_right past] using atIndex)
    · have countZero : count = 0 := by
        apply Classical.byContradiction
        intro hn
        have inside : count < scalar.length := by omega
        let byte := scalar[count]
        have found : scalar[count]? = some byte := by simp [byte, inside]
        exact boundary byte (by simpa [List.getElem?_append_left inside] using found)
          (encoding.continuation_inside (by omega) inside found)
      subst count
      exact Valid.next encoding tail

/-- Read a half-open range from an indexed sequence. The storage proof
    separately establishes that the cells in the range are initialized. -/
def bytesAt (byte : Nat → Nat) (start : Nat) : Nat → List Nat
  | 0 => []
  | length + 1 => byte start :: bytesAt byte (start + 1) length

/-- An absent cell decodes as 256, which no scalar encoding permits. -/
def ValidCells (cell : Nat → Option Nat) (start count : Nat) : Prop :=
  Valid (bytesAt (fun i => (cell i).getD 256) start count)

@[simp] theorem validCells_empty (cell : Nat → Option Nat) (start : Nat) :
    ValidCells cell start 0 := Valid.empty

theorem ValidCells.ascii {cell : Nat → Option Nat} {start byte : Nat}
    (stored : cell start = some byte) (ascii : byte < 128) :
    ValidCells cell start 1 := by
  simpa [ValidCells, bytesAt, stored] using (Scalar.ascii ascii).valid

theorem ValidCells.asciiRange {cell : Nat → Option Nat} {start count : Nat}
    (ascii : ∀ i, i < count → (cell (start + i)).getD 256 < 128) :
    ValidCells cell start count := by
  induction count generalizing start with
  | zero => exact validCells_empty cell start
  | succ count ih =>
    unfold ValidCells bytesAt
    apply Valid.next (scalar := [(cell start).getD 256])
    · exact Scalar.ascii (by simpa using ascii 0 (Nat.zero_lt_succ count))
    · apply ih
      intro i hi
      rw [show start + 1 + i = start + (i + 1) by omega]
      exact ascii (i + 1) (by omega)

theorem bytesAt_append (byte : Nat → Nat) (start first second : Nat) :
    bytesAt byte start (first + second) =
      bytesAt byte start first ++ bytesAt byte (start + first) second := by
  induction first generalizing start with
  | zero => simp [bytesAt]
  | succ first ih =>
    rw [Nat.succ_add]
    simp only [bytesAt, List.cons_append, List.cons.injEq, true_and]
    rw [ih]
    congr 2
    omega

theorem bytesAt_congr {left right : Nat → Nat} {a b length : Nat}
    (same : ∀ i, i < length → left (a + i) = right (b + i)) :
    bytesAt left a length = bytesAt right b length := by
  induction length generalizing a b with
  | zero => rfl
  | succ length ih =>
    simp only [bytesAt, List.cons.injEq]
    constructor
    · simpa using same 0 (Nat.zero_lt_succ length)
    · apply ih
      intro i hi
      simpa [Nat.add_assoc, Nat.add_comm 1] using same (i + 1) (by omega)

theorem Valid.adjacent {byte : Nat → Nat} {start first second : Nat}
    (left : Valid (bytesAt byte start first))
    (right : Valid (bytesAt byte (start + first) second)) :
    Valid (bytesAt byte start (first + second)) := by
  rw [bytesAt_append]
  exact left.append right

/-- Appending maintains validity when the original prefix is preserved and
    the appended range equals the source byte for byte. -/
theorem Valid.copiedAppend {before source after : Nat → Nat} {used count : Nat}
    (initial : Valid (bytesAt before 0 used))
    (input : Valid (bytesAt source 0 count))
    (kept : ∀ i, i < used → after i = before i)
    (copied : ∀ i, i < count → after (used + i) = source i) :
    Valid (bytesAt after 0 (used + count)) := by
  apply Valid.adjacent
  · rwa [bytesAt_congr (a := 0) (b := 0) (fun i hi => by simpa using kept i hi)]
  · rw [Nat.zero_add]
    rwa [bytesAt_congr (a := used) (b := 0) (fun i hi => by simpa using copied i hi)]

/-- Buffer growth may move a byte range to a fresh allocation. The UTF-8
    fact transfers along equality of the initialized cells. -/
theorem ValidCells.copy {before after : Nat → Option Nat} {a b count : Nat}
    (valid : ValidCells before a count)
    (copied : ∀ i, i < count → after (b + i) = before (a + i)) :
    ValidCells after b count := by
  unfold ValidCells at *
  rw [bytesAt_congr (right := fun i => (before i).getD 256) (b := a) (fun i hi => congrArg (fun cell => cell.getD 256) (copied i hi))]
  exact valid

/-- The cell map after an append. The range still has to fit in the buffer
    and land in vacant cells; those are separate memory obligations. -/
theorem ValidCells.ofSliceContents {slice buffer : Nat → Option Nat} {start count : Nat}
    (contents : ∀ i, i < count → slice i = buffer (start + i))
    (valid : ValidCells slice 0 count) :
    ValidCells buffer start count :=
  valid.copy (fun i inside => by simpa only [Nat.zero_add] using (contents i inside).symm)

theorem ValidCells.appendRange {target source : Nat → Option Nat} {used count : Nat}
    (initial : ValidCells target 0 used)
    (input : ValidCells source 0 count) :
    ValidCells
      (fun j => if j < used then target j
        else if j < used + count then source (j - used) else target j)
      0 (used + count) := by
  unfold ValidCells at *
  apply Valid.copiedAppend initial input
  · intro i hi
    simp [hi]
  · intro i hi
    have within : used + i < used + count := by omega
    have past : ¬ used + i < used := by omega
    have atSource : used + i - used = i := by omega
    simp [past, within, atSource]

theorem ValidCells.appendAscii {cells : Nat → Option Nat} {used byte : Nat}
    (initial : ValidCells cells 0 used) (ascii : byte < 128) :
    ValidCells (fun i => if i = used then some byte else cells i) 0 (used + 1) := by
  unfold ValidCells at *
  apply Valid.copiedAppend (source := fun _ => byte) initial
  · simpa [bytesAt] using (Scalar.ascii ascii).valid
  · intro i hi
    simp [show i ≠ used by omega]
  · intro i hi
    have zero : i = 0 := by omega
    simp [zero]

/-- The prefix equality contract used by consuming vector reserve is enough
    to transport UTF-8 validity, regardless of allocation identity. -/
theorem ValidCells.copyPrefix {before after : Nat → Option Nat} {count : Nat}
    (valid : ValidCells before 0 count)
    (copied : ∀ i, i < count → after i = before i) :
    ValidCells after 0 count :=
  valid.copy (by simpa using copied)

theorem bytesAt_take (byte : Nat → Nat) (start count kept : Nat)
    (fits : kept ≤ count) :
    (bytesAt byte start count).take kept = bytesAt byte start kept := by
  induction kept generalizing start count with
  | zero => rfl
  | succ kept ih =>
    cases count with
    | zero => omega
    | succ count =>
      simp only [bytesAt, List.take_succ_cons]
      rw [ih (fits := by omega)]

theorem bytesAt_get (byte : Nat → Nat) (start count index : Nat) :
    (bytesAt byte start count)[index]? =
      if index < count then some (byte (start + index)) else none := by
  induction count generalizing start index with
  | zero => simp [bytesAt]
  | succ count ih =>
    cases index with
    | zero => simp [bytesAt]
    | succ index =>
      simp only [bytesAt, List.getElem?_cons_succ, ih, Nat.succ_lt_succ_iff]
      rw [Nat.add_right_comm start 1 index, Nat.add_assoc]

/-- Keep a prefix ending at the end of the range or at a scalar boundary. -/
theorem ValidCells.takeAtBoundary {cell : Nat → Option Nat} {start count kept : Nat}
    (valid : ValidCells cell start count) (fits : kept ≤ count)
    (boundary : kept < count → ¬ Continuation ((cell (start + kept)).getD 256)) :
    ValidCells cell start kept := by
  unfold ValidCells at *
  rw [← bytesAt_take _ _ count kept fits]
  apply valid.takeAtBoundary
  intro byte atIndex
  rw [bytesAt_get] at atIndex
  by_cases inside : kept < count
  · simpa [inside] using (show ¬ Continuation byte from by
      simp [inside] at atIndex
      exact atIndex ▸ boundary inside)
  · simp [inside] at atIndex

theorem bytesAt_drop (byte : Nat → Nat) (start count skipped : Nat) :
    (bytesAt byte start count).drop skipped =
      bytesAt byte (start + skipped) (count - skipped) := by
  induction skipped generalizing start count with
  | zero => simp
  | succ skipped ih =>
    cases count with
    | zero => simp [bytesAt]
    | succ count =>
      simp only [bytesAt, List.drop_succ_cons, Nat.succ_sub_succ_eq_sub]
      rw [ih]
      congr 1
      omega

theorem ValidCells.partAtBoundaries {cell : Nat → Option Nat} {start count lo hi : Nat}
    (valid : ValidCells cell start count) (fits : hi ≤ count)
    (left : lo < count → ¬ Continuation ((cell (start + lo)).getD 256))
    (right : hi < count → ¬ Continuation ((cell (start + hi)).getD 256)) :
    ValidCells cell (start + lo) (hi - lo) := by
  have prefixValid := valid.takeAtBoundary fits right
  unfold ValidCells at prefixValid ⊢
  rw [← bytesAt_drop]
  apply prefixValid.dropAtBoundary
  intro byte atIndex
  rw [bytesAt_get] at atIndex
  by_cases inside : lo < hi
  · simp [inside] at atIndex
    exact atIndex ▸ left (by omega)
  · simp [inside] at atIndex

theorem ValidCells.slice {cell : Nat → Option Nat} {count lo hi : Nat}
    (valid : ValidCells cell 0 count) (fits : hi ≤ count)
    (left : lo < count → ¬ Continuation ((cell lo).getD 256))
    (right : hi < count → ¬ Continuation ((cell hi).getD 256)) :
    ValidCells (fun index => cell (lo + index)) 0 (hi - lo) := by
  have selected := valid.partAtBoundaries fits
    (by simpa using left) (by simpa using right)
  apply selected.copy
  intro index _
  simp

theorem ValidCells.takeCopied {before after : Nat → Option Nat} {count kept : Nat}
    (copied : ∀ i, i < kept → after i = before i ∧ before i ≠ none)
    (valid : ValidCells before 0 count) (fits : kept ≤ count)
    (boundary : kept < count → ¬ Continuation ((before kept).getD 256)) :
    ValidCells after 0 kept :=
  (valid.takeAtBoundary fits (by simpa using boundary)).copyPrefix
    (fun i inside => (copied i inside).1)

end Witchy.Utf8
