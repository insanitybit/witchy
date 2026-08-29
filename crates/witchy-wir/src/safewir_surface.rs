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

#[cfg(test)]
mod scratch_formatter_range_tests {
    use super::{
        MAX_SCRATCH_FORMATTER_PREFIX_LENGTH, scratch_formatter_prefix_is_nonwrapping,
    };

    #[test]
    fn scratch_formatter_uses_the_full_unsigned_i32_address_domain() {
        assert_eq!(MAX_SCRATCH_FORMATTER_PREFIX_LENGTH, u32::MAX - 21);
        assert!(scratch_formatter_prefix_is_nonwrapping(u32::MAX - 21));
        assert!(!scratch_formatter_prefix_is_nonwrapping(u32::MAX - 20));
    }
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

#[cfg(test)]
mod tests {

    // RFC-0162: the numeric backward cursor is a sealed, non-authorizing
    // marker. It must never be inferred from projection steps, and it must
    // never travel through the ordinary `from_steps` construction path.
    #[test]
    fn numeric_backward_cursor_marker_is_sealed() {
        let root = SafeProjectionExpr::root();
        assert!(!root.is_numeric_backward_cursor());
        assert!(root.steps().is_empty());

        let marked = SafeProjectionExpr::numeric_backward_cursor();
        assert!(marked.is_numeric_backward_cursor());
        assert!(
            marked.steps().is_empty(),
            "the marker carries no projection authority",
        );

        // Ordinary construction never sets the marker, whatever the steps.
        let ordinary = SafeProjectionExpr::from_steps([
            SafeProjectionStep::Field { index: 0 },
            SafeProjectionStep::SumTag,
        ]);
        assert!(
            !ordinary.is_numeric_backward_cursor(),
            "from_steps must not infer the sealed marker",
        );
        assert_eq!(ordinary.steps().len(), 2);
    }
    use super::*;
    use crate::safewir::AccessMode;
    use crate::wir::{Kind, WirFunc, WirLocal, WirModule, WirNode, WirTy, to_wat};

    fn place_with_root(
        access: AccessMode,
        encoded_offset: u32,
        root: WirExpr,
    ) -> Box<SafePlaceExpr> {
        Box::new(SafePlaceExpr {
            object: SafeObjectExpr {
                root: Box::new(root.clone()),
                layout: LayoutId::from_bytes([7; 32]),
                extent: SafeExtentExpr::Exact(SafeSizeExpr::Constant(64)),
                provenance: SafeProvenanceExpr::Unknown,
            },
            projection: SafeProjectionExpr::root(),
            range: SafeRangeExpr {
                object_root: Box::new(root.clone()),
                start: SafeSizeExpr::Constant(u64::from(encoded_offset)),
                length: SafeSizeExpr::Constant(64 - u64::from(encoded_offset)),
            },
            access,
            encoded_base: Box::new(root),
            encoded_offset,
        })
    }

    fn place(access: AccessMode, encoded_offset: u32) -> Box<SafePlaceExpr> {
        place_with_root(
            access,
            encoded_offset,
            WirExpr::GetLocal("object".into()),
        )
    }

    fn module(body: Vec<WirNode>) -> WirModule {
        WirModule {
            imports: vec![],
            funcs: vec![WirFunc {
                name: "touch".into(),
                params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
                ret: vec![],
                locals: vec![],
                body,
                raw_body: None,
            }],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![],
        }
    }

    #[test]
    fn executable_claim_carries_no_validated_authority() {
        let root = WirExpr::GetLocal("object".into());
        let claim = SafePlaceExpr {
            object: SafeObjectExpr {
                root: Box::new(root.clone()),
                layout: LayoutId::from_bytes([7; 32]),
                extent: SafeExtentExpr::Exact(SafeSizeExpr::Constant(8)),
                provenance: SafeProvenanceExpr::Unknown,
            },
            projection: SafeProjectionExpr::root(),
            range: SafeRangeExpr {
                object_root: Box::new(root.clone()),
                start: SafeSizeExpr::Constant(0),
                length: SafeSizeExpr::Constant(8),
            },
            access: AccessMode::Read,
            encoded_base: Box::new(root),
            encoded_offset: 0,
        };
        assert_eq!(claim.projection.steps().len(), 0);
        assert!(matches!(claim.object.provenance, SafeProvenanceExpr::Unknown));
    }

    #[test]
    fn semantic_memory_operations_have_the_exact_raw_wat_shape() {
        let raw = module(vec![
            WirNode::Drop(WirExpr::Load {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                kind: Kind::I32,
                offset: 4,
            }),
            WirNode::Store {
                ptr: WirExpr::GetLocal("object".into()),
                value: WirExpr::ConstI32(9),
                kind: Kind::I32,
                offset: 8,
            },
            WirNode::Drop(WirExpr::Load8U {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                offset: 3,
            }),
            WirNode::Store8 {
                ptr: WirExpr::GetLocal("object".into()),
                value: WirExpr::ConstI32(1),
                offset: 2,
            },
            WirNode::MemoryCopy {
                dest: WirExpr::GetLocal("object".into()),
                src: WirExpr::GetLocal("object".into()),
                len: WirExpr::ConstI32(4),
            },
            WirNode::MemoryFill {
                dest: WirExpr::GetLocal("object".into()),
                value: WirExpr::ConstI32(0),
                len: WirExpr::ConstI32(4),
            },
        ]);
        let semantic = module(vec![
            WirNode::Drop(WirExpr::SafeLoad {
                place: place(AccessMode::Read, 4),
                kind: Kind::I32,
            }),
            WirNode::SafeStore {
                place: place(AccessMode::Write, 8),
                value: WirExpr::ConstI32(9),
                kind: Kind::I32,
            },
            WirNode::Drop(WirExpr::SafeLoad8U {
                place: place(AccessMode::Read, 3),
            }),
            WirNode::SafeStore8 {
                place: place(AccessMode::Write, 2),
                value: WirExpr::ConstI32(1),
            },
            WirNode::SafeMemoryCopy {
                destination: place(AccessMode::Write, 0),
                source: place(AccessMode::Read, 0),
                len: WirExpr::ConstI32(4),
                kind: SafeCopyKind::MayOverlap,
            },
            WirNode::SafeMemoryFill {
                destination: place(AccessMode::Write, 0),
                value: WirExpr::ConstI32(0),
                len: WirExpr::ConstI32(4),
            },
        ]);

        assert_eq!(to_wat(&semantic), to_wat(&raw));
    }

    // RFC-0162: the 584-site migration can only proceed one subsystem at a time
    // if a PARTIALLY migrated module still encodes. Validation forbids raw
    // memory and the ordinary encoder forbids SafeWIR, so without the bridge a
    // module holding one migrated site and one unmigrated site would encode
    // through neither, and all 584 would have to move at once.
    #[test]
    fn migration_bridge_encodes_a_module_that_is_part_raw_and_part_safewir() {
        let mixed = module(vec![
            // an already-migrated site
            WirNode::Drop(WirExpr::SafeLoad {
                place: place(AccessMode::Read, 0),
                kind: Kind::I32,
            }),
            // a site not migrated yet
            WirNode::Drop(WirExpr::Load {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                kind: Kind::I32,
                offset: 0,
            }),
        ]);
        let encoded = crate::wir_encode::try_encode_with_gc_source_map(&mixed, &[], &[])
            .expect("a partially migrated module must still encode");
        assert!(!encoded.wasm.is_empty());

        // And the migrated site erases to exactly the raw operation it denotes,
        // so migrating a site does not change the emitted program.
        let all_raw = module(vec![
            WirNode::Drop(WirExpr::Load {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                kind: Kind::I32,
                offset: 0,
            }),
            WirNode::Drop(WirExpr::Load {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                kind: Kind::I32,
                offset: 0,
            }),
        ]);
        let raw_encoded = crate::wir_encode::try_encode_with_gc_source_map(&all_raw, &[], &[])
            .expect("the all-raw module encodes");
        assert_eq!(
            encoded.wasm, raw_encoded.wasm,
            "migrating a site must not change the emitted bytes",
        );

        // Pin byte identity against the PRE-BRIDGE implementation, which still
        // exists as the test-only `try_encode_with_gc`. Comparing the bridge
        // only against itself would not catch erasure drifting away from what
        // the raw encoder emits.
        let pre_bridge = crate::wir_encode::try_encode_with_gc(&all_raw, &[], &[])
            .expect("the all-raw module encodes on the pre-bridge path");
        assert_eq!(
            raw_encoded.wasm, pre_bridge,
            "the bridge must be byte-identity for an all-raw module",
        );
    }

    // `encoded_offset` is the field most likely to be dropped or normalized by
    // a future refactor of erasure, and every other bridge fixture uses zero.
    #[test]
    fn migration_bridge_preserves_a_nonzero_encoded_offset() {
        let mut place = place(AccessMode::Read, 0);
        place.encoded_offset = 12;
        let semantic = module(vec![WirNode::Drop(WirExpr::SafeLoad {
            place,
            kind: Kind::I32,
        })]);
        let raw = module(vec![WirNode::Drop(WirExpr::Load {
            ptr: Box::new(WirExpr::GetLocal("object".into())),
            kind: Kind::I32,
            offset: 12,
        })]);
        let erased = crate::wir_encode::try_encode_with_gc_source_map(&semantic, &[], &[])
            .expect("a nonzero offset erases");
        let expected = crate::wir_encode::try_encode_with_gc_source_map(&raw, &[], &[])
            .expect("the raw equivalent encodes");
        assert_eq!(
            erased.wasm, expected.wasm,
            "erasure must carry the load immediate offset through unchanged",
        );
    }

    // NOTE: this covers `try_encode_with_gc`, which is `#[cfg(any(test,
    // raw-encoder-test-api))]` and unreachable from a normal build. It does NOT
    // cover the production entrypoint `try_encode_with_gc_source_map`, which
    // during the RFC-0162 migration window deliberately ERASES SafeWIR instead
    // of rejecting it. No test pins a rejection on the production path, because
    // there is currently no rejection there to pin.
    #[test]
    fn test_only_raw_encoder_rejects_semantic_memory_without_authority() {
        let semantic = module(vec![WirNode::Drop(WirExpr::SafeLoad {
            place: place(AccessMode::Read, 0),
            kind: Kind::I32,
        })]);

        let error = crate::wir_encode::try_encode_with_gc(&semantic, &[], &[])
            .expect_err("an unvalidated semantic operation must not encode");
        assert!(error.message.contains("validated encoder authority"), "{error}");
    }

    #[test]
    fn direct_inlining_substitutes_every_safe_place_parameter_reference() {
        let read = WirFunc {
            name: "$read".into(),
            params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::SafeLoad {
                place: place(AccessMode::Read, 0),
                kind: Kind::I32,
            }))],
            raw_body: None,
        };
        let caller = WirFunc {
            name: "main".into(),
            params: vec![WirLocal { name: "caller_object".into(), ty: WirTy::Str }],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Call {
                func: "$read".into(),
                args: vec![WirExpr::GetLocal("caller_object".into())],
            }))],
            raw_body: None,
        };
        let mut candidate = module(vec![]);
        candidate.funcs = vec![read, caller];

        crate::wir_opt::inline_direct_calls(&mut candidate);

        assert_eq!(candidate.funcs.len(), 1, "the inlined helper should be pruned");
        let WirNode::Return(Some(WirExpr::SafeLoad { place, .. })) =
            &candidate.funcs[0].body[0]
        else {
            panic!("the caller should contain the inlined semantic load");
        };
        place.for_each_wir_expr(|expr| {
            assert!(
                !matches!(expr, WirExpr::GetLocal(name) if name == "object"),
                "callee parameter survived substitution: {expr:?}"
            );
        });
        assert!(matches!(
            place.encoded_base.as_ref(),
            WirExpr::GetLocal(name) if name == "caller_object"
        ));
    }

    #[test]
    fn pruning_retains_a_large_helper_called_only_from_a_safe_place() {
        let mut helper_body = vec![WirNode::Drop(WirExpr::ConstI32(0)); 50];
        helper_body.push(WirNode::Return(Some(WirExpr::GetLocal("object".into()))));
        let address = WirFunc {
            name: "$address".into(),
            params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
            ret: vec![WirTy::Str],
            locals: vec![],
            body: helper_body,
            raw_body: None,
        };
        let caller = WirFunc {
            name: "main".into(),
            params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::SafeLoad {
                place: place_with_root(
                    AccessMode::Read,
                    0,
                    WirExpr::Call {
                        func: "$address".into(),
                        args: vec![WirExpr::GetLocal("object".into())],
                    },
                ),
                kind: Kind::I32,
            })],
            raw_body: None,
        };
        let mut candidate = module(vec![]);
        candidate.funcs = vec![address, caller];

        crate::wir_opt::inline_direct_calls(&mut candidate);

        assert!(candidate.funcs.iter().any(|function| function.name == "$address"));
    }

    #[test]
    fn call_summary_preserves_semantic_load_result_kind() {
        let expression = WirExpr::SafeLoad {
            place: place(AccessMode::Read, 0),
            kind: Kind::I32,
        };
        assert_eq!(crate::wir_opt::call_summary::infer_expr_kind(&expression), Kind::I32);
    }
}
