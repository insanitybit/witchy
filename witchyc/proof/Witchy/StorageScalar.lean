namespace Witchy.Storage

/- The one checked numeric scalar storage descriptor: a width in bytes.
    Size, stride, alignment, range and the native codec are all derived from
    it; no width has its own representation or rule. Supported widths are the
    powers of two up to the 64-bit register. -/
namespace Scalar

def wellFormed (width : Nat) : Bool :=
  decide (width = 1 ∨ width = 2 ∨ width = 4 ∨ width = 8)

/-- Exclusive upper bound of a stored value: `2^(8·width)`. Written as literals
    so generated proofs and `omega` see numbers; malformed widths accept nothing. -/
def bound : Nat → Nat
  | 1 => 256
  | 2 => 65536
  | 4 => 4294967296
  | 8 => 18446744073709551616
  | _ => 0

@[simp, grind =] theorem bound_one : bound 1 = 256 := rfl
@[simp, grind =] theorem bound_two : bound 2 = 65536 := rfl
@[simp, grind =] theorem bound_four : bound 4 = 4294967296 := rfl
@[simp, grind =] theorem bound_eight : bound 8 = 18446744073709551616 := rfl

theorem bound_eq_pow (width : Nat) (valid : wellFormed width = true) : bound width = 2 ^ (8 * width) := by
  simp only [wellFormed, decide_eq_true_eq] at valid
  rcases valid with h | h | h | h <;> subst h <;> decide

/-- Every register word fits the full-width descriptor. -/
theorem word_fits (value : UInt64) : value.toNat < bound 8 := by
  have := UInt64.toNat_lt value
  simpa [bound] using this

/-- Decidable range check shared by register packing and cell validity. -/
def fits (width : Nat) (value : UInt64) : Bool := decide (value.toNat < bound width)

theorem fits_iff (width : Nat) (value : UInt64) : fits width value = true ↔ value.toNat < bound width := by
  simp [fits]

theorem wellFormed_alignment (width : Nat) (valid : wellFormed width = true) : ∃ exponent : Nat, width = 2 ^ exponent := by
  simp only [wellFormed, decide_eq_true_eq] at valid
  rcases valid with h | h | h | h <;> subst h
  · exact ⟨0, rfl⟩
  · exact ⟨1, rfl⟩
  · exact ⟨2, rfl⟩
  · exact ⟨3, rfl⟩

theorem wellFormed_le (width : Nat) (valid : wellFormed width = true) : 1 ≤ width ∧ width ≤ 8 := by
  simp only [wellFormed, decide_eq_true_eq] at valid
  omega

end Scalar
end Witchy.Storage
