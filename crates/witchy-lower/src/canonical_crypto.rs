//! Catalog-selected pure crypto over bounded byte slices. Managed list storage
//! never crosses this ABI: RuneHash constructs a checked, pointer-free frame.
use super::*;
use witchy_syntax::intrinsics::{CRYPTO_ED25519_VERIFY_STATUS, CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS, CRYPTO_SHA256, CRYPTO_SHA256_BYTES, CRYPTO_RUNE_HASH};
use witchy_wir::canonical_safewir::{CRYPTO_ED25519_VERIFY_IMPORT, CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT, CRYPTO_SHA256_IMPORT, CRYPTO_RUNE_HASH_IMPORT};
use witchy_syntax::intrinsics::{CRYPTO_SHA512, CRYPTO_SHA3_256, CRYPTO_HMAC_SHA256, CRYPTO_SHAKE128, CRYPTO_SHAKE256, CRYPTO_ECDSA_P256_VERIFY_STATUS, CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS};
use witchy_wir::canonical_safewir::{CRYPTO_SHA512_IMPORT, CRYPTO_SHA3_256_IMPORT, CRYPTO_HMAC_SHA256_IMPORT, CRYPTO_SHAKE128_IMPORT, CRYPTO_SHAKE256_IMPORT, CRYPTO_ECDSA_P256_VERIFY_IMPORT, CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT, CRYPTO_EXTENSION_IMPORT_NAMES};

pub(super) fn is_intrinsic(name: &str) -> bool {
    matches!(name, CRYPTO_ED25519_VERIFY_STATUS | CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS | CRYPTO_SHA256 | CRYPTO_SHA256_BYTES | CRYPTO_RUNE_HASH
        | CRYPTO_SHA512 | CRYPTO_SHA3_256 | CRYPTO_HMAC_SHA256 | CRYPTO_SHAKE128 | CRYPTO_SHAKE256
        | CRYPTO_ECDSA_P256_VERIFY_STATUS | CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS)
}
pub(super) fn is_host_import(name: &str) -> bool {
    matches!(name, CRYPTO_ED25519_VERIFY_IMPORT | CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT | CRYPTO_SHA256_IMPORT | CRYPTO_RUNE_HASH_IMPORT)
        || CRYPTO_EXTENSION_IMPORT_NAMES.contains(&name)
}
pub(super) fn is_needed(module: &witchy_syntax::ast::Module) -> bool {
    let scope = witchy_types::pipeline::canonical_scope(module);
    let mut needed = false;
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression && is_intrinsic(name) { needed = true; }
            Ok(())
        }).expect("crypto import visitor cannot fail");
    }
    needed
}

pub(super) fn catalog(mut catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>,
    bytes: witchy_wir::layout::LayoutId) -> Result<CanonicalAbiCatalog, CanonicalLoweringError>
{
    let buffer = ValueType::Ref(bytes);
    let mut ids = [FunctionSignatureId::new(0); 5];
    for (index, (parameters, result, write)) in [
        (vec![buffer; 3], ValueType::Int, None),
        (vec![buffer; 2], ValueType::Size, Some(1)),
        (vec![buffer; 2], ValueType::Size, Some(1)),
        (vec![buffer; 3], ValueType::Size, Some(2)),
        (vec![buffer, ValueType::Int, buffer], ValueType::Size, Some(2)),
    ].into_iter().enumerate()
    {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let contract = CallableContract {
            parameters: parameters.iter().map(|ty| ParameterContract { value_type: *ty, ownership: ParameterOwnership::Value }).collect(),
            results: vec![ResultContract::Value(result)],
            parameter_effects: parameters.iter().enumerate().filter_map(|(slot, ty)|
                matches!(ty, ValueType::Ref(_)).then_some((slot as u32,
                    vec![if write == Some(slot) { MemoryEffect::Write } else { MemoryEffect::Read }]))).collect(),
        };
        signatures.push(FunctionSignature { id, parameters, results: vec![result], contract });
        ids[index] = id;
    }
    catalog = catalog.with_ed25519_verify_import(ids[0], bytes)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    // Both verifiers have the same three-read-slice signature; their separately
    // authenticated import identities select the cryptographic implementation.
    catalog = catalog.with_rsa_pkcs1_sha256_verify_import(ids[0], bytes)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    catalog = catalog.with_crypto_hash_imports([ids[1], ids[2]], bytes)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    catalog.with_crypto_extension_imports([ids[0], ids[1], ids[3], ids[4]], bytes)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_crypto(&mut self, name: &str, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let arity = match name { CRYPTO_ED25519_VERIFY_STATUS | CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS
                | CRYPTO_ECDSA_P256_VERIFY_STATUS | CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS => 3,
            CRYPTO_SHA256 | CRYPTO_SHA256_BYTES | CRYPTO_SHA512 | CRYPTO_SHA3_256 => 1,
            CRYPTO_RUNE_HASH | CRYPTO_HMAC_SHA256 | CRYPTO_SHAKE128 | CRYPTO_SHAKE256 => 2,
            _ => return Err(CanonicalLoweringError::MissingBinding(name.to_owned())) };
        if args.len() != arity {
            return Err(CanonicalLoweringError::CallArityMismatch { callee: name.to_owned(), expected: arity, actual: args.len() });
        }
        let layout = self.string_layout_id()?;
        if matches!(name, CRYPTO_ED25519_VERIFY_STATUS | CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS
            | CRYPTO_ECDSA_P256_VERIFY_STATUS | CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS)
        {
            let import = match name {
                CRYPTO_ED25519_VERIFY_STATUS => self.host_imports.ed25519_verify,
                CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS => self.host_imports.rsa_pkcs1_sha256_verify,
                CRYPTO_ECDSA_P256_VERIFY_STATUS => self.host_imports.crypto_extensions.get(CRYPTO_ECDSA_P256_VERIFY_IMPORT).copied(),
                _ => self.host_imports.crypto_extensions.get(CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT).copied(),
            }.ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated crypto verification import"))?;
            let mut snapshots = Vec::new();
            for argument in args {
                let value = self.lower_string_argument(argument)?;
                snapshots.push(self.clone_reference_token(value)?);
            }
            // Materialize extents after all effects, while retaining the value
            // of each argument from its original evaluation point.
            let inputs = snapshots.into_iter().map(|snapshot|
                self.string_slice_argument(snapshot, layout)).collect::<Result<Vec<_>, _>>()?;
            let result = self.fresh(ValueType::Int)?;
            self.push_operation(Operation::HostCall { destinations: vec![result], import, arguments: inputs });
            return Ok(result);
        }
        if matches!(name, CRYPTO_SHA512 | CRYPTO_SHA3_256 | CRYPTO_HMAC_SHA256 | CRYPTO_SHAKE128 | CRYPTO_SHAKE256) {
            return self.lower_crypto_extension_digest(name, args);
        }
        let (input, import, length) = if matches!(name, CRYPTO_SHA256 | CRYPTO_SHA256_BYTES) {
            let value = if name == CRYPTO_SHA256_BYTES {
                // Bytes share the counted buffer layout, not String's UTF-8
                // semantics. Do not decode or materialize text before hashing.
                let value = self.lower_expr(&args[0])?;
                if self.values.get(&value).copied() != Some(ValueType::Ref(layout)) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("crypto byte buffer representation"));
                }
                value
            } else {
                self.lower_string_argument(&args[0])?
            };
            let input = self.clone_reference_token(value)?;
            (input, self.host_imports.sha256, 64)
        } else {
            // Snapshot each argument before evaluating the next one. In
            // particular, a var call in contents cannot change the paths seen
            // by this invocation; serialization happens after both evaluations.
            let paths = self.lower_expr(&args[0])?;
            let paths = self.clone_reference_token(paths)?;
            self.ensure_list_extent(paths)?;
            let contents = self.lower_expr(&args[1])?;
            let contents = self.clone_reference_token(contents)?;
            self.ensure_list_extent(contents)?;
            let zero = self.push_const_size(0)?;
            let total = self.crypto_frame_length(paths, zero)?;
            let total = self.crypto_frame_length(contents, total)?;
            let capacity = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::MoveValue { destination: capacity, source: total });
            let frame = self.alloc_bytes(total, capacity)?;
            let cursor = self.crypto_frame_array(frame, total, paths, zero)?;
            let cursor = self.crypto_frame_array(frame, total, contents, cursor)?;
            let complete = self.push_compare(ComparisonOperator::Equal, cursor, total)?;
            // Frame consistency is a semantic assertion, not a range fact.
            // Keep it out of the checker's unsigned bounds-guard vocabulary.
            self.push_trap_unless(complete)?;
            (frame, self.host_imports.rune_hash, 71)
        };
        let import = import.ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated crypto hash import"))?;
        let length = self.push_const_size(length)?;
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let output = self.alloc_bytes(length, capacity)?;
        let read = self.string_slice_argument(input, layout)?;
        let zero = self.push_const_size(0)?;
        let write = HostCallArgument::Buffer { place: witchy_wir::canonical_safewir::Place::projected(output,
            Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write), extent: HostBufferExtent::Slice };
        let required = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![required], import, arguments: vec![read, write] });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, required, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: required });
        self.string_lengths.insert(output, required);
        self.string_capacities.insert(output, capacity);
        Ok(output)
    }

    fn lower_crypto_extension_digest(&mut self, name: &str, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let shake = matches!(name, CRYPTO_SHAKE128 | CRYPTO_SHAKE256);
        let input = if shake { self.lower_expr(&args[0])? } else { self.lower_string_argument(&args[0])? };
        if self.values.get(&input).copied() != Some(ValueType::Ref(layout)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("crypto byte buffer representation"));
        }
        let input = self.clone_reference_token(input)?;
        let second = if name == CRYPTO_HMAC_SHA256 {
            let value = self.lower_string_argument(&args[1])?;
            Some(self.clone_reference_token(value)?)
        } else if shake {
            Some(self.lower_expr(&args[1])?)
        } else { None };
        let length = if shake {
            // The public wrapper returns a typed Err; the private intrinsic
            // independently rejects invalid Int lengths before any allocation.
            self.int_to_size_checked(second.expect("SHAKE length"), 1_048_576)?
        } else { self.push_const_size(if name == CRYPTO_SHA512 { 128 } else { 64 })? };
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let zero = self.push_const_size(0)?;
        let output = self.alloc_bytes(zero, capacity)?;
        let mut arguments = vec![self.string_slice_argument(input, layout)?];
        if let Some(second) = second {
            arguments.push(if shake { HostCallArgument::Value(second) }
                else { self.string_slice_argument(second, layout)? });
        }
        arguments.push(HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(output,
                Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
            extent: HostBufferExtent::Slice,
        });
        let import_name = match name {
            CRYPTO_SHA512 => CRYPTO_SHA512_IMPORT,
            CRYPTO_SHA3_256 => CRYPTO_SHA3_256_IMPORT,
            CRYPTO_HMAC_SHA256 => CRYPTO_HMAC_SHA256_IMPORT,
            CRYPTO_SHAKE128 => CRYPTO_SHAKE128_IMPORT,
            _ => CRYPTO_SHAKE256_IMPORT,
        };
        let import = self.host_imports.crypto_extensions.get(import_name).copied()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated crypto extension import"))?;
        let required = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![required], import, arguments });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, required, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: required });
        self.string_lengths.insert(output, required);
        self.string_capacities.insert(output, capacity);
        Ok(output)
    }

    fn crypto_frame_length(&mut self, list: ValueId, initial: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let four = self.push_const_size(4)?;
        let initial = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, initial, four, OverflowBehavior::Trap)?;
        self.crypto_string_array_fold(list, initial, |this, total, string| {
            let length = this.string_length_of(string)?;
            let four = this.push_const_size(4)?;
            let total = this.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, total, four, OverflowBehavior::Trap)?;
            this.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, total, length, OverflowBehavior::Trap)
        })
    }

    fn crypto_frame_array(&mut self, frame: ValueId, total: ValueId, list: ValueId, cursor: ValueId)
        -> Result<ValueId, CanonicalLoweringError>
    {
        let count = self.list_counts[&list];
        let cursor = self.crypto_frame_word(frame, total, cursor, count)?;
        self.crypto_string_array_fold(list, cursor, |this, cursor, string| {
            let length = this.string_length_of(string)?;
            let cursor = this.crypto_frame_word(frame, total, cursor, length)?;
            let end = this.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, length, OverflowBehavior::Trap)?;
            let fits = this.push_compare(ComparisonOperator::LessEqualUnsigned, end, total)?;
            this.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
            let zero = this.push_const_size(0)?;
            let layout = this.string_layout_id()?;
            this.push_byte_copy(frame, cursor, string, zero, length, layout);
            Ok(end)
        })
    }

    fn crypto_frame_word(&mut self, frame: ValueId, total: ValueId, mut cursor: ValueId, word: ValueId)
        -> Result<ValueId, CanonicalLoweringError>
    {
        for shift in [0, 8, 16, 24] {
            let shift = self.push_const_size(shift)?;
            let shifted = self.push_arithmetic(ValueType::Size, ArithmeticOperator::ShiftRightUnsigned, word, shift, OverflowBehavior::Wrap)?;
            let mask = self.push_const_size(255)?;
            let byte = self.push_arithmetic(ValueType::Size, ArithmeticOperator::BitAnd, shifted, mask, OverflowBehavior::Wrap)?;
            let byte = self.proven_byte(byte)?;
            let fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, total)?;
            self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
            self.push_byte_store(frame, cursor, byte)?;
            let one = self.push_const_size(1)?;
            cursor = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Trap)?;
        }
        Ok(cursor)
    }

    fn crypto_string_array_fold(&mut self, list: ValueId, initial: ValueId,
        mut fold: impl FnMut(&mut Self, ValueId, ValueId) -> Result<ValueId, CanonicalLoweringError>)
        -> Result<ValueId, CanonicalLoweringError>
    {
        let count = self.list_counts[&list];
        let layout = self.string_layout_id()?;
        let zero = self.push_const_size(0)?;
        let facts = self.snapshot_facts();
        let header = self.new_block(&[ValueType::Size, ValueType::Size])?;
        let body = self.new_block(&[])?;
        let exit = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero, initial] })?;
        self.current = header;
        let state = self.block_parameter_values(header);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, state[0], count)?;
        self.finish_current(Terminator::BranchIf { condition: more, then_target: body, then_arguments: vec![], else_target: exit, else_arguments: vec![state[1]] })?;
        self.current = body;
        let string = self.load_list_element(list, state[0], ValueType::Ref(layout))?;
        let total = fold(self, state[1], string)?;
        let one = self.push_const_size(1)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, state[0], one, OverflowBehavior::Trap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next, total] })?;
        self.restore_facts(facts, exit);
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }
}
