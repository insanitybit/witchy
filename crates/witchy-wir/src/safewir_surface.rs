//! Untrusted executable-side claims for semantic SafeWIR memory operations.
//!
//! These types bridge mutable, expression-shaped WIR and the analysis-only
//! [`crate::safewir`] facts. Lowering supplies both the executable address form
//! and the object/projection/range claim. The final adapter assigns
//! `DefinitionId`s from the actual def-use graph and checks the redundant
//! executable form before any encoder authority can exist. Constructing these
//! values alone grants no validation or encoding authority.
//!
//! [`SafeDynamicHeaderBuilder`] is the sole migration bootstrap for reading a
//! canonical dynamic buffer/list header before its exact count is available.
//! It deliberately emits `SafeExtentExpr::Unknown` only for that fixed header
//! projection, and it emits read access without accepting caller-selected
//! access authority. The final adapter/validator must recognize that exact
//! builder shape, bind the fresh load result, and require subsequent payload
//! access to use [`SafeExactExtent::counted`] with the bound executable count
//! identity.



use crate::layout::LayoutId;
use crate::safewir::AccessMode;
use crate::wir::WirExpr;

mod builders;
pub use builders::*;

/// Maximum static prefix length for `prefix + i64` scratch formatting. Twenty
/// decimal digits plus one sign byte must fit without wrapping Wasm's full
/// unsigned i32 address domain.
pub const MAX_SCRATCH_FORMATTER_PREFIX_LENGTH: u32 = u32::MAX - 21;

pub const fn scratch_formatter_prefix_is_nonwrapping(length: u32) -> bool {
    length <= MAX_SCRATCH_FORMATTER_PREFIX_LENGTH
}

#[derive(Debug, Clone)]
pub enum SafeSizeExpr {
    Constant(u64),
    Value(Box<WirExpr>),
    Add(Box<Self>, Box<Self>),
    Multiply(Box<Self>, Box<Self>),
    NarrowU32(Box<Self>),
}

impl SafeSizeExpr {
    pub(crate) fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        match self {
            Self::Constant(_) => {}
            Self::Value(value) => visit(value),
            Self::Add(left, right) | Self::Multiply(left, right) => {
                left.for_each_wir_expr(visit);
                right.for_each_wir_expr(visit);
            }
            Self::NarrowU32(value) => value.for_each_wir_expr(visit),
        }
    }

    pub(crate) fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        match self {
            Self::Constant(_) => {}
            Self::Value(value) => visit(value),
            Self::Add(left, right) | Self::Multiply(left, right) => {
                left.for_each_wir_expr_mut(visit);
                right.for_each_wir_expr_mut(visit);
            }
            Self::NarrowU32(value) => value.for_each_wir_expr_mut(visit),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SafeExtentExpr {
    Unknown,
    Exact(SafeSizeExpr),
    /// A canonical closed-sum descriptor with no candidate-selected byte
    /// extent. The final validator may derive a selected live extent only from
    /// authenticated discriminant evidence.
    ClosedSum,
    Counted {
        header: SafeSizeExpr,
        count: Box<WirExpr>,
        stride: SafeSizeExpr,
    },
}

impl SafeExtentExpr {
    pub(crate) fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        match self {
            Self::Unknown | Self::ClosedSum => {}
            Self::Exact(size) => size.for_each_wir_expr(visit),
            Self::Counted { header, count, stride } => {
                header.for_each_wir_expr(visit);
                visit(count);
                stride.for_each_wir_expr(visit);
            }
        }
    }

    pub(crate) fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        match self {
            Self::Unknown | Self::ClosedSum => {}
            Self::Exact(size) => size.for_each_wir_expr_mut(visit),
            Self::Counted { header, count, stride } => {
                header.for_each_wir_expr_mut(visit);
                visit(count);
                stride.for_each_wir_expr_mut(visit);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum SafeProjectionStep {
    Header {
        component: u32,
    },
    /// The descriptor-defined closed-sum discriminant. Construction carries no
    /// evidence about the currently selected variant.
    SumTag,
    /// One descriptor-defined field in one compile-time variant. The ordinal
    /// identifies physical metadata only; it does not assert that the variant
    /// is currently selected.
    VariantField {
        variant: u32,
        field: u32,
    },
    Field {
        index: u32,
    },
    Element {
        index: Box<WirExpr>,
    },
    /// One slot of one section of a SECTION-MAJOR layout. The descriptor's
    /// section table supplies the section's base and element width; the ordinal
    /// identifies which section, and carries no claim about the slot's contents.
    Section {
        section: u32,
        index: Box<WirExpr>,
    },
    /// Untrusted marker for a pure executable i32 index expression. This is a
    /// transport shape only: construction does not prove the expression is in
    /// range, non-wrapping, or related to the object's live length. The final
    /// adapter must bind every leaf to its exact reaching definition and the
    /// validator must authenticate the successful control-flow edge.
    DerivedElement {
        index: Box<WirExpr>,
    },
    Slice {
        start: Box<WirExpr>,
        length: Box<WirExpr>,
    },
    /// Untrusted marker for one scratch-relative cursor. The executable address
    /// remains `root + cursor`; only the final-WIR adapter can authenticate the
    /// relation, so this syntax grants no absolute-address authority.
    DerivedScratchElement {
        cursor: Box<WirExpr>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct SafeProjectionExpr {
    steps: Vec<SafeProjectionStep>,
    /// Sealed, non-authorizing marker for numeric formatting's absolute
    /// backward cursor. Cursor/count expressions deliberately remain in the
    /// ordinary object/range/encoded fields so generic WIR and reaching-def
    /// traversal sees each executable operand without learning new authority.
    numeric_backward_cursor: bool,
}

impl SafeProjectionExpr {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn from_steps(steps: impl IntoIterator<Item = SafeProjectionStep>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            numeric_backward_cursor: false,
        }
    }

    pub fn numeric_backward_cursor() -> Self {
        Self {
            steps: Vec::new(),
            numeric_backward_cursor: true,
        }
    }

    pub fn is_numeric_backward_cursor(&self) -> bool {
        self.numeric_backward_cursor
    }

    pub fn steps(&self) -> &[SafeProjectionStep] {
        &self.steps
    }

    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        for step in &self.steps {
            match step {
                SafeProjectionStep::Header { .. }
                | SafeProjectionStep::SumTag
                | SafeProjectionStep::VariantField { .. }
                | SafeProjectionStep::Field { .. } => {}
                SafeProjectionStep::Element { index }
                | SafeProjectionStep::Section { index, .. }
                | SafeProjectionStep::DerivedElement { index } => visit(index),
                SafeProjectionStep::Slice { start, length } => {
                    visit(start);
                    visit(length);
                }
                SafeProjectionStep::DerivedScratchElement { cursor } => visit(cursor),
            }
        }
    }

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        for step in &mut self.steps {
            match step {
                SafeProjectionStep::Header { .. }
                | SafeProjectionStep::SumTag
                | SafeProjectionStep::VariantField { .. }
                | SafeProjectionStep::Field { .. } => {}
                SafeProjectionStep::Element { index }
                | SafeProjectionStep::Section { index, .. }
                | SafeProjectionStep::DerivedElement { index } => visit(index),
                SafeProjectionStep::Slice { start, length } => {
                    visit(start);
                    visit(length);
                }
                SafeProjectionStep::DerivedScratchElement { cursor } => visit(cursor),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum SafeProvenanceExpr {
    Fresh,
    Aliases {
        object: Box<WirExpr>,
        projection: SafeProjectionExpr,
    },
    MayAlias(Vec<WirExpr>),
    StaticData {
        object: Box<WirExpr>,
    },
    Unknown,
}

impl SafeProvenanceExpr {
    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        match self {
            Self::Fresh | Self::Unknown => {}
            Self::Aliases { object, projection } => {
                visit(object);
                projection.for_each_wir_expr(visit);
            }
            Self::MayAlias(objects) => objects.iter().for_each(visit),
            Self::StaticData { object } => visit(object),
        }
    }

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        match self {
            Self::Fresh | Self::Unknown => {}
            Self::Aliases { object, projection } => {
                visit(object);
                projection.for_each_wir_expr_mut(visit);
            }
            Self::MayAlias(objects) => objects.iter_mut().for_each(visit),
            Self::StaticData { object } => visit(object),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SafeObjectExpr {
    /// Runtime pointer expression naming the object root.
    pub root: Box<WirExpr>,
    pub layout: LayoutId,
    pub extent: SafeExtentExpr,
    pub provenance: SafeProvenanceExpr,
}

impl SafeObjectExpr {
    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        visit(&self.root);
        self.extent.for_each_wir_expr(&mut visit);
        self.provenance.for_each_wir_expr(&mut visit);
    }

    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        visit(&mut self.root);
        self.extent.for_each_wir_expr_mut(&mut visit);
        self.provenance.for_each_wir_expr_mut(&mut visit);
    }
}

#[derive(Debug, Clone)]
pub struct SafeRangeExpr {
    /// Redundant object-root claim. The adapter compares it with
    /// [`SafeObjectExpr::root`] after assigning reaching definitions.
    pub object_root: Box<WirExpr>,
    pub start: SafeSizeExpr,
    pub length: SafeSizeExpr,
}

#[derive(Debug, Clone)]
pub struct SafePlaceExpr {
    pub object: SafeObjectExpr,
    pub projection: SafeProjectionExpr,
    pub range: SafeRangeExpr,
    pub access: AccessMode,
    /// Exact executable address operand retained for byte-for-byte-compatible
    /// erasure. It is an untrusted claim that validation must prove equivalent
    /// to `object + projection`.
    pub encoded_base: Box<WirExpr>,
    /// Wasm load/store immediate offset retained separately so erasure does not
    /// introduce an `i32.add` or change optimizer-visible instruction shape.
    pub encoded_offset: u32,
}

impl SafePlaceExpr {
    /// Visit every executable WIR expression carried by this untrusted claim.
    /// Consumers must recurse into each expression with their normal policy.
    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        visit(&self.object.root);
        self.object.extent.for_each_wir_expr(&mut visit);
        self.object.provenance.for_each_wir_expr(&mut visit);
        self.projection.for_each_wir_expr(&mut visit);
        visit(&self.range.object_root);
        self.range.start.for_each_wir_expr(&mut visit);
        self.range.length.for_each_wir_expr(&mut visit);
        visit(&self.encoded_base);
    }

    /// Mutable counterpart to [`Self::for_each_wir_expr`].
    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        visit(&mut self.object.root);
        self.object.extent.for_each_wir_expr_mut(&mut visit);
        self.object.provenance.for_each_wir_expr_mut(&mut visit);
        self.projection.for_each_wir_expr_mut(&mut visit);
        visit(&mut self.range.object_root);
        self.range.start.for_each_wir_expr_mut(&mut visit);
        self.range.length.for_each_wir_expr_mut(&mut visit);
        visit(&mut self.encoded_base);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeCopyKind {
    Disjoint,
    MayOverlap,
}
