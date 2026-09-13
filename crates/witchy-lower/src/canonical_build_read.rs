//! One authenticated read creates an immutable response. Size/copy do not read
//! again, and only a guarded initialized prefix becomes the returned String.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_build_read(&mut self, token: ValueId, path: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(path)?;
        let path = self.string_slice_argument(path, layout)?;
        let read = self.capability_import("build_read_at")?;
        let size = self.capability_import("build_read_response_size")?;
        let copy = self.capability_import("build_read_response_into")?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::BUILD_READ_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: read,
            arguments: vec![HostCallArgument::Capability(token), path],
        });
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![length], import: size,
            arguments: vec![HostCallArgument::Capability(response)],
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let object = self.alloc_bytes(length, capacity)?;
        let zero = self.push_const_size(0)?;
        let written = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![written], import: copy,
            arguments: vec![HostCallArgument::Capability(response), HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(object,
                    Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
                extent: HostBufferExtent::Slice,
            }],
        });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: written });
        self.string_lengths.insert(object, written);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }
}
