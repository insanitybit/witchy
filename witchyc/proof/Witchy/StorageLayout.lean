import Witchy.Core

namespace Witchy.Storage

/-- Byte size and element stride are distinct: stride includes padding.
    Descriptors do not grant access or establish allocation provenance. -/
structure Layout where
  size : UInt64
  stride : UInt64
  alignment : UInt64
  deriving Repr, DecidableEq

def Layout.Valid (layout : Layout) : Prop :=
  layout.size.toNat ≤ layout.stride.toNat ∧
    layout.stride.toNat % layout.alignment.toNat = 0 ∧
    ∃ exponent : Nat, layout.alignment.toNat = 2 ^ exponent

/-- Use the executor's checked multiplication, rather than wrapped byte sizing. -/
def Layout.allocationBytes (layout : Layout) (count : UInt64) : Core.Result Core.Value :=
  Core.arithmetic .multiply layout.stride count

theorem Layout.allocation_bytes_checked (layout : Layout) (count : UInt64)
    (bounded : layout.stride.toNat * count.toNat < 2^64) :
    layout.allocationBytes count = .ok (.number (UInt64.ofNat (layout.stride.toNat * count.toNat))) := by
  simp [Layout.allocationBytes, Core.arithmetic, bounded]

theorem Layout.allocation_bytes_overflow (layout : Layout) (count : UInt64)
    (overflow : 2^64 ≤ layout.stride.toNat * count.toNat) :
    layout.allocationBytes count = .error .arithmetic := by
  have outside : ¬ layout.stride.toNat * count.toNat < 2^64 := by omega
  simp [Layout.allocationBytes, Core.arithmetic, outside]

/-- Every complete element lies within the requested allocation extent,
    including padding and zero-sized layouts. This is not a pointer permission. -/
theorem Layout.element_bounds (layout : Layout) (valid : layout.Valid) (count index : Nat)
    (inside : index < count) :
    layout.stride.toNat * index + layout.size.toNat ≤ layout.stride.toNat * count := by
  have covered := Nat.mul_le_mul_left layout.stride.toNat (show index + 1 ≤ count by omega)
  rw [Nat.mul_add, Nat.mul_one] at covered
  have size := valid.1
  omega

/-- Strided element offsets preserve the allocation base's alignment. -/
theorem Layout.element_alignment (layout : Layout) (valid : layout.Valid) (base index : Nat)
    (aligned : base % layout.alignment.toNat = 0) :
    (base + layout.stride.toNat * index) % layout.alignment.toNat = 0 := by
  simp [Nat.add_mod, Nat.mul_mod, aligned, valid.2.1]

/-- An in-range element's byte offset uses the same checked executor arithmetic.
    Byte bounds are established before converting the mathematical product. -/
theorem Layout.element_offset_checked (layout : Layout) (valid : layout.Valid) (count index : UInt64)
    (inside : index.toNat < count.toNat) (bounded : layout.stride.toNat * count.toNat < 2^64) :
    Core.arithmetic .multiply layout.stride index =
      .ok (.number (UInt64.ofNat (layout.stride.toNat * index.toNat))) ∧
    (UInt64.ofNat (layout.stride.toNat * index.toNat)).toNat = layout.stride.toNat * index.toNat := by
  have covered := Layout.element_bounds layout valid count.toNat index.toNat inside
  have offsetBound : layout.stride.toNat * index.toNat < 2^64 := by omega
  exact ⟨by simp [Core.arithmetic, offsetBound], by rw [UInt64.toNat_ofNat', Nat.mod_eq_of_lt offsetBound]⟩

theorem Layout.element_end_checked (layout : Layout) (valid : layout.Valid) (count index : UInt64)
    (inside : index.toNat < count.toNat) (bounded : layout.stride.toNat * count.toNat < 2^64) :
    Core.arithmetic .add (UInt64.ofNat (layout.stride.toNat * index.toNat)) layout.size =
      .ok (.number (UInt64.ofNat (layout.stride.toNat * index.toNat + layout.size.toNat))) ∧
    (UInt64.ofNat (layout.stride.toNat * index.toNat + layout.size.toNat)).toNat =
      layout.stride.toNat * index.toNat + layout.size.toNat := by
  have decoded := (Layout.element_offset_checked layout valid count index inside bounded).2
  have covered := Layout.element_bounds layout valid count.toNat index.toNat inside
  have endBound : layout.stride.toNat * index.toNat + layout.size.toNat < 2^64 := by omega
  constructor
  · simp only [Core.arithmetic]
    rw [decoded]
    simp [endBound]
  · rw [UInt64.toNat_ofNat', Nat.mod_eq_of_lt endBound]

end Witchy.Storage
