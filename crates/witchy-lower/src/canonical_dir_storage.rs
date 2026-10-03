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
        self.lower_cap_result(response, "String", "dir")
    }

    /// `dir.list()`: one host listing, then `Result(List(String), CapError)`.
    pub(super) fn lower_dir_list(&mut self, token: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let list = self.capability_import("dir_list_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: list,
            arguments: vec![HostCallArgument::Capability(token)],
        });
        let list_type = Type::Named("List".into(), vec![Type::Named("String".into(), Vec::new())]);
        self.lower_cap_result_with(response, list_type, "dir", |this, names| this.lower_listing(names))
    }

    /// The NUL-separated names of a listing as a `List(String)`; an empty
    /// payload is the empty list (a split would yield one empty name).
    fn lower_listing(&mut self, names: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let list_type = Type::Named("List".into(), vec![Type::Named("String".into(), Vec::new())]);
        let (list_layout_id, _, _, _) = list_layout(&list_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions)?;
        let length = self.string_length_of(names)?;
        let zero = self.push_const_size(0)?;
        let empty = self.push_compare(ComparisonOperator::Equal, length, zero)?;
        let facts = self.snapshot_facts();
        let none_block = self.new_block(&[])?;
        let split_block = self.new_block(&[])?;
        let join = self.new_block_owned(&[
            (ValueType::Ref(list_layout_id), Some(Ownership::Unique)),
            (ValueType::Size, None),
            (ValueType::Size, None),
        ])?;
        self.finish_current(Terminator::BranchIf {
            condition: empty,
            then_target: none_block,
            then_arguments: Vec::new(),
            else_target: split_block,
            else_arguments: Vec::new(),
        })?;
        self.current = none_block;
        let nothing = self.push_empty_value(&list_type)?;
        let (lanes, _) = self.accumulator_lanes(nothing)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = split_block;
        self.restore_snapshot(facts.clone());
        let separator = self.lower_string_literal(DIRECTORY_LISTING_SEPARATOR)?;
        let pieces = self.lower_string_split(names, separator)?;
        let (lanes, _) = self.accumulator_lanes(pieces)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.restore_facts(facts, join);
        self.current = join;
        let joined = self.block_parameter_values(join);
        self.track_accumulator_lanes(&joined);
        Ok(joined[0])
    }

    /// `dir.subtree`, `dir.read_file`, `dir.write_file`: one host navigation
    /// (`open`), then `Result(Dir|File, CapError)` whose success value is the
    /// capability the response carries.
    pub(super) fn lower_dir_navigate(&mut self, receiver: &Expr, token: ValueId, path: &Expr, open: &str) -> Result<ValueId, CanonicalLoweringError> {
        use witchy_wir::canonical_safewir::{DIR_CAPABILITY_KIND, DIR_RESPONSE_KIND, FILE_CAPABILITY_KIND};
        use witchy_types::typeck::{ty_to_ast, FileRights, Ty};
        // The success type is exactly the checked one: the receiver's `Dir`
        // for `subtree`, the conferred `File[Read]`/`File[Write]` otherwise.
        let (success, take, kind) = match open {
            "dir_subtree_open_at" => (self.expression_type(receiver)?, "dir_response_take_dir", DIR_CAPABILITY_KIND),
            _ => {
                let read = open == "dir_read_file_at";
                let file = ty_to_ast(&Ty::File(FileRights { read, write: !read }))
                    .ok_or(CanonicalLoweringError::UnsupportedExpression("File result type"))?;
                (file, "dir_response_take_file", FILE_CAPABILITY_KIND)
            }
        };
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(path)?;
        let path = self.string_slice_argument(path, layout)?;
        let response = self.fresh(ValueType::Capability(DIR_RESPONSE_KIND))?;
        let import = self.capability_import(open)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import,
            arguments: vec![HostCallArgument::Capability(token), path],
        });
        self.lower_cap_result_with(response, success, "dir", |this, _empty| {
            let minted = this.fresh(ValueType::Capability(kind))?;
            let take = this.capability_import(take)?;
            this.push_operation(Operation::HostCall {
                destinations: vec![minted], import: take,
                arguments: vec![HostCallArgument::Capability(response)],
            });
            Ok(minted)
        })
    }

    /// `file.read()`: one host read, then `Result(String, CapError)` from its
    /// response under File authority.
    pub(super) fn lower_file_read(&mut self, token: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let read = self.capability_import("file_read_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: read,
            arguments: vec![HostCallArgument::Capability(token)],
        });
        self.lower_cap_result(response, "String", "file")
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
            return self.lower_effect_call(token, "dir_set_executable_at", vec![path]).map(Some);
        }
        if name == "__capop.read_bytes" {
            let path = self.string_slice_argument(path, layout)?;
            let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
            let read = self.capability_import("dir_read_bytes_at")?;
            self.push_operation(Operation::HostCall {
                destinations: vec![response], import: read,
                arguments: vec![HostCallArgument::Capability(token), path],
            });
            // Status, size and copy consult the same immutable host snapshot.
            // None performs another filesystem read or consumes a staged global.
            return self.lower_cap_result(response, "Bytes", "dir").map(Some);
        }
        let data = if matches!(name, "__capop.write_bytes" | "__capop.append_bytes") {
            self.lower_expr(&arguments[1])?
        } else {
            self.lower_string_argument(&arguments[1])?
        };
        let data = self.clone_reference_token(data)?;
        let path = self.string_slice_argument(path, layout)?;
        let data = self.string_slice_argument(data, layout)?;
        let import_name = match name {
            "__capop.create_new" => "dir_create_new_at",
            "__capop.replace" => "dir_replace_at",
            "__capop.write_bytes" => "dir_write_bytes_at",
            "__capop.append_bytes" => "dir_append_bytes_at",
            "__capop.rename" => "dir_rename_at",
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("Dir storage operation")),
        };
        self.lower_effect_call(token, import_name, vec![path, data]).map(Some)
    }

    /// One write-family host call answering a response, then
    /// `Result((), CapError)` from it (RFC-0126 rule 3).
    fn lower_effect_call(
        &mut self, token: ValueId, import: &str, arguments: Vec<HostCallArgument>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let import = self.capability_import(import)?;
        let mut all = vec![HostCallArgument::Capability(token)];
        all.extend(arguments);
        self.push_operation(Operation::HostCall { destinations: vec![response], import, arguments: all });
        let unit = witchy_types::typeck::ty_to_ast(&witchy_types::typeck::Ty::Unit)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("unit result type"))?;
        self.lower_cap_result_with(response, unit, "dir", |this, _| this.push_zero(ValueType::Unit))
    }

    /// `dir.write`, `dir.append`, `dir.make_dir`: string arguments in source
    /// order, retained against later mutation, then one effect call.
    pub(super) fn lower_dir_effect(
        &mut self, token: ValueId, import: &str, arguments: &[&Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let mut values = Vec::new();
        for argument in arguments {
            let value = self.lower_string_argument(argument)?;
            values.push(self.clone_reference_token(value)?);
        }
        let mut slices = Vec::new();
        for value in values {
            slices.push(self.string_slice_argument(value, layout)?);
        }
        self.lower_effect_call(token, import, slices)
    }

    /// `dir.exists`, `dir.is_dir`: one probe answering a response whose flag
    /// is the answer, as `Result(Bool, CapError)`.
    pub(super) fn lower_dir_probe(
        &mut self, token: ValueId, import: &str, path: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(path)?;
        let path = self.string_slice_argument(path, layout)?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let import = self.capability_import(import)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import,
            arguments: vec![HostCallArgument::Capability(token), path],
        });
        let boolean = witchy_types::typeck::ty_to_ast(&witchy_types::typeck::Ty::Bool)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("Bool result type"))?;
        self.lower_cap_result_with(response, boolean, "dir", |this, _| {
            let flag = this.fresh(ValueType::Size)?;
            let import = this.capability_import("dir_response_flag")?;
            this.push_operation(Operation::HostCall {
                destinations: vec![flag], import,
                arguments: vec![HostCallArgument::Capability(response)],
            });
            let zero = this.push_const_size(0)?;
            this.push_compare(ComparisonOperator::NotEqual, flag, zero)
        })
    }

    /// `file.write(data)`: one host write, then `Result((), CapError)` under
    /// File authority.
    pub(super) fn lower_file_effect(&mut self, token: ValueId, data: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let data = self.lower_string_argument(data)?;
        let data = self.string_slice_argument(data, layout)?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::DIR_RESPONSE_KIND))?;
        let import = self.capability_import("file_write_at")?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import,
            arguments: vec![HostCallArgument::Capability(token), data],
        });
        let unit = witchy_types::typeck::ty_to_ast(&witchy_types::typeck::Ty::Unit)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("unit result type"))?;
        self.lower_cap_result_with(response, unit, "file", |this, _| this.push_zero(ValueType::Unit))
    }
}
