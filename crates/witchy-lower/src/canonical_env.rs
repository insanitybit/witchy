//! `env.get_env(name)` as `Result(Option(String), CapError)`.
//!
//! The host consults the grant's allow-list and the process environment once
//! and answers with an immutable response handle. Its status is 0 for a set
//! variable, 1 for an unset one and 2 for a name the grant does not cover; its
//! payload is the value or the denial message (RFC-0126 rule 3).
use super::*;
use witchy_syntax::cap_ops::{CAP_ERROR_TYPE, CAP_ERROR_VARIANTS};
use witchy_wir::canonical_safewir::CAP_RESPONSE_KIND;

impl FunctionLowerer<'_> {
    pub(super) fn lower_env_get(&mut self, token: ValueId, name: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let name = self.lower_string_argument(name)?;
        let name = self.string_slice_argument(name, layout)?;
        let response = self.fresh(ValueType::Capability(CAP_RESPONSE_KIND))?;
        let lookup = self.capability_import("env_get_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: lookup,
            arguments: vec![HostCallArgument::Capability(token), name],
        });
        let status = self.lower_response_status(response)?;
        // Both reads consult the one snapshot; an unset answer's payload is
        // empty and unused.
        let text = self.lower_response_payload(response)?;
        let string = Type::Named("String".into(), Vec::new());
        let option = Type::Named("Option".into(), vec![string]);
        let error = Type::Named(CAP_ERROR_TYPE.into(), Vec::new());
        let result = Type::Named("Result".into(), vec![option.clone(), error.clone()]);
        let shape = |ty: &Type, what: &'static str| scalar_aggregate_shape(ty, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression(what));
        let result_shape = shape(&result, "Result(Option(String), CapError) shape")?;
        let option_shape = shape(&option, "Option(String) shape")?;
        let error_shape = shape(&error, "CapError shape")?;
        let variant = |shape: &ScalarAggregateShape, name: &str| shape.variant(name)
            .map(|(index, _)| index)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.into()));
        let ok = variant(&result_shape, "Ok")?;
        let err = variant(&result_shape, "Err")?;
        let some = variant(&option_shape, "Some")?;
        let none = variant(&option_shape, "None")?;
        let denied = variant(&error_shape, CAP_ERROR_VARIANTS[0])?;
        let affine = witchy_canonical_source::storage::affine_storage_shape(&result, self.definitions).is_some();
        let zero = self.push_const_size(0)?;
        let set = self.push_compare(ComparisonOperator::Equal, status, zero)?;
        self.lower_conditional_branches(set, affine, |this| {
            let value = this.lower_aggregate_for_source(&option, &option_shape, some, vec![text])?;
            let value = this.lower_aggregate_for_source(&result, &result_shape, ok, vec![value])?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        }, |this| {
            let one = this.push_const_size(1)?;
            let unset = this.push_compare(ComparisonOperator::Equal, status, one)?;
            // The host answers no other status; anything else fails closed
            // as a denial.
            let value = this.lower_conditional_branches(unset, affine, |this| {
                let value = this.lower_aggregate_for_source(&option, &option_shape, none, Vec::new())?;
                let value = this.lower_aggregate_for_source(&result, &result_shape, ok, vec![value])?;
                Ok(Some(this.detach_bound_result(value, affine)?))
            }, |this| {
                let value = this.lower_aggregate_for_source(&error, &error_shape, denied, vec![text])?;
                let value = this.lower_aggregate_for_source(&result, &result_shape, err, vec![value])?;
                Ok(Some(this.detach_bound_result(value, affine)?))
            })?.ok_or(CanonicalLoweringError::UnsupportedExpression("divergent Env result"))?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        })?.ok_or(CanonicalLoweringError::UnsupportedExpression("divergent Env result"))
    }
}
