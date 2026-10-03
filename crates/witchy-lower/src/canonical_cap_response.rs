//! A fallible capability operation's host response as `Result(T, CapError)`.
//!
//! The host performs the operation once and answers with an immutable
//! response handle. Its status selects the variant (0 success, otherwise the
//! `CapError` ordinal plus one) and its payload is the success contents or the
//! failure message. Reading either consults the same snapshot, never the
//! filesystem again (RFC-0126 rule 3).
use super::*;
use witchy_syntax::cap_ops::{CAP_ERROR_TYPE, CAP_ERROR_VARIANTS};
use witchy_wir::canonical_safewir::DIR_RESPONSE_KIND;

impl FunctionLowerer<'_> {
    /// The response payload copied into a fresh guest string. The published
    /// length is the copy's answer under the buffer's own capacity guard.
    pub(super) fn lower_dir_response_payload(&mut self, response: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let size = self.fresh(ValueType::Size)?;
        let size_import = self.capability_import("dir_response_size")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![size], import: size_import,
            arguments: vec![HostCallArgument::Capability(response)],
        });
        let zero = self.push_const_size(0)?;
        let output = self.alloc_bytes(zero, size)?;
        let required = self.fresh(ValueType::Size)?;
        let copy = self.capability_import("dir_response_into")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![required], import: copy,
            arguments: vec![HostCallArgument::Capability(response), HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(output,
                    Projection::Slice { start: zero, length: size }, ValueType::Ref(layout), AccessMode::Write),
                extent: HostBufferExtent::Slice,
            }],
        });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, required, size)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: required });
        self.string_lengths.insert(output, required);
        self.string_capacities.insert(output, size);
        Ok(output)
    }

    /// `Result(String, CapError)` from a `Dir` response whose payload is the
    /// success text or the failure message.
    pub(super) fn lower_dir_text_result(&mut self, response: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        if self.values[&response] != ValueType::Capability(DIR_RESPONSE_KIND) {
            return Err(CanonicalLoweringError::UnsupportedExpression("Dir response token"));
        }
        let status = self.fresh(ValueType::Size)?;
        let status_import = self.capability_import("dir_response_status")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![status], import: status_import,
            arguments: vec![HostCallArgument::Capability(response)],
        });
        let text = self.lower_dir_response_payload(response)?;
        let string = Type::Named("String".into(), Vec::new());
        let error = Type::Named(CAP_ERROR_TYPE.into(), Vec::new());
        let result = Type::Named("Result".into(), vec![string, error.clone()]);
        let result_shape = scalar_aggregate_shape(&result, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("Result(String, CapError) shape"))?;
        let error_shape = scalar_aggregate_shape(&error, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("CapError shape"))?;
        let (ok, _) = result_shape.variant("Ok").ok_or(CanonicalLoweringError::MissingBinding("Ok".into()))?;
        let (err, _) = result_shape.variant("Err").ok_or(CanonicalLoweringError::MissingBinding("Err".into()))?;
        let affine = witchy_canonical_source::storage::affine_storage_shape(&result, self.definitions).is_some();
        let zero = self.push_const_size(0)?;
        let succeeded = self.push_compare(ComparisonOperator::Equal, status, zero)?;
        self.lower_conditional_branches(succeeded, affine, |this| {
            let value = this.lower_aggregate_for_source(&result, &result_shape, ok, vec![text])?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        }, |this| {
            let failure = this.lower_cap_error(status, text, &error, &error_shape, 0)?;
            let value = this.lower_aggregate_for_source(&result, &result_shape, err, vec![failure])?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        })?.ok_or(CanonicalLoweringError::UnsupportedExpression("divergent capability result"))
    }

    /// The `CapError` variant `status` selects, from the variant at
    /// `ordinal` on. The last variant, `Io`, also takes any status the
    /// ladder has not matched; the host answers no other.
    fn lower_cap_error(
        &mut self,
        status: ValueId,
        message: ValueId,
        error: &Type,
        shape: &ScalarAggregateShape,
        ordinal: usize,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let variant = |name: &str| shape.variant(name)
            .map(|(index, _)| index)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.into()));
        let affine = witchy_canonical_source::storage::affine_storage_shape(error, self.definitions).is_some();
        if ordinal + 1 == CAP_ERROR_VARIANTS.len() {
            let index = variant(CAP_ERROR_VARIANTS[ordinal])?;
            return self.lower_aggregate_for_source(error, shape, index, vec![message]);
        }
        let expected = self.push_const_size(u64::try_from(ordinal + 1).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
        let matches = self.push_compare(ComparisonOperator::Equal, status, expected)?;
        let index = variant(CAP_ERROR_VARIANTS[ordinal])?;
        self.lower_conditional_branches(matches, affine, |this| {
            let value = this.lower_aggregate_for_source(error, shape, index, vec![message])?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        }, |this| {
            let value = this.lower_cap_error(status, message, error, shape, ordinal + 1)?;
            Ok(Some(this.detach_bound_result(value, affine)?))
        })?.ok_or(CanonicalLoweringError::UnsupportedExpression("divergent CapError"))
    }
}
