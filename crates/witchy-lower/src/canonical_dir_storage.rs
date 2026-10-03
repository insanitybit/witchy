//! Directory storage operations retain source evaluation order and byte semantics.
use super::*;

pub(super) fn is_operation(name: &str) -> bool {
    matches!(name, "__capop.create_new" | "__capop.replace" | "__capop.read_bytes"
        | "__capop.write_bytes" | "__capop.append_bytes" | "__capop.set_executable" | "__capop.rename")
}

impl FunctionLowerer<'_> {
    /// `dir.read(path)`: one host read, then `Result(String, CapError)` from
    /// its response.
    pub(super) fn lower_dir_read(&mut self, token: ValueId, path: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(path)?;
        let path = self.string_slice_argument(path, layout)?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let read = self.capability_import("dir_read_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: read,
            arguments: vec![HostCallArgument::Capability(token), path],
        });
        self.lower_dir_text_result(response)
    }

    pub(super) fn lower_dir_storage(
        &mut self, name: &str, token: ValueId, arguments: &[Expr],
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let expected = if matches!(name, "__capop.read_bytes" | "__capop.set_executable") { 1 } else { 2 };
        if arguments.len() != expected {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: name.to_owned(), expected, actual: arguments.len(),
            });
        }
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(&arguments[0])?;
        // A later value argument may mutate the source path binding. Retain
        // the already evaluated value, making such mutation copy-on-write.
        let path = self.clone_reference_token(path)?;
        if name == "__capop.set_executable" {
            let path = self.string_slice_argument(path, layout)?;
            let import = self.capability_import("dir_set_executable_at")?;
            self.push_operation(Operation::HostCall {
                destinations: Vec::new(), import,
                arguments: vec![HostCallArgument::Capability(token), path],
            });
            return self.push_zero(ValueType::Unit).map(Some);
        }
        if name == "__capop.read_bytes" {
            let path = self.string_slice_argument(path, layout)?;
            let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
            let read = self.capability_import("dir_read_bytes_at")?;
            self.push_operation(Operation::HostCall {
                destinations: vec![response], import: read,
                arguments: vec![HostCallArgument::Capability(token), path],
            });
            // Size and copy consult the same immutable host snapshot. Neither
            // performs another filesystem read or consumes a staged global.
            return self.lower_dir_response_payload(response).map(Some);
        }
        let data = if matches!(name, "__capop.write_bytes" | "__capop.append_bytes") {
            self.lower_expr(&arguments[1])?
        } else {
            self.lower_string_argument(&arguments[1])?
        };
        let data = self.clone_reference_token(data)?;
        let path = self.string_slice_argument(path, layout)?;
        let data = self.string_slice_argument(data, layout)?;
        let (import_name, creates) = match name {
            "__capop.create_new" => ("dir_create_new_at", true),
            "__capop.replace" => ("dir_replace_at", false),
            "__capop.write_bytes" => ("dir_write_bytes_at", false),
            "__capop.append_bytes" => ("dir_append_bytes_at", false),
            "__capop.rename" => ("dir_rename_at", false),
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("Dir storage operation")),
        };
        let result = if creates { Some(self.fresh(ValueType::Bool)?) } else { None };
        let import = self.capability_import(import_name)?;
        self.push_operation(Operation::HostCall {
            destinations: result.into_iter().collect(), import,
            arguments: vec![HostCallArgument::Capability(token), path, data],
        });
        match result {
            Some(value) => Ok(Some(value)),
            None => self.push_zero(ValueType::Unit).map(Some),
        }
    }
}
