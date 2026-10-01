//! Generic dictionary probing over the authenticated guest table.
//!
//! Bucket metadata and typed key loads go through checked dictionary
//! operations. Equality remains authoritative: hashes select a probe path,
//! but a full row matches only after the key comparator succeeds.

use super::*;
use witchy_wir::canonical_safewir::dictionary::{
    DictionaryHeaderField, DictionaryOperation, DictionaryRehashTransfer, DictionarySlotField,
};
use witchy_wir::layout::LayoutKind;

impl FunctionLowerer<'_> {
    pub(super) fn is_table_dictionary(
        &mut self,
        source: &Type,
    ) -> Result<bool, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.field_value_type(source)? else {
            return Ok(false);
        };
        let physical = self.layouts.resolve_nominal(layout)
            .map_err(|_| CanonicalLoweringError::MissingLayout(source.clone()))?;
        Ok(matches!(
            self.layouts.get(physical).map(|descriptor| descriptor.kind()),
            Some(LayoutKind::CanonicalDictionary { .. })
        ))
    }

    fn lower_dict_table_operand(
        &mut self,
        operand: &Expr,
    ) -> Result<(ValueId, Type, Type), CanonicalLoweringError> {
        let ty = self.expression_type(operand)?;
        let Type::Named(name, arguments) = ty.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict type"));
        };
        if name != "Dict" || arguments.len() != 2 || !self.is_table_dictionary(&ty)? {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "dict table type",
            ));
        }
        let key_type = arguments[0].clone();
        let value_type = arguments[1].clone();
        let dict = self.lower_expr(operand)?;
        Ok((dict, key_type, value_type))
    }

    /// Construct a private table. `Initialize` authenticates the specialized
    /// layout and initializes every bucket state before publishing the root.
    pub(super) fn lower_dict_table_new(
        &mut self,
        dict_type: &Type,
        capacity: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let value_type = self.field_value_type(dict_type)?;
        let ValueType::Ref(layout) = value_type else {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "dictionary table constructor layout",
            ));
        };
        if self.values[&capacity] != ValueType::Size {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "dictionary table constructor capacity",
            ));
        }
        let destination = self.fresh(value_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::Initialize {
            destination,
            layout,
            capacity,
        }));
        Ok(destination)
    }

    pub(super) fn lower_dict_clone_exact(&mut self, source: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: capacity,
            dict: source,
            field: DictionaryHeaderField::Capacity,
        }));
        let destination = self.fresh(self.values[&source])?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::CloneExact {
            destination,
            source,
            capacity,
        }));
        Ok(destination)
    }

    fn round_dict_table_capacity(
        &mut self,
        requested: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let minimum = self.push_const_size(16)?;
        let header = self.new_block(&[ValueType::Size])?;
        let grow = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![minimum],
        })?;
        self.current = header;
        let current = self.block_parameter_values(header)[0];
        let too_small = self.push_compare(ComparisonOperator::LessUnsigned, current, requested)?;
        self.finish_current(Terminator::BranchIf {
            condition: too_small,
            then_target: grow,
            then_arguments: vec![current],
            else_target: exit,
            else_arguments: vec![current],
        })?;
        self.current = grow;
        let current = self.block_parameter_values(grow)[0];
        let doubled = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Add,
            current,
            current,
            OverflowBehavior::Trap,
        )?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![doubled],
        })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    fn dict_bucket(&mut self, hash_or_next: ValueId, capacity: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        // Constructors round to a power of two; CloneExact and Rehash keep it.
        let one = self.push_const_size(1)?;
        let mask = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, capacity, one, OverflowBehavior::Wrap)?;
        self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitAnd, hash_or_next, mask, OverflowBehavior::Wrap)
    }

    pub(super) fn lower_dict_table_value_at(
        &mut self,
        dict: ValueId,
        bucket: ValueId,
        value_type: &Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let ty = self.field_value_type(value_type)?;
        let destination = self.fresh(ty)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination,
            dict,
            bucket,
            field: DictionarySlotField::Value,
            transfer: if matches!(ty, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        }));
        Ok(destination)
    }

    fn lower_dict_table_list(
        &mut self,
        dict: ValueId,
        element_source_type: &Type,
        field: DictionarySlotField,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let list_type = Type::Named("List".into(), vec![element_source_type.clone()]);
        let element_type = self.field_value_type(element_source_type)?;
        let out = self.push_empty_value(&list_type)?;
        let first = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: first,
            dict,
            field: DictionaryHeaderField::Head,
        }));
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let (lanes, mut shapes) = self.accumulator_lanes(out)?;
        let exit = self.new_block_owned(&shapes)?;
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let mut entry = lanes.clone();
        entry.push(first);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        self.current = header;
        let current = self.block_parameter_values(header);
        self.track_accumulator_lanes(&current);
        let more = self.push_compare(ComparisonOperator::NotEqual, current[3], zero)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: current.clone(),
            else_target: exit,
            else_arguments: current[..3].to_vec(),
        })?;
        self.current = body;
        let current = self.block_parameter_values(body);
        self.track_accumulator_lanes(&current);
        let bucket = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Subtract,
            current[3],
            one,
            OverflowBehavior::Wrap,
        )?;
        let element = self.fresh(element_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: element,
            dict,
            bucket,
            field,
            transfer: if matches!(element_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        }));
        let element = self.owned_dict_value(element, element_source_type)?;
        let grown = self.push_owned_or_reuse_scalar_element(current[0], element, &list_type)?;
        let next = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: next,
            dict,
            bucket,
            field: DictionarySlotField::Next,
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        }));
        let (folded, _) = self.accumulator_lanes(grown)?;
        let mut folded = self.split_folded_lanes(folded)?;
        folded.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = exit;
        let result = self.block_parameter_values(exit);
        self.track_accumulator_lanes(&result);
        Ok(result[0])
    }

    fn lower_dict_table_pairs(
        &mut self,
        dict: ValueId,
        key_type: &Type,
        value_type: &Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let pair_type = Type::Tuple(vec![key_type.clone(), value_type.clone()]);
        let pair_shape = scalar_aggregate_shape(&pair_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("dict pair type"))?;
        let list_type = Type::Named("List".into(), vec![pair_type.clone()]);
        let out = self.push_empty_value(&list_type)?;
        let first = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: first,
            dict,
            field: DictionaryHeaderField::Head,
        }));
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let (lanes, mut shapes) = self.accumulator_lanes(out)?;
        let exit = self.new_block_owned(&shapes)?;
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let mut entry = lanes.clone();
        entry.push(first);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        self.current = header;
        let current = self.block_parameter_values(header);
        self.track_accumulator_lanes(&current);
        let more = self.push_compare(ComparisonOperator::NotEqual, current[3], zero)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: current.clone(),
            else_target: exit,
            else_arguments: current[..3].to_vec(),
        })?;
        self.current = body;
        let current = self.block_parameter_values(body);
        self.track_accumulator_lanes(&current);
        let bucket = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Subtract,
            current[3],
            one,
            OverflowBehavior::Wrap,
        )?;
        let mut elements = Vec::with_capacity(2);
        for (source_type, field) in [(key_type, DictionarySlotField::Key), (value_type, DictionarySlotField::Value)] {
            let element_type = self.field_value_type(source_type)?;
            let element = self.fresh(element_type)?;
            self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
                destination: element,
                dict,
                bucket,
                field,
                transfer: if matches!(element_type, ValueType::Ref(_)) {
                    witchy_wir::canonical_safewir::LoadTransfer::CloneShared
                } else {
                    witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
                },
            }));
            elements.push(self.owned_dict_value(element, source_type)?);
        }
        let pair = self.lower_aggregate_moving(&pair_type, &pair_shape, 0, elements)?;
        let grown = self.push_owned_or_reuse_scalar_element(current[0], pair, &list_type)?;
        let next = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: next,
            dict,
            bucket,
            field: DictionarySlotField::Next,
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        }));
        let (folded, _) = self.accumulator_lanes(grown)?;
        let mut folded = self.split_folded_lanes(folded)?;
        folded.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = exit;
        let result = self.block_parameter_values(exit);
        self.track_accumulator_lanes(&result);
        Ok(result[0])
    }

    pub(super) fn lower_dict_table_call(
        &mut self,
        expression: &Expr,
        name: &str,
        args: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        match (name, args) {
            ("dict.new", []) => {
                let ty = self.expression_type(expression)?;
                let capacity = self.push_const_size(16)?;
                self.lower_dict_table_new(&ty, capacity)
            }
            ("dict.with_capacity", [hint]) => {
                let ty = self.expression_type(expression)?;
                // Reservation is a hint. The checked constructor owns the
                // power-of-two rounding and capacity overflow policy.
                let hint = self.lower_expr(hint)?;
                let zero_int = self.push_const_int(0)?;
                let negative = self.push_compare(ComparisonOperator::LessSigned, hint, zero_int)?;
                let zero = self.push_const_size(0)?;
                let nonnegative = self.new_block(&[])?;
                let clamp = self.new_block(&[])?;
                let join = self.new_block(&[ValueType::Size])?;
                self.finish_current(Terminator::BranchIf {
                    condition: negative,
                    then_target: clamp,
                    then_arguments: Vec::new(),
                    else_target: nonnegative,
                    else_arguments: Vec::new(),
                })?;
                self.current = clamp;
                self.finish_current(Terminator::Branch {
                    target: join,
                    arguments: vec![zero],
                })?;
                self.current = nonnegative;
                let limit = self.push_const_int(1 << 30)?;
                let within_limit =
                    self.push_compare(ComparisonOperator::LessEqualSigned, hint, limit)?;
                self.push_trap_unless(within_limit)?;
                let capacity = self.fresh(ValueType::Size)?;
                self.push_operation(Operation::NarrowSize {
                    destination: capacity,
                    value: hint,
                });
                self.finish_current(Terminator::Branch {
                    target: join,
                    arguments: vec![capacity],
                })?;
                self.current = join;
                let capacity = self.block_parameter_values(join)[0];
                let capacity = self.round_dict_table_capacity(capacity)?;
                self.lower_dict_table_new(&ty, capacity)
            }
            ("dict.length", [operand]) => {
                let (dict, _, _) = self.lower_dict_table_operand(operand)?;
                let count = self.fresh(ValueType::Size)?;
                self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
                    destination: count,
                    dict,
                    field: DictionaryHeaderField::Count,
                }));
                let length = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::WidenSize {
                    destination: length,
                    value: count,
                });
                Ok(length)
            }
            ("dict.contains_key", [operand, key]) => {
                let (dict, key_type, _) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let (found, _, _, _) = self.lower_dict_find_table(dict, key, &key_type)?;
                Ok(found)
            }
            ("dict.__contains_by", [operand, query]) => {
                let (dict, key_type, _) = self.lower_dict_table_operand(operand)?;
                let query_type = self.expression_type(query)?;
                let Type::Qualified(TypeQual::View, payload) = query_type else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict query view"));
                };
                let query = self.lower_expr(query)?;
                let (found, _, _, _) = self.lower_dict_find_query_table(
                    dict, query, &key_type, payload.unqualified(),
                )?;
                Ok(found)
            }
            ("dict.at", [operand, key]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let (found, bucket, _, _) = self.lower_dict_find_table(dict, key, &key_type)?;
                self.push_trap_unless(found)?;
                self.lower_dict_table_value_at(dict, bucket, &value_type)
            }
            ("dict.get_or", [operand, key, default]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let default = self.lower_expr(default)?;
                let (found, bucket, _, _) = self.lower_dict_find_table(dict, key, &key_type)?;
                let facts = self.snapshot_facts();
                let bindings = self.bindings.clone();
                let hit = self.new_block(&[])?;
                let miss = self.new_block(&[])?;
                let join = self.dict_value_join(&value_type)?;
                self.finish_current(Terminator::BranchIf {
                    condition: found,
                    then_target: hit,
                    then_arguments: Vec::new(),
                    else_target: miss,
                    else_arguments: Vec::new(),
                })?;
                self.current = hit;
                let present = self.lower_dict_table_value_at(dict, bucket, &value_type)?;
                let present = self.owned_dict_value(present, &value_type)?;
                let lanes = self.dict_value_lanes(present, &value_type)?;
                self.finish_current(Terminator::Branch {
                    target: join,
                    arguments: lanes,
                })?;
                self.current = miss;
                self.restore_snapshot(facts.clone());
                let absent = self.owned_dict_value(default, &value_type)?;
                let lanes = self.dict_value_lanes(absent, &value_type)?;
                self.finish_current(Terminator::Branch {
                    target: join,
                    arguments: lanes,
                })?;
                self.restore_facts(facts, join);
                self.current = join;
                self.bindings = bindings;
                let values = self.block_parameter_values(join);
                self.track_accumulator_lanes(&values);
                Ok(values[0])
            }
            ("dict.__insert_extract", [Expr::Var(target), key, value]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(&args[0])?;
                let key = self.lower_expr(key)?;
                let value = self.lower_expr(value)?;
                let (rebuilt, extracted) = self.lower_dict_insert_table(
                    dict, &key_type, &value_type, key, value, true,
                )?;
                self.bindings.insert(target.clone(), rebuilt);
                extracted.ok_or(CanonicalLoweringError::UnsupportedExpression("dict insert result"))
            }
            ("dict.__insert", [operand, key, value]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let value = self.lower_expr(value)?;
                let (rebuilt, _) = self.lower_dict_insert_table(
                    dict, &key_type, &value_type, key, value, false,
                )?;
                Ok(rebuilt)
            }
            ("dict.__remove_extract", [Expr::Var(target), key]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(&args[0])?;
                let key = self.lower_expr(key)?;
                let (rebuilt, extracted) = self.lower_dict_remove_table(
                    dict, &key_type, &value_type, key, true,
                )?;
                self.bindings.insert(target.clone(), rebuilt);
                extracted.ok_or(CanonicalLoweringError::UnsupportedExpression("dict remove result"))
            }
            ("dict.__remove", [operand, key]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let (rebuilt, _) = self.lower_dict_remove_table(
                    dict, &key_type, &value_type, key, false,
                )?;
                Ok(rebuilt)
            }
            ("dict.keys" | "dict.values", [operand]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                if name == "dict.keys" {
                    self.lower_dict_table_list(dict, &key_type, DictionarySlotField::Key)
                } else {
                    self.lower_dict_table_list(dict, &value_type, DictionarySlotField::Value)
                }
            }
            ("dict.pairs", [operand]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                self.lower_dict_table_pairs(dict, &key_type, &value_type)
            }
            ("dict.__update", [operand, key, default, callback]) => {
                let (dict, key_type, value_type) = self.lower_dict_table_operand(operand)?;
                let key = self.lower_expr(key)?;
                let default = self.lower_expr(default)?;
                let callback = self.lower_expr(callback)?;
                self.lower_dict_update_table(
                    dict, &key_type, &value_type, key, default, callback,
                )
            }
            _ => Err(CanonicalLoweringError::UnsupportedExpression(
                "dict table operation",
            )),
        }
    }

    /// Return `(found, bucket, first_tombstone, hash)` for a dictionary query.
    /// A miss's `bucket` is the first empty slot or `capacity` if none exists.
    /// `first_tombstone` is `capacity` when none precedes the miss. Insertion
    /// reuses a tombstone only after the full probe has ruled out an equal key.
    pub(super) fn lower_dict_find_table(
        &mut self,
        dict: ValueId,
        key: ValueId,
        key_type: &Type,
    ) -> Result<(ValueId, ValueId, ValueId, ValueId), CanonicalLoweringError> {
        let hash = self.lower_dict_key_hash(key, key_type)?;
        self.lower_dict_find_table_hashed(dict, key, key_type, hash, None)
    }

    fn lower_dict_find_query_table(
        &mut self,
        dict: ValueId,
        query: ValueId,
        key_type: &Type,
        query_type: &Type,
    ) -> Result<(ValueId, ValueId, ValueId, ValueId), CanonicalLoweringError> {
        let (_, _, comparator, hasher) = self.dictionary_equivalents.iter()
            .find(|(key, candidate, _, _)| key.unqualified() == key_type.unqualified()
                && candidate.unqualified() == query_type.unqualified())
            .ok_or(CanonicalLoweringError::UnsupportedExpression("dict Equivalent witness"))?;
        let comparator = comparator.clone();
        let hasher = hasher.clone();
        let hash = self.lower_dict_hash_call(&hasher, query)?;
        self.lower_dict_find_table_hashed(dict, query, key_type, hash, Some(&comparator))
    }

    fn lower_dict_find_table_hashed(
        &mut self,
        dict: ValueId,
        key: ValueId,
        key_type: &Type,
        hash: ValueId,
        query_comparator: Option<&str>,
    ) -> Result<(ValueId, ValueId, ValueId, ValueId), CanonicalLoweringError> {
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: capacity,
            dict,
            field: DictionaryHeaderField::Capacity,
        }));
        // Narrow only a nonnegative low-bit prefix. This is a bucket selector,
        // never a key identity; the full comparator resolves collisions.
        let low_mask = self.push_const_int(0xffff_ffff)?;
        let low = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::BitAnd,
            hash,
            low_mask,
            OverflowBehavior::Wrap,
        )?;
        let low_size = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize {
            destination: low_size,
            value: low,
        });
        // Keep the fingerprint out of the low bits used by the initial
        // bucket selector. The stored hash is the same 32-bit low prefix.
        let fingerprint_shift = self.push_const_int(25)?;
        let fingerprint_int = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::ShiftRightUnsigned,
            low,
            fingerprint_shift,
            OverflowBehavior::Wrap,
        )?;
        let fingerprint = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize {
            destination: fingerprint,
            value: fingerprint_int,
        });
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let sixteen = self.push_const_size(16)?;
        let missing = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool {
            destination: missing,
            value: false,
        });
        let present = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool {
            destination: present,
            value: true,
        });
        let first = self.dict_bucket(low_size, capacity)?;
        // The last four lanes carry the three SIMD control masks and the
        // number of buckets remaining in the current group.
        let lanes = [ValueType::Size; 7];
        let header = self.new_block(&lanes)?;
        let load_group = self.new_block(&lanes)?;
        let probe = self.new_block(&lanes)?;
        let nonempty = self.new_block(&lanes)?;
        let occupied = self.new_block(&lanes)?;
        let compare_key = self.new_block(&lanes)?;
        let advance = self.new_block(&lanes)?;
        let exit = self.new_block(&[ValueType::Bool, ValueType::Size, ValueType::Size])?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![first, zero, capacity, zero, zero, zero, zero],
        })?;

        self.current = header;
        let cursor = self.block_parameter_values(header);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor[1], capacity)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: load_group,
            then_arguments: cursor.clone(),
            else_target: exit,
            else_arguments: vec![missing, capacity, cursor[2]],
        })?;

        self.current = load_group;
        let cursor = self.block_parameter_values(load_group);
        let group_exhausted = self.push_compare(ComparisonOperator::Equal, cursor[6], zero)?;
        let scan_group = self.new_block(&lanes)?;
        self.finish_current(Terminator::BranchIf {
            condition: group_exhausted,
            then_target: scan_group,
            then_arguments: cursor.clone(),
            else_target: probe,
            else_arguments: cursor,
        })?;
        self.current = scan_group;
        let cursor = self.block_parameter_values(scan_group);
        let matching = self.fresh(ValueType::Size)?;
        let empty = self.fresh(ValueType::Size)?;
        let deleted = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ProbeGroup {
            matching, empty, deleted, dict, bucket: cursor[0], fingerprint,
        }));
        let events = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitOr, matching, empty, OverflowBehavior::Wrap)?;
        let events = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitOr, events, deleted, OverflowBehavior::Wrap)?;
        let has_event = self.push_compare(ComparisonOperator::NotEqual, events, zero)?;
        let skip_group = self.new_block(&[ValueType::Size, ValueType::Size, ValueType::Size])?;
        self.finish_current(Terminator::BranchIf {
            condition: has_event,
            then_target: probe,
            then_arguments: vec![cursor[0], cursor[1], cursor[2], matching, empty, deleted, sixteen],
            else_target: skip_group,
            else_arguments: cursor[..3].to_vec(),
        })?;
        self.current = skip_group;
        let cursor = self.block_parameter_values(skip_group);
        let incremented = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor[0], sixteen, OverflowBehavior::Wrap)?;
        let next = self.dict_bucket(incremented, capacity)?;
        let attempts = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor[1], sixteen, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![next, attempts, cursor[2], zero, zero, zero, zero],
        })?;

        self.current = probe;
        let cursor = self.block_parameter_values(probe);
        let empty_bit = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitAnd, cursor[4], one, OverflowBehavior::Wrap)?;
        let is_empty = self.push_compare(ComparisonOperator::NotEqual, empty_bit, zero)?;
        self.finish_current(Terminator::BranchIf {
            condition: is_empty,
            then_target: exit,
            then_arguments: vec![missing, cursor[0], cursor[2]],
            else_target: nonempty,
            else_arguments: cursor,
        })?;

        self.current = nonempty;
        let cursor = self.block_parameter_values(nonempty);
        let deleted_bit = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitAnd, cursor[5], one, OverflowBehavior::Wrap)?;
        let is_deleted = self.push_compare(ComparisonOperator::NotEqual, deleted_bit, zero)?;
        let tombstone = self.new_block(&lanes)?;
        self.finish_current(Terminator::BranchIf {
            condition: is_deleted,
            then_target: tombstone,
            then_arguments: cursor.clone(),
            else_target: occupied,
            else_arguments: cursor,
        })?;

        self.current = tombstone;
        let cursor = self.block_parameter_values(tombstone);
        let first_tombstone = self.push_compare(ComparisonOperator::Equal, cursor[2], capacity)?;
        let record_tombstone = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: first_tombstone,
            then_target: record_tombstone,
            then_arguments: Vec::new(),
            else_target: advance,
            else_arguments: cursor.clone(),
        })?;
        self.current = record_tombstone;
        self.finish_current(Terminator::Branch {
            target: advance,
            arguments: vec![cursor[0], cursor[1], cursor[0], cursor[3], cursor[4], cursor[5], cursor[6]],
        })?;

        self.current = occupied;
        let cursor = self.block_parameter_values(occupied);
        let match_bit = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitAnd, cursor[3], one, OverflowBehavior::Wrap)?;
        let candidate = self.push_compare(ComparisonOperator::NotEqual, match_bit, zero)?;
        let check_hash = self.new_block(&lanes)?;
        self.finish_current(Terminator::BranchIf {
            condition: candidate,
            then_target: check_hash,
            then_arguments: cursor.clone(),
            else_target: advance,
            else_arguments: cursor,
        })?;
        self.current = check_hash;
        let cursor = self.block_parameter_values(check_hash);
        let stored_hash = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: stored_hash,
            dict,
            bucket: cursor[0],
            field: DictionarySlotField::Hash,
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        }));
        let same_hash = self.push_compare(ComparisonOperator::Equal, stored_hash, low)?;
        self.finish_current(Terminator::BranchIf {
            condition: same_hash,
            then_target: compare_key,
            then_arguments: cursor.clone(),
            else_target: advance,
            else_arguments: cursor,
        })?;

        self.current = compare_key;
        let cursor = self.block_parameter_values(compare_key);
        let key_value_type = self.field_value_type(key_type)?;
        let element = self.fresh(key_value_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: element,
            dict,
            bucket: cursor[0],
            field: DictionarySlotField::Key,
            transfer: if matches!(key_value_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        }));
        let comparator = self
            .dictionary_comparators
            .iter()
            .find(|(ty, _, _)| ty.unqualified() == key_type.unqualified())
            .map(|(_, name, _)| name.clone());
        let (matched, transferred) = if let Some(comparator) = query_comparator {
            self.lower_dict_key_comparison(comparator, key, element, element)?
        } else if let Some(comparator) = comparator {
            self.lower_dict_key_comparison(&comparator, element, key, element)?
        } else if Self::is_string_type(key_type) {
            (self.lower_string_equality(element, key, false)?, false)
        } else {
            (self.push_compare(ComparisonOperator::Equal, element, key)?, false)
        };
        if matches!(key_value_type, ValueType::Ref(_)) && !transferred {
            self.push_operation(Operation::DropRef { object: element });
        }
        self.finish_current(Terminator::BranchIf {
            condition: matched,
            then_target: exit,
            then_arguments: vec![present, cursor[0], cursor[2]],
            else_target: advance,
            else_arguments: cursor,
        })?;

        self.current = advance;
        let cursor = self.block_parameter_values(advance);
        let incremented = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Add,
            cursor[0],
            one,
            OverflowBehavior::Wrap,
        )?;
        let next = self.dict_bucket(incremented, capacity)?;
        let attempts = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Add,
            cursor[1],
            one,
            OverflowBehavior::Wrap,
        )?;
        let matching = self.push_arithmetic(ValueType::Size, ArithmeticOperator::ShiftRightUnsigned, cursor[3], one, OverflowBehavior::Wrap)?;
        let empty = self.push_arithmetic(ValueType::Size, ArithmeticOperator::ShiftRightUnsigned, cursor[4], one, OverflowBehavior::Wrap)?;
        let deleted = self.push_arithmetic(ValueType::Size, ArithmeticOperator::ShiftRightUnsigned, cursor[5], one, OverflowBehavior::Wrap)?;
        let remaining = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, cursor[6], one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![next, attempts, cursor[2], matching, empty, deleted, remaining],
        })?;

        self.current = exit;
        let result = self.block_parameter_values(exit);
        Ok((result[0], result[1], result[2], hash))
    }

    /// After a successful miss and a rehash, find storage for the new key
    /// without invoking `Eq` again. The first lookup already established that
    /// no equal key exists; a second comparator call could have effects.
    fn lower_dict_vacant_bucket(
        &mut self,
        dict: ValueId,
        hash: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: capacity,
            dict,
            field: DictionaryHeaderField::Capacity,
        }));
        let mask = self.push_const_int(0xffff_ffff)?;
        let low = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::BitAnd,
            hash,
            mask,
            OverflowBehavior::Wrap,
        )?;
        let low_size = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: low_size, value: low });
        let first = self.dict_bucket(low_size, capacity)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let header = self.new_block(&[ValueType::Size, ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size, ValueType::Size])?;
        let advance = self.new_block(&[ValueType::Size, ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![first, zero] })?;
        self.current = header;
        let cursor = self.block_parameter_values(header);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor[1], capacity)?;
        self.push_guard(more, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.finish_current(Terminator::Branch { target: body, arguments: cursor })?;
        self.current = body;
        let cursor = self.block_parameter_values(body);
        let state = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadSlot {
            destination: state,
            dict,
            bucket: cursor[0],
            field: DictionarySlotField::State,
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        }));
        let full = self.push_compare(ComparisonOperator::Equal, state, one)?;
        self.finish_current(Terminator::BranchIf {
            condition: full,
            then_target: advance,
            then_arguments: cursor.clone(),
            else_target: exit,
            else_arguments: vec![cursor[0]],
        })?;
        self.current = advance;
        let cursor = self.block_parameter_values(advance);
        let incremented = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor[0], one, OverflowBehavior::Wrap)?;
        let next = self.dict_bucket(incremented, capacity)?;
        let attempts = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor[1], one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next, attempts] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    fn lower_dict_reusable_bucket(
        &mut self,
        empty: ValueId,
        tombstone: ValueId,
        capacity: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let use_tombstone = self.push_compare(ComparisonOperator::LessUnsigned, tombstone, capacity)?;
        let tomb = self.new_block(&[])?;
        let vacant = self.new_block(&[])?;
        let join = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::BranchIf {
            condition: use_tombstone,
            then_target: tomb,
            then_arguments: Vec::new(),
            else_target: vacant,
            else_arguments: Vec::new(),
        })?;
        self.current = tomb;
        self.finish_current(Terminator::Branch { target: join, arguments: vec![tombstone] })?;
        self.current = vacant;
        let has_empty = self.push_compare(ComparisonOperator::LessUnsigned, empty, capacity)?;
        self.push_guard(has_empty, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.finish_current(Terminator::Branch { target: join, arguments: vec![empty] })?;
        self.current = join;
        Ok(self.block_parameter_values(join)[0])
    }

    fn lower_dict_store_operand(
        &mut self,
        value: ValueId,
        source_type: &Type,
    ) -> Result<(ValueId, witchy_wir::canonical_safewir::StoreTransfer), CanonicalLoweringError> {
        let expected = self.field_value_type(source_type)?;
        if self.values[&value] != expected {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict table slot type"));
        }
        match expected {
            ValueType::Ref(_) => Ok((
                self.owned_for_storage(value, source_type)?,
                witchy_wir::canonical_safewir::StoreTransfer::MoveUnique,
            )),
            ValueType::Gc(_) => Err(CanonicalLoweringError::UnsupportedExpression("dict table GC slot")),
            _ => Ok((value, witchy_wir::canonical_safewir::StoreTransfer::CopyScalar)),
        }
    }

    /// Leave one quarter of the table vacant. This bounds linear-probe length
    /// while avoiding the extra allocation caused by a half-full threshold.
    fn dict_growth_limit_reached(
        &mut self,
        used: ValueId,
        capacity: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let two = self.push_const_size(2)?;
        let quarter = self.push_arithmetic(
            ValueType::Size, ArithmeticOperator::ShiftRightUnsigned,
            capacity, two, OverflowBehavior::Wrap,
        )?;
        let limit = self.push_arithmetic(
            ValueType::Size, ArithmeticOperator::Subtract,
            capacity, quarter, OverflowBehavior::Wrap,
        )?;
        self.push_compare(ComparisonOperator::GreaterEqualUnsigned, used, limit)
    }

    /// Upsert on an owned guest table. A shared normal-mode snapshot is
    /// copied once before probing; unique roots mutate in place. Growth
    /// rehashes only after the single semantic `Eq` search has missed.
    pub(super) fn lower_dict_insert_table(
        &mut self,
        dict: ValueId,
        key_type: &Type,
        value_type: &Type,
        key: ValueId,
        value: ValueId,
        extract: bool,
    ) -> Result<(ValueId, Option<ValueId>), CanonicalLoweringError> {
        let dict = if self.known_shared_value(dict) {
            self.lower_dict_clone_exact(dict)?
        } else {
            dict
        };
        let lookup = self.lower_dict_find_table(dict, key, key_type)?;
        self.lower_dict_insert_table_found(dict, key_type, value_type, key, value, extract, lookup)
    }

    fn lower_dict_insert_table_found(
        &mut self,
        dict: ValueId,
        key_type: &Type,
        value_type: &Type,
        key: ValueId,
        value: ValueId,
        extract: bool,
        lookup: (ValueId, ValueId, ValueId, ValueId),
    ) -> Result<(ValueId, Option<ValueId>), CanonicalLoweringError> {
        let (found, empty, tombstone, hash) = lookup;
        let option_type = Type::Named("Option".into(), vec![value_type.clone()]);
        let option_shape = extract.then(|| scalar_aggregate_shape(&option_type, self.definitions)).flatten();
        let mut join_shapes = vec![(self.values[&dict], Some(Ownership::Unique))];
        if extract {
            join_shapes.push((self.field_value_type(&option_type)?, Some(Ownership::Unique)));
        }
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        let hit = self.new_block(&[])?;
        let miss = self.new_block(&[])?;
        let join = self.new_block_owned(&join_shapes)?;
        self.finish_current(Terminator::BranchIf {
            condition: found,
            then_target: hit,
            then_arguments: Vec::new(),
            else_target: miss,
            else_arguments: Vec::new(),
        })?;

        self.current = hit;
        let (stored_value, transfer) = self.lower_dict_store_operand(value, value_type)?;
        let displaced_type = self.field_value_type(value_type)?;
        let displaced = self.fresh(displaced_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReplaceValue {
            displaced,
            dict,
            bucket: empty,
            value: stored_value,
            transfer,
        }));
        let mut result = vec![dict];
        if let Some(option_shape) = &option_shape {
            result.push(self.lower_aggregate_for_source(&option_type, option_shape, 0, vec![displaced])?);
        } else if matches!(self.values[&displaced], ValueType::Ref(_)) {
            self.push_operation(Operation::DropRef { object: displaced });
        }
        self.finish_current(Terminator::Branch { target: join, arguments: result })?;

        self.current = miss;
        self.restore_snapshot(facts.clone());
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: count,
            dict,
            field: DictionaryHeaderField::Count,
        }));
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: capacity,
            dict,
            field: DictionaryHeaderField::Capacity,
        }));
        let tombstones = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::ReadHeader {
            destination: tombstones,
            dict,
            field: DictionaryHeaderField::Tombstones,
        }));
        let occupied = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, count, tombstones, OverflowBehavior::Trap)?;
        let needs_growth = self.dict_growth_limit_reached(occupied, capacity)?;
        let grow = self.new_block(&[])?;
        let room = self.new_block(&[])?;
        let ready = self.new_block_owned(&[(self.values[&dict], Some(Ownership::Unique)), (ValueType::Size, None)])?;
        let miss_facts = self.snapshot_facts();
        self.finish_current(Terminator::BranchIf {
            condition: needs_growth,
            then_target: grow,
            then_arguments: Vec::new(),
            else_target: room,
            else_arguments: Vec::new(),
        })?;
        self.current = grow;
        let live_needs_growth = self.dict_growth_limit_reached(count, capacity)?;
        let double = self.new_block(&[])?;
        let compact = self.new_block(&[])?;
        let target_capacity = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::BranchIf {
            condition: live_needs_growth,
            then_target: double,
            then_arguments: Vec::new(),
            else_target: compact,
            else_arguments: Vec::new(),
        })?;
        self.current = double;
        let doubled = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, capacity, capacity, OverflowBehavior::Trap)?;
        let small_limit = self.push_const_size(4096)?;
        let small = self.push_compare(ComparisonOperator::LessUnsigned, capacity, small_limit)?;
        let quadruple = self.new_block(&[])?;
        let use_double = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: small,
            then_target: quadruple,
            then_arguments: Vec::new(),
            else_target: use_double,
            else_arguments: Vec::new(),
        })?;
        self.current = quadruple;
        let quadrupled = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, doubled, doubled, OverflowBehavior::Trap)?;
        self.finish_current(Terminator::Branch { target: target_capacity, arguments: vec![quadrupled] })?;
        self.current = use_double;
        self.finish_current(Terminator::Branch { target: target_capacity, arguments: vec![doubled] })?;
        self.current = compact;
        self.finish_current(Terminator::Branch { target: target_capacity, arguments: vec![capacity] })?;
        self.current = target_capacity;
        let selected_capacity = self.block_parameter_values(target_capacity)[0];
        let grown = self.fresh(self.values[&dict])?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::Rehash {
            destination: grown,
            source: dict,
            capacity: selected_capacity,
            transfer: DictionaryRehashTransfer::MoveUnique,
        }));
        let bucket = self.lower_dict_vacant_bucket(grown, hash)?;
        self.finish_current(Terminator::Branch { target: ready, arguments: vec![grown, bucket] })?;
        self.current = room;
        self.restore_snapshot(miss_facts.clone());
        let bucket = self.lower_dict_reusable_bucket(empty, tombstone, capacity)?;
        self.finish_current(Terminator::Branch { target: ready, arguments: vec![dict, bucket] })?;
        self.restore_facts(miss_facts, ready);
        self.current = ready;
        let ready_values = self.block_parameter_values(ready);
        let (stored_key, key_transfer) = self.lower_dict_store_operand(key, key_type)?;
        let (stored_value, value_transfer) = self.lower_dict_store_operand(value, value_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::InsertSlot {
            dict: ready_values[0],
            bucket: ready_values[1],
            key: stored_key,
            value: stored_value,
            hash,
            key_transfer,
            value_transfer,
        }));
        let mut result = vec![ready_values[0]];
        if let Some(option_shape) = &option_shape {
            result.push(self.lower_aggregate_for_source(&option_type, option_shape, 1, Vec::new())?);
        }
        self.finish_current(Terminator::Branch { target: join, arguments: result })?;
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let joined = self.block_parameter_values(join);
        Ok((joined[0], joined.get(1).copied()))
    }

    pub(super) fn lower_dict_remove_table(
        &mut self,
        dict: ValueId,
        key_type: &Type,
        value_type: &Type,
        key: ValueId,
        extract: bool,
    ) -> Result<(ValueId, Option<ValueId>), CanonicalLoweringError> {
        let dict = if self.known_shared_value(dict) {
            self.lower_dict_clone_exact(dict)?
        } else {
            dict
        };
        let (found, bucket, _, _) = self.lower_dict_find_table(dict, key, key_type)?;
        let option_type = Type::Named("Option".into(), vec![value_type.clone()]);
        let option_shape = extract.then(|| scalar_aggregate_shape(&option_type, self.definitions)).flatten();
        let mut join_shapes = vec![(self.values[&dict], Some(Ownership::Unique))];
        if extract {
            join_shapes.push((self.field_value_type(&option_type)?, Some(Ownership::Unique)));
        }
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        let hit = self.new_block(&[])?;
        let miss = self.new_block(&[])?;
        let join = self.new_block_owned(&join_shapes)?;
        self.finish_current(Terminator::BranchIf {
            condition: found,
            then_target: hit,
            then_arguments: Vec::new(),
            else_target: miss,
            else_arguments: Vec::new(),
        })?;

        self.current = hit;
        let removed_type = self.field_value_type(value_type)?;
        let removed_value = self.fresh(removed_type)?;
        self.push_operation(Operation::Dictionary(DictionaryOperation::RemoveSlot {
            removed_value,
            dict,
            bucket,
        }));
        let mut result = vec![dict];
        if let Some(option_shape) = &option_shape {
            result.push(self.lower_aggregate_for_source(&option_type, option_shape, 0, vec![removed_value])?);
        } else if matches!(removed_type, ValueType::Ref(_)) {
            self.push_operation(Operation::DropRef { object: removed_value });
        }
        self.finish_current(Terminator::Branch { target: join, arguments: result })?;

        self.current = miss;
        self.restore_snapshot(facts.clone());
        let mut result = vec![dict];
        if let Some(option_shape) = &option_shape {
            result.push(self.lower_aggregate_for_source(&option_type, option_shape, 1, Vec::new())?);
        }
        self.finish_current(Terminator::Branch { target: join, arguments: result })?;
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let joined = self.block_parameter_values(join);
        Ok((joined[0], joined.get(1).copied()))
    }

    fn lower_dict_update_table(
        &mut self,
        dict: ValueId,
        key_type: &Type,
        value_type: &Type,
        key: ValueId,
        default: ValueId,
        callback: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.closure_layout_for_value(callback)?;
        let signature_id = *self.closures.signature_by_layout.get(&layout)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("closure signature"))?;
        let declaration_id = *self.closures.declaration_by_layout.get(&layout)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("closure signature"))?;
        let expected = self.field_value_type(value_type)?;
        let declaration = self.signatures.get(declaration_id.get() as usize)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("closure signature"))?;
        if declaration.parameters != [expected] || declaration.results != [expected]
            || write_back_count(declaration) != 0
        {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict.update callback signature"));
        }
        let dict = if self.known_shared_value(dict) {
            self.lower_dict_clone_exact(dict)?
        } else {
            dict
        };
        let lookup = self.lower_dict_find_table(dict, key, key_type)?;
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        let hit = self.new_block(&[])?;
        let miss = self.new_block(&[])?;
        let join = self.dict_value_join(value_type)?;
        self.finish_current(Terminator::BranchIf {
            condition: lookup.0,
            then_target: hit,
            then_arguments: Vec::new(),
            else_target: miss,
            else_arguments: Vec::new(),
        })?;
        self.current = hit;
        let present = self.lower_dict_table_value_at(dict, lookup.1, value_type)?;
        let lanes = self.dict_value_lanes(present, value_type)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = miss;
        self.restore_snapshot(facts.clone());
        let absent = self.owned_dict_value(default, value_type)?;
        let lanes = self.dict_value_lanes(absent, value_type)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let selected = self.block_parameter_values(join);
        self.track_accumulator_lanes(&selected);
        let argument = if matches!(expected, ValueType::Ref(_) | ValueType::Gc(_)) {
            CallArgument::Move(self.clone_reference_token(selected[0])?)
        } else {
            CallArgument::Value(selected[0])
        };
        let results = self.emit_closure_call(callback, signature_id, vec![argument])?;
        let [updated] = results.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict.update callback result"));
        };
        let (rebuilt, _) = self.lower_dict_insert_table_found(
            dict, key_type, value_type, key, *updated, false, lookup,
        )?;
        Ok(rebuilt)
    }
}
