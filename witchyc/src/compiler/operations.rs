//! Semantic operators shared by logical and concrete bodies.
#[derive(Clone, Copy, Debug)]
pub(super) enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Operations on I64, the two's-complement 64-bit integer. They are kept apart from `Binary`,
/// whose operands the proofs read as natural numbers: no proof reads an I64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signed {
    Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge,
    /// The I64 that has these 64 bits. Its operand is a U64.
    FromBits,
    /// The 64 bits of an I64, as a U64.
    Bits,
}

/// A bit operation on unsigned integers of one type (`& | ^ ~ << >>`). A shift by the type's
/// width or more traps; bits shifted out are lost, and a narrow type's `<<` and `~` keep only its
/// own bits. Proofs read the result as an unknown value of the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bits {
    And,
    Or,
    Xor,
    Not,
    Shl,
    Shr,
    WrapAdd,
    WrapSub,
    WrapMul,
    MulHigh,
    /// How many zero bits lie below the lowest set bit of a U64 (64 for zero); cannot trap.
    TrailingZeros,
    /// How many bits of a U64 are set; cannot trap.
    CountOnes,
    Packed(Packed),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaneOp {
    Add,
    Sub,
    Mul,
    Eq,
    Gt,
    And,
    Or,
    Xor,
    Mask,
    EqMask,
    FloatAdd,
    FloatMul,
    FloatDiv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Packed {
    pub bits: u8,
    pub op: LaneOp,
    pub high: bool,
    pub both: bool,
}

impl Bits {
    pub(super) fn word_method(name: &str) -> Option<(Self, usize)> {
        let op = match name {
            "wrapping_add" => Self::WrapAdd,
            "wrapping_sub" => Self::WrapSub,
            "wrapping_mul" => Self::WrapMul,
            "mul_high" => Self::MulHigh,
            "trailing_zeros" => return Some((Self::TrailingZeros, 1)),
            "count_ones" => return Some((Self::CountOnes, 1)),
            "byte_bitmask" | "byte_eq_mask" => {
                let (op, arity) = if name == "byte_bitmask" { (LaneOp::Mask, 2) } else { (LaneOp::EqMask, 4) };
                return Some((
                    Self::Packed(Packed {
                        bits: 8,
                        op,
                        high: false,
                        both: false,
                    }),
                    arity,
                ));
            }
            _ => {
                let mut parts = name.strip_prefix("packed_")?.split('_');
                let op = match parts.next()? {
                    "fadd" => LaneOp::FloatAdd,
                    "fmul" => LaneOp::FloatMul,
                    "fdiv" => LaneOp::FloatDiv,
                    "add" => LaneOp::Add,
                    "sub" => LaneOp::Sub,
                    "mul" => LaneOp::Mul,
                    "eq" => LaneOp::Eq,
                    "gt" => LaneOp::Gt,
                    "and" => LaneOp::And,
                    "or" => LaneOp::Or,
                    "xor" => LaneOp::Xor,
                    _ => return None,
                };
                let bits = match parts.next()? {
                    "8" => 8,
                    "16" => 16,
                    "32" => 32,
                    "64" => 64,
                    _ => return None,
                };
                let both = matches!(op, LaneOp::FloatAdd | LaneOp::FloatMul | LaneOp::FloatDiv);
                let high = match (parts.next()?, both) {
                    ("pair", true) if bits == 64 => false,
                    ("lo", false) => false,
                    ("hi", false) => true,
                    _ => return None,
                };
                if parts.next().is_some() {
                    return None;
                }
                return Some((Self::Packed(Packed { bits, op, high, both }), 4));
            }
        };
        Some((op, 2))
    }
}

/// An operation on F64 values (docs/wasm-f64.md): IEEE 754 binary64, rounded to nearest. None
/// traps but `ToI64`, which traps on NaN and on a value outside I64 (it truncates toward zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Float {
    Add, Sub, Mul, Div, Neg, Eq, Ne, Lt, Le, Gt, Ge,
    /// The F64 with these 64 bits (a U64); a literal is one of these.
    FromBits,
    /// The 64 bits of an F64, as a U64.
    Bits,
    FromU64, FromI64, ToI64, Sqrt, Abs, Floor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StorageOp { Init, Read, Take, Swap, GetUnchecked, RefUnchecked, Initialized,
    /// First-cell loan via a stored allocation token, proved equal to the owner's.
    RefLink, MutLink,
    /// Proved in bounds and occupied: a `&mut` reference to the cell. The proof goes on as if
    /// the cell held any value at all, since the holder of the reference may store one.
    MutUnchecked,
    /// Checked at run time: a reference to an occupied cell, or an error code.
    Ref,
    /// Checked at run time: fill an empty cell, or hand the value back.
    TryInit, InitUnchecked, TakeUnchecked,
    /// Replace one proved full cell, returning its previous owner without changing occupancy.
    ReplaceUnchecked,
    /// Proved: cells `from..to` (index, then value) all hold values and `to <= capacity`; the
    /// run of them as `&[T]` (docs/design-slices.md).
    SliceUnchecked,
    /// Proved: cells `0..count` of the source (receiver) all hold values, those of the target
    /// (`value`) are all empty, and `count` fits both. The values move in order; the source's
    /// cells are left empty. `index` is the count (docs/proposal-cell-ranges.md).
    MovePrefixUnchecked,
    /// Proved: the source's (receiver's) cells `0..count` hold values and the target's cells
    /// `at..at + count` are empty and within it. The values are copied, which only a Copy element
    /// allows; the source is unchanged. `index` is the count; `value` is `(target, at)` as an
    /// aggregate of the two, which no code evaluates as one.
    CopyIntoUnchecked,
    /// Proved: the receiver's cells `0..count` all hold values and `count` fits it; they are left
    /// empty, the rest unchanged. Only a Copy element allows it (nothing is lost). `index` is the
    /// count (docs/proposal-cell-ranges.md).
    TakePrefixUnchecked,
    /// Proved: the receiver's cells `0..count` are all empty and `count` fits it; afterwards each
    /// holds `value`, the rest unchanged. Only a Copy element allows it (one value, many cells).
    /// `index` is the count (docs/proposal-cell-ranges.md).
    FillUnchecked,
    /// Proved: `count <= 8`, the receiver's cells `at..at + count` are empty and within it. They
    /// are written from the low `count` bytes of `word`, lowest first; the rest is unchanged. A
    /// `Buffer<U8>` only. `index` is `at`; `value` is `(word, count)` as an aggregate of the two,
    /// which no code evaluates as one (docs/proposal-cell-ranges.md).
    InitWordUnchecked,
    /// Initialize a vacant range from a shared slice of the same numeric element type.
    InitSliceUnchecked,
    /// Proved: `count <= 8` and the receiver's cells `at..at + count` all hold values, within it.
    /// Their bytes as one U64, lowest first, the rest 0. A `Buffer<U8>` only; nothing changes.
    /// `index` is `at`; `value` is the count.
    ReadWordUnchecked }
impl StorageOp {
    /// Whether the operation is admitted by a proof rather than checked at run time: only a
    /// function whose contract the memory model proves may use it. Every operation is named here,
    /// with no catch-all, so a new one cannot be added without saying which it is (review: three
    /// range operations were missing from the lists this replaced and were accepted unproved).
    pub(super) fn proof_admitted(self) -> bool {
        match self {
            StorageOp::RefLink | StorageOp::MutLink | StorageOp::GetUnchecked | StorageOp::RefUnchecked | StorageOp::MutUnchecked | StorageOp::InitUnchecked
            | StorageOp::TakeUnchecked | StorageOp::ReplaceUnchecked | StorageOp::SliceUnchecked | StorageOp::MovePrefixUnchecked
            | StorageOp::CopyIntoUnchecked | StorageOp::TakePrefixUnchecked | StorageOp::FillUnchecked
            | StorageOp::InitWordUnchecked | StorageOp::InitSliceUnchecked | StorageOp::ReadWordUnchecked => true,
            StorageOp::Init | StorageOp::Read | StorageOp::Take | StorageOp::Swap | StorageOp::Initialized
            | StorageOp::Ref | StorageOp::TryInit => false,
        }
    }
}


/// A nullable owning pointer. Only allocation creates a non-null owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoxOp { New, Address, Ref, Mut, Take, EndNull, Lend, LendMut, Weaken, FromBuffer, WeakBits, Follow, FollowMut, FollowLoan, FollowLoanMut, Enclosing }
impl BoxOp {
    pub fn proved(self) -> bool { matches!(self, Self::EndNull | Self::Lend | Self::LendMut | Self::Follow | Self::FollowMut | Self::FollowLoan | Self::FollowLoanMut | Self::Enclosing) }
}
