//! Opaque RFC-0162 memory-kernel operations carried through candidate WIR.
//!
//! These operations expose only their executable operands and untrusted ghost
//! claims. They deliberately contain no [`crate::wir::WirFunc`], raw body, or
//! implementation sequence: final validation must check every claim, and only
//! the future validated encoder may select the corresponding fixed template.

use crate::layout::LayoutId;
use crate::safewir_surface::{SafeExtentExpr, SafeObjectExpr, SafeSizeExpr};
use crate::wir::WirExpr;

/// The fixed allocator template selected after validation.
///
/// This choice is explicit candidate data rather than an encoder heuristic.
/// Validation must prove that the chosen representation agrees with the
/// object's canonical layout and ownership contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeAllocationClass {
    /// Header-free allocation from the monotonic heap frontier.
    Bump,
    /// Reference-counted allocation, including size-classed free-list reuse.
    ReferenceCounted,
}

/// Closed identity of every RFC-0162 privileged kernel operation.
///
/// This is an audit identity, not encoder authority. Keeping the census next
/// to the candidate operations makes additions fail exhaustive matches in the
/// validator, template contract, and tests instead of silently inheriting a
/// nearby template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SafeKernelOperation {
    RegionMark,
    ScratchMark,
    AllocateBump,
    AllocateReferenceCounted,
    ScratchAllocate,
    Retain,
    Reuse,
    RegionRewindIteration,
    RegionRewindLexical,
    RegionRewindCopyOut,
    ScratchRewind,
    Release,
    Reclaim,
}

pub const SAFE_KERNEL_OPERATION_CENSUS: [SafeKernelOperation; 13] = [
    SafeKernelOperation::RegionMark,
    SafeKernelOperation::ScratchMark,
    SafeKernelOperation::AllocateBump,
    SafeKernelOperation::AllocateReferenceCounted,
    SafeKernelOperation::ScratchAllocate,
    SafeKernelOperation::Retain,
    SafeKernelOperation::Reuse,
    SafeKernelOperation::RegionRewindIteration,
    SafeKernelOperation::RegionRewindLexical,
    SafeKernelOperation::RegionRewindCopyOut,
    SafeKernelOperation::ScratchRewind,
    SafeKernelOperation::Release,
    SafeKernelOperation::Reclaim,
];

impl SafeKernelOperation {
    pub const fn name(self) -> &'static str {
        match self {
            Self::RegionMark => "region.mark",
            Self::ScratchMark => "scratch.mark",
            Self::AllocateBump => "allocate.bump",
            Self::AllocateReferenceCounted => "allocate.rc",
            Self::ScratchAllocate => "scratch.allocate",
            Self::Retain => "retain",
            Self::Reuse => "reuse",
            Self::RegionRewindIteration => "region.rewind.iteration",
            Self::RegionRewindLexical => "region.rewind.lexical",
            Self::RegionRewindCopyOut => "region.rewind.copy_out",
            Self::ScratchRewind => "scratch.rewind",
            Self::Release => "release",
            Self::Reclaim => "reclaim",
        }
    }
}

/// Untrusted description of the object produced by allocation or reuse.
///
/// Definition and generation identities are intentionally absent. The final
/// validator assigns them from the actual result site; a producer cannot mint
/// either authority by constructing this claim. For allocation and reuse, the
/// result extent is the exact live object extent, while
/// [`SafeKernelSize::checked`] is the allocator request, distinct from both
/// the live extent and reusable body capacity tracked after adaptation.
/// Validation must prove that the request is the exact executable allocation
/// size. Diagnostic padding must never become reusable or object authority.
#[derive(Debug, Clone)]
pub struct SafeKernelResultClaim {
    pub layout: LayoutId,
    pub extent: SafeExtentExpr,
}

impl SafeKernelResultClaim {
    pub(crate) fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        self.extent.for_each_wir_expr(visit);
    }

    pub(crate) fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        self.extent.for_each_wir_expr_mut(visit);
    }
}

/// Untrusted pair binding the mathematical allocator request checked by
/// SafeWIR to the one executable i32 expression consumed by the fixed kernel
/// template.
///
/// The validator assigns reaching definitions to every value embedded in
/// `checked`, proves its arithmetic non-wrapping before `NarrowU32`, and then
/// proves that `encoded` is the exact narrowed result. The encoder evaluates
/// only `encoded`; constructing this pair grants no size authority. Generic
/// WIR transforms deliberately visit both sides so definition rewrites cannot
/// make them stale. Until validation proves that the checked leaves are pure
/// scalar definitions and rejects every checked-only runtime dependency, that
/// traversal may conservatively retain locals, calls, specialization targets,
/// or effects. A validated claim must therefore make erasing `checked`
/// incapable of changing executable locals, call edges, or specialization.
#[derive(Debug, Clone)]
pub struct SafeKernelSize {
    pub checked: SafeSizeExpr,
    pub encoded: Box<WirExpr>,
}

impl SafeKernelSize {
    pub(crate) fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        self.checked.for_each_wir_expr(visit);
        visit(&self.encoded);
    }

    pub(crate) fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        self.checked.for_each_wir_expr_mut(visit);
        visit(&mut self.encoded);
    }
}

/// Value-producing opaque kernel operations. Every variant produces an `i32`
/// carrier, but validation distinguishes object roots from region and scratch
/// watermark authority. Ghost result claims are erased only by the validated
/// encoder.
#[derive(Debug, Clone)]
pub enum SafeKernelExpr {
    /// Capture the current allocation frontier as an opaque region watermark.
    ///
    /// The returned `i32` is not an object pointer. Final validation must bind
    /// every use to this exact result definition and enforce lexical/LIFO use;
    /// an arbitrary scalar is never valid watermark authority.
    RegionMark,
    /// Capture the authenticated bounded-scratch frontier. The result is a
    /// scalar watermark in a domain disjoint from [`Self::RegionMark`].
    ScratchMark,
    /// Allocate the checked size using one fixed allocator template.
    Allocate {
        class: SafeAllocationClass,
        result: SafeKernelResultClaim,
        size: SafeKernelSize,
    },
    /// Reserve one exact borrowed byte range in the bounded scratch arena.
    /// Validation reconciles all three size descriptions and binds the result
    /// to the current scratch scope; construction alone grants no authority.
    ScratchAllocate {
        result: SafeKernelResultClaim,
        size: SafeKernelSize,
    },
    /// Add one shared owner and return the unchanged object pointer.
    Retain {
        object: SafeObjectExpr,
    },
    /// Destructively reuse uniquely owned storage for a new object generation.
    Reuse {
        object: SafeObjectExpr,
        result: SafeKernelResultClaim,
        size: SafeKernelSize,
    },
}

impl SafeKernelExpr {
    pub const fn operation(&self) -> SafeKernelOperation {
        match self {
            Self::RegionMark => SafeKernelOperation::RegionMark,
            Self::ScratchMark => SafeKernelOperation::ScratchMark,
            Self::Allocate { class: SafeAllocationClass::Bump, .. } => {
                SafeKernelOperation::AllocateBump
            }
            Self::Allocate { class: SafeAllocationClass::ReferenceCounted, .. } => {
                SafeKernelOperation::AllocateReferenceCounted
            }
            Self::ScratchAllocate { .. } => SafeKernelOperation::ScratchAllocate,
            Self::Retain { .. } => SafeKernelOperation::Retain,
            Self::Reuse { .. } => SafeKernelOperation::Reuse,
        }
    }

    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        match self {
            Self::RegionMark | Self::ScratchMark => {}
            Self::Allocate { result, size, .. } => {
                result.for_each_wir_expr(&mut visit);
                size.for_each_wir_expr(&mut visit);
            }
            Self::ScratchAllocate { result, size } => {
                result.for_each_wir_expr(&mut visit);
                size.for_each_wir_expr(&mut visit);
            }
            Self::Retain { object } => object.for_each_wir_expr(&mut visit),
            Self::Reuse { object, result, size } => {
                object.for_each_wir_expr(&mut visit);
                result.for_each_wir_expr(&mut visit);
                size.for_each_wir_expr(&mut visit);
            }
        }
    }

    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        match self {
            Self::RegionMark | Self::ScratchMark => {}
            Self::Allocate { result, size, .. } => {
                result.for_each_wir_expr_mut(&mut visit);
                size.for_each_wir_expr_mut(&mut visit);
            }
            Self::ScratchAllocate { result, size } => {
                result.for_each_wir_expr_mut(&mut visit);
                size.for_each_wir_expr_mut(&mut visit);
            }
            Self::Retain { object } => object.for_each_wir_expr_mut(&mut visit),
            Self::Reuse { object, result, size } => {
                object.for_each_wir_expr_mut(&mut visit);
                result.for_each_wir_expr_mut(&mut visit);
                size.for_each_wir_expr_mut(&mut visit);
            }
        }
    }
}

/// The three fixed region-exit templates used by current lowering.
///
/// Keeping these shapes distinct prevents an untrusted producer from toggling
/// allocator bookkeeping with booleans. The checked adapter must prove that
/// the selected shape matches the source construct and its copy-out path.
/// Telemetry remains ordinary WIR outside these authority-bearing operations,
/// preserving loop lowering's hot-local counter aggregation.
#[derive(Debug, Clone)]
pub enum SafeRegionRewind {
    /// End one loop iteration. Per-iteration RC reclamation is suppressed, so
    /// the pre-existing free list remains valid and must be preserved. Final
    /// validation must re-prove that no operation in the region linked a new
    /// entry into that list.
    Iteration {
        watermark: Box<WirExpr>,
    },
    /// Exit a scalar-valued lexical region. Everything above the watermark is
    /// reclaimed and the possibly dangling free list is cleared.
    Lexical {
        watermark: Box<WirExpr>,
    },
    /// Exit a pointer-valued lexical region by sliding the retained result to
    /// the watermark. Keeping the copy inside
    /// this operation preserves the checked-heap reclaim-before-copy order.
    /// Final validation must prove non-overflowing bounds
    /// `watermark <= source <= source + retained_bytes <= current_heap`, bind
    /// the copied bytes to the one escaping result object, and invalidate all
    /// other object generations allocated at or above the watermark.
    CopyOut {
        watermark: Box<WirExpr>,
        source: Box<WirExpr>,
        retained_bytes: Box<WirExpr>,
    },
}

impl SafeRegionRewind {
    fn for_each_wir_expr(&self, visit: &mut impl FnMut(&WirExpr)) {
        match self {
            Self::Iteration { watermark } | Self::Lexical { watermark } => visit(watermark),
            Self::CopyOut { watermark, source, retained_bytes } => {
                visit(watermark);
                visit(source);
                visit(retained_bytes);
            }
        }
    }

    fn for_each_wir_expr_mut(&mut self, visit: &mut impl FnMut(&mut WirExpr)) {
        match self {
            Self::Iteration { watermark } | Self::Lexical { watermark } => visit(watermark),
            Self::CopyOut { watermark, source, retained_bytes } => {
                visit(watermark);
                visit(source);
                visit(retained_bytes);
            }
        }
    }
}

/// Effect-only opaque kernel operations.
#[derive(Debug, Clone)]
pub enum SafeKernelNode {
    /// Restore a validator-authenticated region watermark using one fixed exit
    /// template. This is allocator-state authority, not object-memory access.
    RegionRewind(SafeRegionRewind),
    /// Restore the authenticated scratch frontier to the exact current LIFO
    /// mark after all borrowed generations in that scope have died.
    ScratchRewind {
        watermark: Box<WirExpr>,
    },
    /// Release one shared owner, reclaiming the object when its count reaches
    /// zero according to the fixed RC/free-list template.
    Release {
        object: SafeObjectExpr,
    },
    /// Immediately return proven-unique storage to the free list.
    Reclaim {
        object: SafeObjectExpr,
    },
}

impl SafeKernelNode {
    pub const fn operation(&self) -> SafeKernelOperation {
        match self {
            Self::RegionRewind(SafeRegionRewind::Iteration { .. }) => {
                SafeKernelOperation::RegionRewindIteration
            }
            Self::RegionRewind(SafeRegionRewind::Lexical { .. }) => {
                SafeKernelOperation::RegionRewindLexical
            }
            Self::RegionRewind(SafeRegionRewind::CopyOut { .. }) => {
                SafeKernelOperation::RegionRewindCopyOut
            }
            Self::ScratchRewind { .. } => SafeKernelOperation::ScratchRewind,
            Self::Release { .. } => SafeKernelOperation::Release,
            Self::Reclaim { .. } => SafeKernelOperation::Reclaim,
        }
    }

    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        match self {
            Self::RegionRewind(rewind) => rewind.for_each_wir_expr(&mut visit),
            Self::ScratchRewind { watermark } => visit(watermark),
            Self::Release { object } | Self::Reclaim { object } => {
                object.for_each_wir_expr(&mut visit);
            }
        }
    }

    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        match self {
            Self::RegionRewind(rewind) => rewind.for_each_wir_expr_mut(&mut visit),
            Self::ScratchRewind { watermark } => visit(watermark),
            Self::Release { object } | Self::Reclaim { object } => {
                object.for_each_wir_expr_mut(&mut visit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safewir_surface::{
        SafeProjectionExpr, SafeProjectionStep, SafeProvenanceExpr,
    };
    use crate::wir::{WirFunc, WirLocal, WirModule, WirNode, WirTy, collect_used_locals_seq};

    fn layout(byte: u8) -> LayoutId {
        LayoutId::from_bytes([byte; 32])
    }

    fn result_claim(layout_byte: u8, size: WirExpr) -> SafeKernelResultClaim {
        SafeKernelResultClaim {
            layout: layout(layout_byte),
            extent: SafeExtentExpr::Exact(SafeSizeExpr::Value(Box::new(size))),
        }
    }

    fn size_claim(encoded: WirExpr) -> SafeKernelSize {
        SafeKernelSize {
            checked: SafeSizeExpr::NarrowU32(Box::new(SafeSizeExpr::Value(Box::new(
                encoded.clone(),
            )))),
            encoded: Box::new(encoded),
        }
    }

    fn object(root: &str, layout_byte: u8) -> SafeObjectExpr {
        SafeObjectExpr {
            root: Box::new(WirExpr::GetLocal(root.into())),
            layout: layout(layout_byte),
            extent: SafeExtentExpr::Counted {
                header: SafeSizeExpr::Value(Box::new(WirExpr::GetLocal("header".into()))),
                count: Box::new(WirExpr::GetLocal("count".into())),
                stride: SafeSizeExpr::Value(Box::new(WirExpr::GetLocal("stride".into()))),
            },
            provenance: SafeProvenanceExpr::Aliases {
                object: Box::new(WirExpr::GetLocal("alias".into())),
                projection: SafeProjectionExpr::from_steps([
                    SafeProjectionStep::Element {
                        index: Box::new(WirExpr::GetLocal("index".into())),
                    },
                    SafeProjectionStep::Slice {
                        start: Box::new(WirExpr::GetLocal("start".into())),
                        length: Box::new(WirExpr::GetLocal("length".into())),
                    },
                ]),
            },
        }
    }

    fn module(body: Vec<WirNode>) -> WirModule {
        let names = [
            "object", "header", "count", "stride", "alias", "index", "start", "length",
            "size", "next_size", "checked_size", "encoded_size",
        ];
        WirModule {
            imports: vec![],
            funcs: vec![WirFunc {
                name: "main".into(),
                params: names
                    .into_iter()
                    .map(|name| WirLocal { name: name.into(), ty: WirTy::Bool })
                    .collect(),
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

    fn expression_operations() -> Vec<SafeKernelExpr> {
        vec![
            SafeKernelExpr::RegionMark,
            SafeKernelExpr::ScratchMark,
            SafeKernelExpr::Allocate {
                class: SafeAllocationClass::Bump,
                result: result_claim(1, WirExpr::GetLocal("size".into())),
                size: size_claim(WirExpr::GetLocal("size".into())),
            },
            SafeKernelExpr::Allocate {
                class: SafeAllocationClass::ReferenceCounted,
                result: result_claim(2, WirExpr::GetLocal("size".into())),
                size: size_claim(WirExpr::GetLocal("size".into())),
            },
            SafeKernelExpr::ScratchAllocate {
                result: result_claim(5, WirExpr::GetLocal("size".into())),
                size: size_claim(WirExpr::GetLocal("size".into())),
            },
            SafeKernelExpr::Retain { object: object("object", 3) },
            SafeKernelExpr::Reuse {
                object: object("object", 3),
                result: result_claim(4, WirExpr::GetLocal("next_size".into())),
                size: size_claim(WirExpr::GetLocal("next_size".into())),
            },
        ]
    }

    fn statement_operations() -> Vec<SafeKernelNode> {
        vec![
            SafeKernelNode::RegionRewind(SafeRegionRewind::Iteration {
                watermark: Box::new(WirExpr::GetLocal("start".into())),
            }),
            SafeKernelNode::RegionRewind(SafeRegionRewind::Lexical {
                watermark: Box::new(WirExpr::GetLocal("start".into())),
            }),
            SafeKernelNode::RegionRewind(SafeRegionRewind::CopyOut {
                watermark: Box::new(WirExpr::GetLocal("start".into())),
                source: Box::new(WirExpr::GetLocal("index".into())),
                retained_bytes: Box::new(WirExpr::GetLocal("length".into())),
            }),
            SafeKernelNode::ScratchRewind {
                watermark: Box::new(WirExpr::GetLocal("start".into())),
            },
            SafeKernelNode::Release { object: object("object", 3) },
            SafeKernelNode::Reclaim { object: object("object", 3) },
        ]
    }

    #[test]
    fn ordinary_encoder_rejects_every_kernel_operation_without_authority() {
        for operation in expression_operations() {
            let candidate = module(vec![WirNode::Drop(WirExpr::SafeKernel(operation))]);
            let error = crate::wir_encode::try_encode_with_gc(&candidate, &[], &[])
                .expect_err("an unvalidated kernel expression must not encode");
            assert!(error.message.contains("validated encoder authority"), "{error}");
        }
        for operation in statement_operations() {
            let candidate = module(vec![WirNode::SafeKernel(operation)]);
            let error = crate::wir_encode::try_encode_with_gc(&candidate, &[], &[])
                .expect_err("an unvalidated kernel statement must not encode");
            assert!(error.message.contains("validated encoder authority"), "{error}");
        }
    }

    #[test]
    fn visitors_reach_executable_operands_and_every_untrusted_ghost_claim() {
        let mut operation = SafeKernelExpr::Reuse {
            object: object("object", 3),
            result: result_claim(4, WirExpr::GetLocal("next_size".into())),
            size: SafeKernelSize {
                checked: SafeSizeExpr::NarrowU32(Box::new(SafeSizeExpr::Value(Box::new(
                    WirExpr::GetLocal("checked_size".into()),
                )))),
                encoded: Box::new(WirExpr::GetLocal("encoded_size".into())),
            },
        };
        let mut before = Vec::new();
        operation.for_each_wir_expr(|expr| {
            if let WirExpr::GetLocal(name) = expr {
                before.push(name.clone());
            }
        });
        assert_eq!(
            before,
            [
                "object",
                "header",
                "count",
                "stride",
                "alias",
                "index",
                "start",
                "length",
                "next_size",
                "checked_size",
                "encoded_size",
            ]
        );

        operation.for_each_wir_expr_mut(|expr| {
            if let WirExpr::GetLocal(name) = expr {
                *name = format!("visited_{name}");
            }
        });
        let body = vec![WirNode::Drop(WirExpr::SafeKernel(operation))];
        let mut used = std::collections::HashSet::new();
        collect_used_locals_seq(&body, &mut used);
        for name in before {
            assert!(used.contains(&format!("visited_{name}")), "visitor skipped {name}");
        }

        let mut rewind = SafeKernelNode::RegionRewind(SafeRegionRewind::CopyOut {
            watermark: Box::new(WirExpr::GetLocal("start".into())),
            source: Box::new(WirExpr::GetLocal("index".into())),
            retained_bytes: Box::new(WirExpr::GetLocal("length".into())),
        });
        let mut rewind_before = Vec::new();
        rewind.for_each_wir_expr(|expr| {
            if let WirExpr::GetLocal(name) = expr {
                rewind_before.push(name.clone());
            }
        });
        assert_eq!(rewind_before, ["start", "index", "length"]);
        rewind.for_each_wir_expr_mut(|expr| {
            if let WirExpr::GetLocal(name) = expr {
                *name = format!("visited_{name}");
            }
        });
        let body = vec![WirNode::SafeKernel(rewind)];
        let mut used = std::collections::HashSet::new();
        collect_used_locals_seq(&body, &mut used);
        assert!(used.contains("visited_start"));
        assert!(used.contains("visited_index"));
        assert!(used.contains("visited_length"));
    }

}
