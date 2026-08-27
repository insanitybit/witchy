//! Opaque RFC-0162 memory-kernel operations carried through candidate WIR.
//!
//! These operations expose only their executable operands and untrusted ghost
//! claims. They deliberately contain no [`crate::wir::WirFunc`], raw body, or
//! implementation sequence: final validation must check every claim, and only
//! the future validated encoder may select the corresponding fixed template.

use crate::layout::LayoutId;
use crate::safewir_surface::{SafeExtentExpr, SafeObjectExpr};
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

/// Untrusted description of the object produced by allocation or reuse.
///
/// Definition and generation identities are intentionally absent. The final
/// validator assigns them from the actual result site; a producer cannot mint
/// either authority by constructing this claim.
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

/// Value-producing opaque kernel operations. Every variant produces the same
/// runtime `i32` object pointer representation; its ghost result claim is
/// erased only by the future validated encoder.
#[derive(Debug, Clone)]
pub enum SafeKernelExpr {
    /// Allocate `encoded_size` bytes using one fixed allocator template.
    Allocate {
        class: SafeAllocationClass,
        result: SafeKernelResultClaim,
        encoded_size: Box<WirExpr>,
    },
    /// Add one shared owner and return the unchanged object pointer.
    Retain {
        object: SafeObjectExpr,
    },
    /// Destructively reuse uniquely owned storage for a new object generation.
    Reuse {
        object: SafeObjectExpr,
        result: SafeKernelResultClaim,
        encoded_size: Box<WirExpr>,
    },
}

impl SafeKernelExpr {
    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        match self {
            Self::Allocate { result, encoded_size, .. } => {
                result.for_each_wir_expr(&mut visit);
                visit(encoded_size);
            }
            Self::Retain { object } => object.for_each_wir_expr(&mut visit),
            Self::Reuse { object, result, encoded_size } => {
                object.for_each_wir_expr(&mut visit);
                result.for_each_wir_expr(&mut visit);
                visit(encoded_size);
            }
        }
    }

    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        match self {
            Self::Allocate { result, encoded_size, .. } => {
                result.for_each_wir_expr_mut(&mut visit);
                visit(encoded_size);
            }
            Self::Retain { object } => object.for_each_wir_expr_mut(&mut visit),
            Self::Reuse { object, result, encoded_size } => {
                object.for_each_wir_expr_mut(&mut visit);
                result.for_each_wir_expr_mut(&mut visit);
                visit(encoded_size);
            }
        }
    }
}

/// Effect-only opaque kernel operations.
#[derive(Debug, Clone)]
pub enum SafeKernelNode {
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
    pub fn for_each_wir_expr(&self, mut visit: impl FnMut(&WirExpr)) {
        match self {
            Self::Release { object } | Self::Reclaim { object } => {
                object.for_each_wir_expr(&mut visit);
            }
        }
    }

    pub fn for_each_wir_expr_mut(&mut self, mut visit: impl FnMut(&mut WirExpr)) {
        match self {
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
        SafeProjectionExpr, SafeProjectionStep, SafeProvenanceExpr, SafeSizeExpr,
    };
    use crate::wir::{WirFunc, WirLocal, WirModule, WirNode, WirTy, collect_used_locals_seq, to_wat};

    fn layout(byte: u8) -> LayoutId {
        LayoutId::from_bytes([byte; 32])
    }

    fn result_claim(layout_byte: u8, size: WirExpr) -> SafeKernelResultClaim {
        SafeKernelResultClaim {
            layout: layout(layout_byte),
            extent: SafeExtentExpr::Exact(SafeSizeExpr::Value(Box::new(size))),
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
            "size", "next_size",
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
            SafeKernelExpr::Allocate {
                class: SafeAllocationClass::Bump,
                result: result_claim(1, WirExpr::GetLocal("size".into())),
                encoded_size: Box::new(WirExpr::GetLocal("size".into())),
            },
            SafeKernelExpr::Allocate {
                class: SafeAllocationClass::ReferenceCounted,
                result: result_claim(2, WirExpr::GetLocal("size".into())),
                encoded_size: Box::new(WirExpr::GetLocal("size".into())),
            },
            SafeKernelExpr::Retain { object: object("object", 3) },
            SafeKernelExpr::Reuse {
                object: object("object", 3),
                result: result_claim(4, WirExpr::GetLocal("next_size".into())),
                encoded_size: Box::new(WirExpr::GetLocal("next_size".into())),
            },
        ]
    }

    fn statement_operations() -> Vec<SafeKernelNode> {
        vec![
            SafeKernelNode::Release { object: object("object", 3) },
            SafeKernelNode::Reclaim { object: object("object", 3) },
        ]
    }

    #[test]
    fn opaque_operation_census_and_shapes_have_no_helper_body_slot() {
        let expression_shapes = expression_operations()
            .into_iter()
            .map(|operation| match operation {
                SafeKernelExpr::Allocate { class, result, encoded_size } => {
                    let SafeKernelResultClaim { layout: _, extent: _ } = result;
                    let _ = encoded_size;
                    match class {
                        SafeAllocationClass::Bump => "allocate.bump",
                        SafeAllocationClass::ReferenceCounted => "allocate.rc",
                    }
                }
                SafeKernelExpr::Retain { object } => {
                    let SafeObjectExpr { root: _, layout: _, extent: _, provenance: _ } = object;
                    "retain"
                }
                SafeKernelExpr::Reuse { object, result, encoded_size } => {
                    let SafeObjectExpr { root: _, layout: _, extent: _, provenance: _ } = object;
                    let SafeKernelResultClaim { layout: _, extent: _ } = result;
                    let _ = encoded_size;
                    "reuse"
                }
            })
            .collect::<Vec<_>>();
        let statement_shapes = statement_operations()
            .into_iter()
            .map(|operation| match operation {
                SafeKernelNode::Release { object } => {
                    let SafeObjectExpr { root: _, layout: _, extent: _, provenance: _ } = object;
                    "release"
                }
                SafeKernelNode::Reclaim { object } => {
                    let SafeObjectExpr { root: _, layout: _, extent: _, provenance: _ } = object;
                    "reclaim"
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(expression_shapes, ["allocate.bump", "allocate.rc", "retain", "reuse"]);
        assert_eq!(statement_shapes, ["release", "reclaim"]);
    }

    #[test]
    fn debug_shape_keeps_every_operation_opaque_and_first_class() {
        let mut body = expression_operations()
            .into_iter()
            .map(|operation| WirNode::Drop(WirExpr::SafeKernel(operation)))
            .collect::<Vec<_>>();
        body.extend(statement_operations().into_iter().map(WirNode::SafeKernel));

        let wat = to_wat(&module(body));
        let operations = wat
            .lines()
            .filter(|line| line.contains("safewir.kernel."))
            .map(str::trim)
            .collect::<Vec<_>>();
        assert_eq!(
            operations,
            [
                "safewir.kernel.allocate.bump",
                "safewir.kernel.allocate.rc",
                "safewir.kernel.retain",
                "safewir.kernel.reuse",
                "safewir.kernel.release",
                "safewir.kernel.reclaim",
            ]
        );
        assert!(!wat.contains("call $rc_"), "opaque operations became ordinary helpers:\n{wat}");
        assert!(!wat.contains("i32.load"), "kernel implementation leaked into candidate WIR:\n{wat}");
        assert!(!wat.contains("i32.store"), "kernel implementation leaked into candidate WIR:\n{wat}");
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
            encoded_size: Box::new(WirExpr::GetLocal("size".into())),
        };
        let mut before = Vec::new();
        operation.for_each_wir_expr(|expr| {
            if let WirExpr::GetLocal(name) = expr {
                before.push(name.clone());
            }
        });
        assert_eq!(
            before,
            ["object", "header", "count", "stride", "alias", "index", "start", "length", "next_size", "size"]
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
    }

    #[test]
    fn optimizer_keeps_kernel_effects_and_simplifies_nested_operands() {
        let redundant_size = WirExpr::FromSlot(
            Box::new(WirExpr::ToSlot(
                Box::new(WirExpr::GetLocal("size".into())),
                crate::wir::Kind::I32,
            )),
            crate::wir::Kind::I32,
        );
        let mut candidate = module(vec![
            WirNode::Drop(WirExpr::SafeKernel(SafeKernelExpr::Allocate {
                class: SafeAllocationClass::ReferenceCounted,
                result: result_claim(2, redundant_size.clone()),
                encoded_size: Box::new(redundant_size),
            })),
            WirNode::SafeKernel(SafeKernelNode::Release { object: object("object", 3) }),
        ]);

        let properties = crate::wir_opt::evaluate_seq(&candidate.funcs[0].body);
        assert!(!properties.is_safe_to_eliminate());
        assert!(properties.memory.is_unknown());

        crate::wir_opt::optimize(&mut candidate);
        let WirNode::Drop(WirExpr::SafeKernel(SafeKernelExpr::Allocate {
            result,
            encoded_size,
            ..
        })) = &candidate.funcs[0].body[0]
        else {
            panic!("optimizer erased or rewrote the opaque allocation");
        };
        assert!(matches!(encoded_size.as_ref(), WirExpr::GetLocal(name) if name == "size"));
        assert!(matches!(
            &result.extent,
            SafeExtentExpr::Exact(SafeSizeExpr::Value(value))
                if matches!(value.as_ref(), WirExpr::GetLocal(name) if name == "size")
        ));
        assert!(matches!(candidate.funcs[0].body[1], WirNode::SafeKernel(SafeKernelNode::Release { .. })));
    }
}
