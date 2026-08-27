//! Untrusted executable-side claims for semantic SafeWIR memory operations.
//!
//! These types bridge mutable, expression-shaped WIR and the analysis-only
//! [`crate::safewir`] facts. Lowering supplies both the executable address form
//! and the object/projection/range claim. The final adapter assigns
//! `DefinitionId`s from the actual def-use graph and checks the redundant
//! executable form before any encoder authority can exist. Constructing these
//! values alone grants no validation or encoding authority.

use crate::layout::LayoutId;
use crate::safewir::AccessMode;
use crate::wir::WirExpr;

#[derive(Debug, Clone)]
pub enum SafeSizeExpr {
    Constant(u64),
    Value(Box<WirExpr>),
    Add(Box<Self>, Box<Self>),
    Multiply(Box<Self>, Box<Self>),
    NarrowU32(Box<Self>),
}

impl SafeSizeExpr {
    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
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

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
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
    Counted {
        header: SafeSizeExpr,
        count: Box<WirExpr>,
        stride: SafeSizeExpr,
    },
}

impl SafeExtentExpr {
    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        match self {
            Self::Unknown => {}
            Self::Exact(size) => size.for_each_wir_expr(visit),
            Self::Counted { header, count, stride } => {
                header.for_each_wir_expr(visit);
                visit(count);
                stride.for_each_wir_expr(visit);
            }
        }
    }

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        match self {
            Self::Unknown => {}
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
    Field {
        index: u32,
    },
    Element {
        index: Box<WirExpr>,
    },
    Slice {
        start: Box<WirExpr>,
        length: Box<WirExpr>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct SafeProjectionExpr(Vec<SafeProjectionStep>);

impl SafeProjectionExpr {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn from_steps(steps: impl IntoIterator<Item = SafeProjectionStep>) -> Self {
        Self(steps.into_iter().collect())
    }

    pub fn steps(&self) -> &[SafeProjectionStep] {
        &self.0
    }

    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        for step in &self.0 {
            match step {
                SafeProjectionStep::Header { .. } | SafeProjectionStep::Field { .. } => {}
                SafeProjectionStep::Element { index } => visit(index),
                SafeProjectionStep::Slice { start, length } => {
                    visit(start);
                    visit(length);
                }
            }
        }
    }

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        for step in &mut self.0 {
            match step {
                SafeProjectionStep::Header { .. } | SafeProjectionStep::Field { .. } => {}
                SafeProjectionStep::Element { index } => visit(index),
                SafeProjectionStep::Slice { start, length } => {
                    visit(start);
                    visit(length);
                }
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

    #[test]
    fn ordinary_encoder_rejects_semantic_memory_without_authority() {
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
