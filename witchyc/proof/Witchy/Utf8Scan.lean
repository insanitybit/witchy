import Witchy.Utf8

namespace Witchy.Utf8

private def continuation (byte : Nat) : Bool :=
  decide (128 ≤ byte ∧ byte ≤ 191)

/-- Recognize complete scalar encodings one at a time. The byte tests are the
same as the checked byte-to-text conversion's lead and continuation tests. -/
def scan : List Nat → Bool
  | [] => true
  | a :: rest =>
    if a < 128 then scan rest
    else if 194 ≤ a ∧ a ≤ 223 then
      match rest with
      | b :: tail => continuation b && scan tail
      | [] => false
    else if 224 ≤ a ∧ a ≤ 239 then
      match rest with
      | b :: c :: tail =>
        continuation b && continuation c &&
          decide ((a = 224 → 160 ≤ b) ∧ (a = 237 → b ≤ 159)) && scan tail
      | _ => false
    else if 240 ≤ a ∧ a ≤ 244 then
      match rest with
      | b :: c :: d :: tail =>
        continuation b && continuation c && continuation d &&
          decide ((a = 240 → 144 ≤ b) ∧ (a = 244 → b ≤ 143)) && scan tail
      | _ => false
    else false

/-- Skipping a block of ASCII bytes leaves the scanner's decision unchanged. -/
theorem scan_skip_ascii (front suffix : List Nat)
    (ascii : ∀ byte ∈ front, byte < 128) :
    scan (front ++ suffix) = scan suffix := by
  induction front with
  | nil => rfl
  | cons byte rest ih =>
    have head := ascii byte (List.mem_cons_self ..)
    have tail : ∀ value ∈ rest, value < 128 :=
      fun value member => ascii value (List.mem_cons_of_mem byte member)
    change scan (byte :: (rest ++ suffix)) = scan suffix
    rw [scan.eq_def]
    simpa only [ite_eq_left head] using ih tail

/-- A zero result from the little-endian load's high-bit mask establishes that
    all eight bytes are ASCII. -/
theorem ascii_word_bytes (a b c d e f g h : BitVec 8)
    (masked : (((((((a.zeroExtend 64 ||| (b.zeroExtend 64 <<< 8))
        ||| (c.zeroExtend 64 <<< 16)) ||| (d.zeroExtend 64 <<< 24))
        ||| (e.zeroExtend 64 <<< 32)) ||| (f.zeroExtend 64 <<< 40))
        ||| (g.zeroExtend 64 <<< 48)) ||| (h.zeroExtend 64 <<< 56))
        &&& 9259542123273814144#64 = 0#64) :
    a < 128#8 ∧ b < 128#8 ∧ c < 128#8 ∧ d < 128#8 ∧
    e < 128#8 ∧ f < 128#8 ∧ g < 128#8 ∧ h < 128#8 := by
  have bit (index : Nat) := congrArg (fun word : BitVec 64 => word.getLsbD index) masked
  repeat' apply And.intro
  all_goals
    rw [BitVec.lt_def]
    apply BitVec.toNat_lt_of_msb_false
    rw [BitVec.msb_eq_getLsbD_last]
    first
    | simpa [BitVec.zeroExtend] using bit 7
    | simpa [BitVec.zeroExtend] using bit 15
    | simpa [BitVec.zeroExtend] using bit 23
    | simpa [BitVec.zeroExtend] using bit 31
    | simpa [BitVec.zeroExtend] using bit 39
    | simpa [BitVec.zeroExtend] using bit 47
    | simpa [BitVec.zeroExtend] using bit 55
    | simpa [BitVec.zeroExtend] using bit 63

theorem scan_valid (bytes : List Nat) (ok : scan bytes = true) : Valid bytes := by
  induction bytes using scan.induct with
  | case1 => exact .empty
  | case2 a rest ascii ih =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_left ascii] at ok
    exact .next (Scalar.ascii ascii) (ih ok)
  | case3 a notAscii width b tail ih =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_left width] at ok
    simp [continuation, Bool.and_eq_true] at ok
    exact .next (Scalar.two width.1 width.2 ok.1) (ih ok.2)
  | case4 a notAscii width =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_left width] at ok
    contradiction
  | case5 a notAscii notTwo width b c tail ih =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_right notTwo, ite_eq_left width] at ok
    simp [continuation, Bool.and_eq_true] at ok
    rcases ok with ⟨⟨⟨hb, hc⟩, lo, hi⟩, next⟩
    exact .next (Scalar.three width.1 width.2 hb hc (by omega) (by omega)) (ih next)
  | case6 a rest notAscii notTwo width short =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_right notTwo, ite_eq_left width] at ok
    cases rest with
    | nil => contradiction
    | cons b tail =>
      cases tail with
      | nil => contradiction
      | cons c tail => exact False.elim (short b c tail rfl)
  | case7 a notAscii notTwo notThree width b c d tail ih =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_right notTwo, ite_eq_right notThree, ite_eq_left width] at ok
    simp [continuation, Bool.and_eq_true] at ok
    rcases ok with ⟨⟨⟨⟨hb, hc⟩, hd⟩, lo, hi⟩, next⟩
    exact .next (Scalar.four width.1 width.2 hb hc hd (by omega) (by omega)) (ih next)
  | case8 a rest notAscii notTwo notThree width short =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_right notTwo, ite_eq_right notThree, ite_eq_left width] at ok
    cases rest with
    | nil => contradiction
    | cons b tail =>
      cases tail with
      | nil => contradiction
      | cons c tail =>
        cases tail with
        | nil => contradiction
        | cons d tail => exact False.elim (short b c d tail rfl)
  | case9 a rest notAscii notTwo notThree notFour =>
    rw [scan.eq_def] at ok
    simp only [ite_eq_right notAscii, ite_eq_right notTwo, ite_eq_right notThree, ite_eq_right notFour] at ok
    contradiction

/-- Connect a successful byte scan to the predicate used by ownership proofs.
Absent cells decode as 256 and cannot make the scan succeed. -/
theorem scan_cells_valid (cell : Nat → Option Nat) (start count : Nat)
    (accepted : scan (bytesAt (fun index => (cell index).getD 256) start count) = true) :
    ValidCells cell start count :=
  scan_valid _ accepted

end Witchy.Utf8
