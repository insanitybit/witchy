import Witchy.Core

/-! Fixed scalar normalization shared by numeric source-postcondition proofs.
No path equality becomes a rewrite rule. Every rewrite uses an existing checked
constructor, Boolean or arithmetic lemma; ordinary warning policy stays enabled.
-/
macro "witchy_scalar_post_normalize" : tactic =>
  `(tactic|
    (simp only [Option.some.injEq, _root_.Witchy.Core.Value.boolean.injEq, decide_eq_true_iff, decide_eq_false_iff_not] at *
     simp (disch := omega) only [UInt64.le_iff_toNat_le, UInt64.lt_iff_toNat_lt, UInt64.toNat_zero, UInt64.toNat_one, UInt64.toNat_ofNat, UInt64.toNat_add, UInt64.toNat_sub_of_le, Nat.mod_eq_of_lt] at *
     omega))
