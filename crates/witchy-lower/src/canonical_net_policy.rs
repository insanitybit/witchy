//! `net.only(policy)` as `Result(Net, CapError)` and `net.deny(policy)` as
//! `Net` (RFC-0126 rule 3).
//!
//! Both pass the sealed `NetPolicy`'s pattern to one host call. Narrowing to
//! an endpoint the receiver does not already admit is a `Denied` response;
//! subtraction only ever removes endpoints, so it cannot fail.
use super::*;
use witchy_wir::canonical_safewir::{NET_CAPABILITY_KIND, CAP_RESPONSE_KIND};

impl FunctionLowerer<'_> {
    /// The policy's pattern as a read slice. `NetPolicy` is sealed, so only
    /// the checked `Net.*` builders produced it.
    fn net_policy_slice(&mut self, policy: &Expr) -> Result<HostCallArgument, CanonicalLoweringError> {
        if !matches!(self.expression_type(policy)?.unqualified(), Type::Named(name, arguments)
            if name.rsplit('.').next() == Some("NetPolicy") && arguments.is_empty())
        {
            return Err(CanonicalLoweringError::UnsupportedExpression("Net policy argument"));
        }
        let pattern = self.lower_record_field(policy, "pattern")?;
        self.ensure_string_extent(pattern)?;
        let layout = self.string_layout_id()?;
        self.string_slice_argument(pattern, layout)
    }

    pub(super) fn lower_net_only(&mut self, receiver: &Expr, token: ValueId, policy: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let success = self.expression_type(receiver)?;
        let pattern = self.net_policy_slice(policy)?;
        let response = self.fresh(ValueType::Capability(CAP_RESPONSE_KIND))?;
        let import = self.capability_import("net_only_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import,
            arguments: vec![HostCallArgument::Capability(token), pattern],
        });
        self.lower_cap_result_with(response, success, |this, _empty| this.lower_response_take(response, NET_CAPABILITY_KIND))
    }

    pub(super) fn lower_net_deny(&mut self, token: ValueId, policy: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let pattern = self.net_policy_slice(policy)?;
        let narrowed = self.fresh(ValueType::Capability(NET_CAPABILITY_KIND))?;
        let import = self.capability_import("net_deny_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![narrowed], import,
            arguments: vec![HostCallArgument::Capability(token), pattern],
        });
        Ok(narrowed)
    }
}
