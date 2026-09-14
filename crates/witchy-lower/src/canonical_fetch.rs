//! Snapshot source arguments, then pass initialized bounded slices once.
use super::*;
use witchy_wir::canonical_safewir::{FETCH_CAPABILITY_KIND, FETCH_RESPONSE_KIND};

impl FunctionLowerer<'_> {
    pub(super) fn lower_fetch_only(&mut self, fetch: ValueId, origins: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let text = self.lower_string_argument(origins)?;
        let origins = self.string_slice_argument(text, layout)?;
        let import = self.capability_import("fetch_only_at")?;
        let narrowed = self.fresh(ValueType::Capability(FETCH_CAPABILITY_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![narrowed], import,
            arguments: vec![HostCallArgument::Capability(fetch), origins] });
        Ok(narrowed)
    }

    pub(super) fn lower_fetch_derive(&mut self, net: ValueId, origin: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let text = self.lower_string_argument(origin)?;
        let origin = self.string_slice_argument(text, layout)?;
        let import = self.capability_import("net_fetch_at")?;
        let fetch = self.fresh(ValueType::Capability(FETCH_CAPABILITY_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![fetch], import,
            arguments: vec![HostCallArgument::Capability(net), origin] });
        Ok(fetch)
    }

    pub(super) fn lower_fetch_send(&mut self, fetch: ValueId, fields: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let mut snapshots = Vec::with_capacity(fields.len());
        for field in fields {
            let text = self.lower_string_argument(field)?;
            // A later argument can write back the binding that supplied this
            // value. Retain the evaluated object, not its old binding token.
            snapshots.push(self.clone_reference_token(text)?);
        }
        let mut arguments = vec![HostCallArgument::Capability(fetch)];
        for text in snapshots { arguments.push(self.string_slice_argument(text, layout)?); }
        let import = self.capability_import("fetch_send_at")?;
        let response = self.fresh(ValueType::Capability(FETCH_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![response], import, arguments });
        let size = self.capability_import("fetch_response_size")?;
        let copy = self.capability_import("fetch_response_into")?;
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![length], import: size,
            arguments: vec![HostCallArgument::Capability(response)] });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let object = self.alloc_bytes(length, capacity)?;
        let zero = self.push_const_size(0)?;
        let written = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![written], import: copy,
            arguments: vec![HostCallArgument::Capability(response), HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(object,
                    Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
                extent: HostBufferExtent::Slice,
            }] });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: written });
        self.string_lengths.insert(object, written);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }
}
