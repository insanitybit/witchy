//! RFC-0111 host-layout selection before WIR erases layouts to Wasm kinds.

use super::*;
use witchy_wir::layout::{HostLayoutDecision, HostLayoutPolicy};

/// No production host import currently reads a structured specialized value.
/// Keeping this registry empty is the fail-closed contract: adding an exact or
/// counted-marshal entry must land with the real adapter and its counter site.
pub(super) fn production_host_layout_policy(_boundary: &str) -> HostLayoutPolicy {
    HostLayoutPolicy::default()
}

pub(super) fn boundary_layout_is_unsupported(
    arguments: impl Iterator<Item = LayoutId>,
    result: Option<LayoutId>,
    mut unsupported: impl FnMut(LayoutId) -> bool,
) -> bool {
    arguments.chain(result).any(&mut unsupported)
}

impl Codegen<'_> {
    fn host_layout_is_unsupported(&self, boundary: &str, layout: LayoutId) -> bool {
        match production_host_layout_policy(boundary)
            .decide(&self.specialized_layouts, layout)
        {
            HostLayoutDecision::Exact => false,
            // A registered marshal decision is not permission to emit a raw
            // pointer call. Until the matching generated adapter consumes the
            // decision and increments its named metric, this code path rejects.
            HostLayoutDecision::Marshal { .. } | HostLayoutDecision::Reject => true,
        }
    }

    fn intrinsic_host_layout_is_unsupported(
        &self,
        name: &str,
        args: &[Expr],
        result: Option<LayoutId>,
    ) -> bool {
        let arguments = args
            .iter()
            .filter_map(|arg| self.specialized_layout_of_expr(arg));
        if intrinsics::wir_host_call(name).is_some() {
            boundary_layout_is_unsupported(arguments, result, |layout| {
                self.host_layout_is_unsupported(name, layout)
            })
        } else {
            // Non-host generic helpers still use the universal WIR ABI. They
            // remain unsupported until their own exact descriptor adapter
            // exists; they must not borrow authority from the host registry.
            boundary_layout_is_unsupported(arguments, result, |_| true)
        }
    }

    pub(super) fn reject_unsupported_specialized_boundary(&mut self, expr: &Expr) -> bool {
        let boundary = match expr {
            Expr::Call { name, args }
                if self.locals.contains_key(name)
                    && (self.specialized_boundary_result_layout(expr).is_some()
                        || args
                            .iter()
                            .any(|argument| self.specialized_layout_of_expr(argument).is_some())) =>
            {
                Some("first-class function call".to_string())
            }
            Expr::Call { name, args }
                if !self.emitted_funcs.contains(name)
                    && witchy_syntax::intrinsics::lookup(
                        witchy_syntax::intrinsics::canonical_operation_name(name),
                    )
                    .is_some()
                    && witchy_syntax::intrinsics::canonical_operation_name(name)
                        != intrinsics::LIST_LENGTH
                    && self.intrinsic_host_layout_is_unsupported(
                        name,
                        args,
                        self.specialized_boundary_result_layout(expr),
                    ) =>
            {
                Some(format!("intrinsic `{name}`"))
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
                ..
            } if self.specialized_boundary_result_layout(expr).is_some()
                || self.specialized_layout_of_expr(receiver).is_some()
                || args
                    .iter()
                    .any(|argument| self.specialized_layout_of_expr(argument).is_some()) =>
            {
                Some(format!("method `{method}`"))
            }
            Expr::ExistentialCall {
                receiver,
                method,
                args,
                ..
            } if self.specialized_boundary_result_layout(expr).is_some()
                || self.specialized_layout_of_expr(receiver).is_some()
                || args
                    .iter()
                    .any(|argument| self.specialized_layout_of_expr(argument).is_some()) =>
            {
                Some(format!("trait/existential method `{method}`"))
            }
            Expr::Apply { func, args }
                if self.specialized_boundary_result_layout(expr).is_some()
                    || self.specialized_layout_of_expr(func).is_some()
                    || args.iter().any(|arg| self.specialized_layout_of_expr(arg).is_some()) =>
            {
                Some("first-class function call".to_string())
            }
            Expr::Var(name)
                if !self.locals.contains_key(name) && self.callable_layouts.contains_key(name) =>
            {
                Some(format!("function value `{name}`"))
            }
            Expr::Binary { lhs, rhs, .. }
                if self.specialized_layout_of_expr(lhs).is_some()
                    || self.specialized_layout_of_expr(rhs).is_some() =>
            {
                Some("aggregate binary operation".to_string())
            }
            _ => None,
        };
        let Some(boundary) = boundary else { return false };
        self.reject_reason.get_or_insert_with(|| CodegenError {
            message: format!(
                "declared packed layout cannot cross unsupported {boundary}; \
                 this boundary requires an exact RFC-0111 LayoutId adapter and cannot box or reshape"
            ),
        });
        true
    }
}
