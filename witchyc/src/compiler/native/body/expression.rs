use super::{Block, Body, I64, InstBuilder, Result, TRAP, Value, error};
use crate::compiler::logical_body::{Binary, Bits, Expr, Ty, Value as Logical};
use cranelift_module::Module;

impl Body<'_, '_> {
    pub(super) fn expr(&mut self, expr: &Expr) -> Result<Vec<Value>> {
        let lanes = self.layout.lanes(expr.ty)?;
        use crate::compiler::host::HostOp;
        let value = match &expr.kind {
            Logical::Unit => return Ok(vec![]),
            Logical::U64(n) => self.fb.ins().iconst(I64, *n as i64),
            Logical::Bool(b) => self.fb.ins().iconst(I64, i64::from(*b)),
            Logical::EntryConsole => self.entry_grant(1)?,
            // The checked memory.share form uses these singleton operations
            // after requiring a borrow of the corresponding grant.
            Logical::EntryAllocator if self.entry_grants.is_none() => self.constant(2),
            Logical::EntryAllocator => self.entry_grant(2)?,
            Logical::TryAlloc {
                allocator,
                capacity,
                type_id,
                prefix_only,
            } => {
                return self.allocation(expr.ty, allocator, capacity, *type_id, *prefix_only);
            }
            Logical::Capacity(value, type_id) => self.capacity(value, *type_id)?,
            Logical::AllocationKey(value, type_id) => { self.not_dense(Ty::Buffer(type_id.checked_sub(1).ok_or_else(|| error("missing native buffer type"))?))?; self.handle(value)? }
            Logical::TryRelease(value, type_id) => return self.release(expr.ty, value, *type_id),
            Logical::Storage {
                op,
                receiver,
                index,
                value,
                buffer,
            } => {
                return self.storage(expr.ty, *op, receiver, index, value.as_deref(), *buffer);
            }
            Logical::Exchange(left, right) => {
                let lhs = self.locate(left)?;
                let rhs = self.locate(right)?;
                let a = self.read_location(lhs)?;
                let b = self.read_location(rhs)?;
                self.reads.clear();
                self.write_location(lhs, &b)?;
                self.write_location(rhs, &a)?;
                return Ok(vec![]);
            }
            Logical::ReadPlace(place)
            | Logical::Move { place, .. }
            | Logical::Extract { place, .. } => {
                if lanes == 0 {
                    return Ok(vec![]);
                }
                if let Some(value) = self.read_shared(place)? {
                    return Ok(vec![value]);
                }
                let location = self.locate(place)?;
                return self.read_location(location);
            }
            Logical::Address(place) if lanes == 1 => self.place(place)?,
            Logical::Stored(place, inner) if lanes == 1 => {
                let values = self.expr(inner)?;
                let addr = self.place(place)?;
                self.reads.clear();
                self.store_typed(addr, inner.ty, &values)?;
                addr
            }
            Logical::Discard(inner) => {
                self.expr(inner)?;
                return Ok(vec![]);
            }
            Logical::StaticStr(text) => {
                use cranelift_module::DataDescription;

                let data = if let Some(&data) = self.literals.get(text) {
                    data
                } else {
                    let data = self.module.declare_anonymous_data(false, false).map_err(error)?;
                    let mut description = DataDescription::new();
                    // An empty slice still has a valid address.
                    description.define(if text.is_empty() {
                        Box::new([0])
                    } else {
                        text.as_bytes().into()
                    });
                    self.module.define_data(data, &description).map_err(error)?;
                    self.literals.insert(text.clone(), data);
                    data
                };
                let global = self.module.declare_data_in_func(data, self.fb.func);
                let address = self.fb.ins().symbol_value(I64, global);
                return Ok(vec![address, self.constant(text.len() as u64)]);
            }
            Logical::Widen(inner) | Logical::ProofUtf8(inner) => return self.expr(inner),
            Logical::Float(op, operands) => self.float(*op, operands)?,
            Logical::Signed(op, operands) => self.signed(*op, operands)?,
            Logical::Sum { arm, payload } => {
                let payload = self.abi_lanes(payload)?;
                return self.sum_value(expr.ty, *arm, payload);
            }
            Logical::Retag(inner, map) => return self.retag(expr.ty, inner, map),
            Logical::Aggregate(fields) => {
                let mut values = Vec::with_capacity(lanes as usize);
                for field in fields {
                    values.extend(self.abi_lanes(field)?);
                }
                return Ok(values);
            }
            Logical::Repeat(inner, count) => {
                let values = self.abi_lanes(inner)?;
                return Ok(values.repeat(*count as usize));
            }
            Logical::Project(inner, ordinal) => {
                let offset = self.layout.field(inner.ty, *ordinal)?.lane_offset;
                let values = self.expr(inner)?;
                return Ok(values[offset as usize..(offset + lanes) as usize].to_vec());
            }
            Logical::Index(inner, index, length) => {
                self.layout.array(inner.ty, *length)?;
                let values = self.expr(inner)?;
                let addr = self.scratch(inner.ty)?;
                self.store_typed(addr, inner.ty, &values)?;
                let index = self.one(index)?;
                let len = (!self.abi.proved_array_bounds)
                    .then(|| self.fb.ins().iconst(I64, i64::from(*length)));
                let addr = self.index_address(addr, index, len, expr.ty)?;
                return self.load_expression(addr, expr.ty);
            }
            Logical::SliceOf(place, length) => {
                self.layout.array(place.ty, *length)?;
                let addr = self.place(place)?;
                let len = self.fb.ins().iconst(I64, i64::from(*length));
                return Ok(vec![addr, len]);
            }
            Logical::SliceLen(slice) => self.slice_word(slice, 1)?,
            Logical::SliceIndex(slice, index, tested) => {
                // An untested read is one a proved length lets through (`counted_container`):
                // checked mode tests it like any other.
                let (addr, length) = if (*tested || super::super::checked()) && !self.abi.proved_slice_bounds {
                    let (addr, length) = self.slice(slice)?;
                    (addr, Some(length))
                } else {
                    (self.slice_word(slice, 0)?, None)
                };
                let index = self.one(index)?;
                let addr = self.index_address(addr, index, length, expr.ty)?;
                return self.load_expression(addr, expr.ty);
            }
            Logical::Call(call) => return self.emit_call(call, None),
            Logical::Print(receiver, value, word) => {
                let addr = self.place(receiver)?;
                let token = self.load_word(addr);
                let number = self.one(value)?;
                let mode = if *word {
                    1
                } else if value.ty == Ty::Unsigned(crate::compiler::scalar::Scalar::U8) {
                    0
                } else {
                    2
                };
                let mode = self.fb.ins().iconst(I64, mode);
                let print = self.module.declare_func_in_func(self.print, self.fb.func);
                self.reads.clear();
                self.fb.ins().call(print, &[token, number, mode]);
                return Ok(vec![]);
            }
            Logical::Not(inner) => {
                let inner = self.one(inner)?;
                self.fb.ins().bxor_imm_s(inner, 1)
            }
            Logical::Binary(op @ (Binary::And | Binary::Or), left, right) => {
                self.short_circuit(*op, left, right)?
            }
            Logical::Binary(op @ (Binary::Div | Binary::Rem), left, right)
                if let Logical::U64(divisor) = right.kind && divisor != 0 =>
            {
                let dividend = self.one(left)?;
                self.unsigned_constant_division(*op, dividend, divisor)
            }
            Logical::Binary(op, left, right) => {
                let left = self.one(left)?;
                let right = self.one(right)?;
                self.binary_typed(*op, left, right, expr.ty, false)
            }
            Logical::Bits(Bits::Packed(packed), operands) => return self.packed(*packed, operands),
            Logical::Bits(op, operands) => {
                let left = self.one(&operands[0])?;
                let Ty::Unsigned(scalar) = expr.ty else {
                    return Err(error("expected unsigned bit operation"));
                };
                let width = scalar.layout();
                let result = if *op == Bits::Not {
                    self.fb.ins().bnot(left)
                } else if *op == Bits::TrailingZeros {
                    self.fb.ins().ctz(left)
                } else if *op == Bits::CountOnes {
                    self.fb.ins().popcnt(left)
                } else {
                    let right = self.one(&operands[1])?;
                    match op {
                        Bits::And => self.fb.ins().band(left, right),
                        Bits::Or => self.fb.ins().bor(left, right),
                        Bits::Xor => self.fb.ins().bxor(left, right),
                        Bits::Shl | Bits::Shr => {
                            let past = self.fb.ins().icmp_imm_s(
                                super::IntCC::UnsignedGreaterThanOrEqual,
                                right,
                                i64::from(width.bits),
                            );
                            self.fb.ins().trapnz(past, TRAP);
                            if *op == Bits::Shl {
                                self.fb.ins().ishl(left, right)
                            } else {
                                self.fb.ins().ushr(left, right)
                            }
                        }
                        Bits::WrapAdd => self.fb.ins().iadd(left, right),
                        Bits::WrapSub => self.fb.ins().isub(left, right),
                        Bits::WrapMul => self.fb.ins().imul(left, right),
                        Bits::MulHigh => self.fb.ins().umulhi(left, right),
                        Bits::Not | Bits::TrailingZeros | Bits::CountOnes | Bits::Packed(_) => unreachable!(),
                    }
                };
                if width.bits == 64 {
                    result
                } else {
                    self.fb.ins().band_imm_s(result, width.maximum() as i64)
                }
            }
            Logical::SliceAt(slice, index, reach) => {
                return self.slice_at(expr.ty, slice, index, *reach);
            }
            Logical::SliceReadLe64(slice, index) => return Ok(vec![self.slice_read_le64(slice, index)?]),
            Logical::SlicePart(slice, lo, hi) => return self.slice_part(slice, lo, hi, false),
            Logical::SliceWithin(slice, lo, hi) => return self.slice_part(slice, lo, hi, true),
            Logical::SliceCopy(target, source) => {
                self.slice_copy(target, source)?;
                return Ok(vec![]);
            }
            Logical::EntryInput if super::super::BROKERED => self.entry_grant(8)?,
            Logical::Read(receiver, run) if super::super::BROKERED => {
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                let run = self.constant(u64::from(*run));
                self.runtime_value(super::storage::Runtime::ReadInput, &[grant, run])?
            }
            Logical::EntryInput | Logical::Read(..) => {
                return Err(error("Console<Read> requires the Wasm target"));
            }
            Logical::Host(op, Some(receiver), operands)
                if super::super::BROKERED && matches!(receiver.ty, Ty::Host(crate::compiler::host::Host::Dir(_))) =>
            {
                use crate::compiler::host::{Host, HostOp, Right};
                let mode = match op {
                    HostOp::PathByte => 0,
                    HostOp::OpenFile if expr.ty == Ty::Host(Host::File(Right::Write)) => 8,
                    HostOp::OpenFile => 1,
                    HostOp::Status => 2,
                    HostOp::Probe => 4,
                    HostOp::OpenDir if expr.ty == Ty::Host(Host::Dir(Right::All)) => 14,
                    HostOp::OpenDir if expr.ty == Ty::Host(Host::Dir(Right::Write)) => 15,
                    HostOp::OpenDir => 5,
                    HostOp::OpenList => 6,
                    HostOp::MakeDir => 7,
                    HostOp::OpenNew => 9,
                    HostOp::Remove => 10,
                    HostOp::OpenAppend => 11,
                    HostOp::RenameFrom => 12,
                    HostOp::Rename => 13,
                    _ => return Err(error("unsupported native directory operation")),
                };
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let mode = self.constant(mode);
                let value = if *op == HostOp::PathByte { self.one(&operands[0])? } else { self.constant(0) };
                let result = self.runtime_value(super::storage::Runtime::Directory, &[mode, handle, value])?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                result
            }
            Logical::Host(op, Some(receiver), operands)
                if super::super::BROKERED && matches!(receiver.ty, Ty::Host(crate::compiler::host::Host::Exec | crate::compiler::host::Host::Run)) =>
            {
                use crate::compiler::host::HostOp;
                let mode = match op {
                    HostOp::ExecNameByte => 0,
                    HostOp::ExecOnly => 1,
                    HostOp::ExecArgByte => 2,
                    HostOp::ExecArgEnd => 3,
                    HostOp::ExecRun => 4,
                    HostOp::ExecStatus => 5,
                    HostOp::ExecOutByte => 6,
                    HostOp::ExecCode => 7,
                    HostOp::ExecInputByte => 8,
                    HostOp::ExecDeadline => 9,
                    HostOp::ExecOutputLimit => 10,
                    HostOp::ExecErrByte => 11,
                    HostOp::ExecCwd => 12,
                    HostOp::ExecEnvNameByte => 13,
                    HostOp::ExecEnvValueByte => 14,
                    HostOp::ExecEnvEnd => 15,
                    _ => return Err(error("unsupported native exec operation")),
                };
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let mode = self.constant(mode);
                let value = if matches!(op, HostOp::ExecNameByte | HostOp::ExecArgByte | HostOp::ExecInputByte | HostOp::ExecDeadline | HostOp::ExecOutputLimit | HostOp::ExecCwd | HostOp::ExecEnvNameByte | HostOp::ExecEnvValueByte) { self.one(&operands[0])? } else { self.constant(0) };
                let result = self.runtime_value(super::storage::Runtime::Exec, &[mode, handle, value])?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                result
            }
            Logical::Host(op, receiver, operands)
                if super::super::BROKERED
                    && receiver.as_ref().map(|place| place.ty)
                        .or_else(|| operands.first().map(|value| value.ty))
                        == Some(Ty::Host(crate::compiler::host::Host::File(crate::compiler::host::Right::Write))) =>
            {
                use crate::compiler::host::HostOp;
                let mode = self.constant(match op {
                    HostOp::Status => 0,
                    HostOp::WriteByte => 1,
                    HostOp::WriteBulk => 2,
                    HostOp::Close => 3,
                    HostOp::Abandon => 4,
                    _ => return Err(error("unsupported native write-file operation")),
                });
                let handle = if let Some(place) = receiver {
                    let location = self.locate(place)?;
                    self.read_location(location)?[0]
                } else {
                    self.one(&operands[0])?
                };
                let zero = self.constant(0);
                let (value, count) = match op {
                    HostOp::WriteByte => (self.one(&operands[0])?, zero),
                    HostOp::WriteBulk => self.slice(&operands[0])?,
                    _ => (zero, zero),
                };
                let result = self.runtime_value(super::storage::Runtime::WriteFile, &[mode, handle, value, count])?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                result
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(op @ (crate::compiler::host::HostOp::SecretNameByte | crate::compiler::host::HostOp::SecretGet |
                crate::compiler::host::HostOp::SecretStatus), Some(receiver), operands)
                if receiver.ty == Ty::Host(crate::compiler::host::Host::SecretStore) => {
                use crate::compiler::host::HostOp;
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let input = if operands.is_empty() { self.constant(0) } else { self.one(&operands[0])? };
                let mode = match op {
                    HostOp::SecretNameByte => self.constant(0),
                    HostOp::SecretGet => self.fb.ins().iadd_imm_s(input, 1),
                    HostOp::SecretStatus => self.constant(3),
                    _ => unreachable!(),
                };
                let result = self.runtime_value(super::storage::Runtime::SecretStore, &[handle, input, mode])?;
                if expr.ty == Ty::Unit { return Ok(vec![]); }
                result
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(op @ (crate::compiler::host::HostOp::SecretStatus | crate::compiler::host::HostOp::SecretMessageByte |
                crate::compiler::host::HostOp::SecretHmac | crate::compiler::host::HostOp::SecretDigestByte), Some(receiver), operands)
                if matches!(receiver.ty, Ty::Host(crate::compiler::host::Host::Secret(crate::compiler::host::Right::Write | crate::compiler::host::Right::All))) => {
                use crate::compiler::host::HostOp;
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let mode = match op {
                    HostOp::SecretStatus => 0,
                    HostOp::SecretMessageByte => 1,
                    HostOp::SecretHmac => 2,
                    HostOp::SecretDigestByte => 3,
                    _ => unreachable!(),
                };
                let value = if operands.is_empty() { self.constant(0) } else { self.one(&operands[0])? };
                let mode = self.constant(mode);
                let result = self.runtime_value(super::storage::Runtime::SecretSeal, &[handle, value, mode])?;
                if expr.ty == Ty::Unit { return Ok(vec![]); }
                result
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(op @ (crate::compiler::host::HostOp::SecretByte | crate::compiler::host::HostOp::SecretStatus), Some(receiver), operands)
                if matches!(receiver.ty, Ty::Host(crate::compiler::host::Host::Secret(crate::compiler::host::Right::Read | crate::compiler::host::Right::All))) => {
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let reading = *op == crate::compiler::host::HostOp::SecretByte;
                let index = if reading { self.one(&operands[0])? } else { self.constant(0) };
                let mode = self.constant(u64::from(reading));
                self.runtime_value(super::storage::Runtime::SecretRead, &[handle, index, mode])?
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(HostOp::TlsSignerSign, Some(receiver), operands) => {
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let (digest, len) = self.slice(&operands[0])?;
                let (signature, capacity) = self.slice(&operands[1])?;
                self.runtime_value(super::storage::Runtime::TlsSignerSign,
                    &[handle, digest, len, signature, capacity])?
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(op @ (crate::compiler::host::HostOp::SignerPublicKey | crate::compiler::host::HostOp::SignerSign), Some(receiver), operands) => {
                use crate::compiler::host::HostOp;
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                let (output, length) = self.slice(operands.last().ok_or_else(|| error("signer output slice missing"))?)?;
                if *op == HostOp::SignerSign {
                    let (message, size) = self.slice(&operands[0])?;
                    self.runtime_value(super::storage::Runtime::SignerSign, &[handle, message, size, output, length])?
                } else {
                    self.runtime_value(super::storage::Runtime::SignerPublic, &[handle, output, length])?
                }
            }
            Logical::Host(crate::compiler::host::HostOp::MlDsa87Verify, None, _)
                if !cfg!(all(target_os = "macos", witchy_native_aws_lc)) => {
                return Err(error("native ML-DSA-87 verification requires a macOS arm64 compiler built with WITCHY_NATIVE_AWS_LC_ROOT"));
            }
            #[cfg(not(witchy_native_aws_lc))]
            Logical::Host(crate::compiler::host::HostOp::MlDsa65Verify, None, _) => {
                return Err(error("native ML-DSA-65 verification requires a compiler built with WITCHY_NATIVE_AWS_LC_ROOT"));
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::Host(op @ (crate::compiler::host::HostOp::MlDsa65Verify | crate::compiler::host::HostOp::MlDsa87Verify), None, operands) => {
                let suite = self.constant(if *op == crate::compiler::host::HostOp::MlDsa65Verify { 65 } else { 87 });
                let (key, key_len) = self.slice(&operands[0])?;
                let (message, message_len) = self.slice(&operands[1])?;
                let (signature, signature_len) = self.slice(&operands[2])?;
                self.runtime_value(super::storage::Runtime::PublicVerify,
                    &[suite, key, key_len, message, message_len, signature, signature_len])?
            }
            Logical::Host(HostOp::X509P384Verify, None, operands) => {
                #[cfg(witchy_native_aws_lc)]
                {
                    let mut arguments = Vec::with_capacity(10);
                    for operand in operands {
                        let (address, length) = self.slice(operand)?;
                        arguments.extend([address, length]);
                    }
                    self.runtime_value(super::storage::Runtime::PkixVerify, &arguments)?
                }
                #[cfg(not(witchy_native_aws_lc))]
                {
                    let _ = operands;
                    return Err(error("native certificate verification requires a compiler built with WITCHY_NATIVE_AWS_LC_ROOT"));
                }
            }
            Logical::Host(op @ (HostOp::Aes128GcmSeal | HostOp::Aes128GcmOpen |
                HostOp::Aes256GcmSeal | HostOp::Aes256GcmOpen | HostOp::VerifyTag48), None, operands) => {
                #[cfg(not(all(target_os = "macos", witchy_native_aws_lc)))]
                {
                    let _ = (op, operands);
                    return Err(error("native TLS record cryptography requires macOS ARM64 with WITCHY_NATIVE_AWS_LC_ROOT"));
                }
                #[cfg(all(target_os = "macos", witchy_native_aws_lc))]
                {
                    use super::storage::Runtime;
                    if *op == HostOp::VerifyTag48 {
                        let (left, left_len) = self.slice(&operands[0])?;
                        let (right, right_len) = self.slice(&operands[1])?;
                        self.runtime_value(Runtime::VerifyTag48, &[left, left_len, right, right_len])?
                    } else {
                        let mode = self.constant(match op {
                            HostOp::Aes128GcmSeal => 0,
                            HostOp::Aes128GcmOpen => 1,
                            HostOp::Aes256GcmSeal => 2,
                            _ => 3,
                        });
                        let mut arguments = vec![mode];
                        for operand in operands {
                            let (address, length) = self.slice(operand)?;
                            arguments.extend([address, length]);
                        }
                        self.runtime_value(Runtime::TlsAead, &arguments)?
                    }
                }
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::Sha256 | crate::compiler::host::HostOp::Sha384 | crate::compiler::host::HostOp::Sha512), None, operands)
                if super::super::BROKERED => {
                use crate::compiler::host::HostOp;
                let mode = self.constant(match op {
                    HostOp::Sha256 => 256,
                    HostOp::Sha384 => 384,
                    _ => 512,
                });
                let (data, length) = self.slice(&operands[0])?;
                let (digest, capacity) = self.slice(&operands[1])?;
                self.runtime_value(super::storage::Runtime::PublicDigest, &[mode, data, length, digest, capacity])?
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::ReadByte | crate::compiler::host::HostOp::ReadBulk | crate::compiler::host::HostOp::Status), Some(receiver), operands)
                if super::super::BROKERED && receiver.ty == Ty::Host(crate::compiler::host::Host::File(crate::compiler::host::Right::Read)) => {
                use crate::compiler::host::HostOp;
                use super::storage::Runtime;
                let location = self.locate(receiver)?;
                let handle = self.read_location(location)?[0];
                match op {
                    HostOp::Status => self.runtime_value(Runtime::FileStatus, &[handle])?,
                    HostOp::ReadByte => self.runtime_value(Runtime::FileByte, &[handle])?,
                    _ => {
                        let (address, count) = self.slice(&operands[0])?;
                        self.runtime_value(Runtime::FileRead, &[handle, address, count])?
                    }
                }
            }
            Logical::Host(crate::compiler::host::HostOp::Close | crate::compiler::host::HostOp::Abandon, None, operands)
                if super::super::BROKERED && operands.first().is_some_and(|operand| matches!(operand.ty, Ty::Host(crate::compiler::host::Host::Dir(_)))) =>
            {
                let handle = self.one(&operands[0])?;
                let op = self.constant(3);
                let unused = self.constant(0);
                let result = self.runtime_value(super::storage::Runtime::Directory, &[op, handle, unused])?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                result
            }
            Logical::Host(crate::compiler::host::HostOp::Close | crate::compiler::host::HostOp::Abandon, None, operands)
                if super::super::BROKERED && operands.first().is_some_and(|operand| operand.ty == Ty::Host(crate::compiler::host::Host::File(crate::compiler::host::Right::Read))) => {
                let handle = self.one(&operands[0])?;
                let status = self.runtime_value(super::storage::Runtime::FileClose, &[handle])?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                status
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::GrantNameByte | crate::compiler::host::HostOp::GrantLookup), Some(receiver), operands)
                if super::super::BROKERED =>
            {
                use super::storage::Runtime;
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                if *op == crate::compiler::host::HostOp::GrantNameByte {
                    let byte = self.one(&operands[0])?;
                    self.runtime(Runtime::GrantName, &[grant, byte])?;
                    return Ok(vec![]);
                }
                self.runtime_value(Runtime::GrantLookup, &[grant])?
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::EnvNameByte | crate::compiler::host::HostOp::EnvLookup | crate::compiler::host::HostOp::EnvValueByte), Some(receiver), operands) if super::super::BROKERED => {
                use crate::compiler::host::HostOp;
                use super::storage::Runtime;
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                if *op == HostOp::EnvLookup {
                    self.runtime_value(Runtime::EnvLookup, &[grant])?
                } else {
                    let operand = self.one(&operands[0])?;
                    if *op == HostOp::EnvNameByte {
                        self.runtime(Runtime::EnvNameByte, &[grant, operand])?;
                        return Ok(vec![]);
                    }
                    self.runtime_value(Runtime::EnvValueByte, &[grant, operand])?
                }
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::ArgsCount | crate::compiler::host::HostOp::ArgsSelect | crate::compiler::host::HostOp::ArgsByte), Some(receiver), operands) if super::super::BROKERED => {
                use crate::compiler::host::HostOp;
                use super::storage::Runtime;
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                match op {
                    HostOp::ArgsCount => self.runtime_value(Runtime::ArgsCount, &[grant])?,
                    _ => {
                        let index = self.one(&operands[0])?;
                        let function = if *op == HostOp::ArgsSelect { Runtime::ArgsSelect } else { Runtime::ArgsByte };
                        self.runtime_value(function, &[grant, index])?
                    }
                }
            }
            Logical::Host(op @ (HostOp::RandP384Start | HostOp::RandP384Public |
                HostOp::RandP384Finish | HostOp::RandP384Close | HostOp::RandP384Agree), receiver, operands) => {
                #[cfg(witchy_native_aws_lc)]
                {
                    let value = self.p384_operation(*op, receiver.as_ref(), operands)?;
                    if *op == HostOp::RandP384Close {
                        return Ok(vec![]);
                    }
                    value
                }
                #[cfg(not(witchy_native_aws_lc))]
                {
                    let _ = (op, receiver, operands);
                    return Err(error("native P-384 agreement requires a compiler built with WITCHY_NATIVE_AWS_LC_ROOT"));
                }
            }
            Logical::Host(crate::compiler::host::HostOp::RandNext, Some(receiver), _) if super::super::BROKERED => {
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                self.runtime_value(super::storage::Runtime::RandNext, &[grant])?
            }
            Logical::Host(crate::compiler::host::HostOp::ClockNow, Some(receiver), _) if super::super::BROKERED => {
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                self.runtime_value(super::storage::Runtime::ClockNow, &[grant])?
            }
            Logical::Host(op @ (crate::compiler::host::HostOp::ClockTimer | crate::compiler::host::HostOp::ClockTimerEnd), Some(receiver), operands)
                if super::super::BROKERED =>
            {
                use crate::compiler::host::HostOp;
                use super::storage::Runtime;
                let location = self.locate(receiver)?;
                let grant = self.read_location(location)?[0];
                let operand = self.one(&operands[0])?;
                if *op == HostOp::ClockTimer {
                    self.runtime_value(Runtime::ClockTimer, &[grant, operand])?
                } else {
                    self.runtime(Runtime::ClockTimerEnd, &[grant, operand])?;
                    return Ok(vec![]);
                }
            }
            Logical::Host(crate::compiler::host::HostOp::NetHandle, Some(receiver), _)
                if super::super::BROKERED =>
            {
                let location = self.locate(receiver)?;
                return self.read_location(location);
            }
            Logical::Host(op, receiver, operands) if super::super::BROKERED => {
                use crate::compiler::host::HostOp;
                use super::storage::Runtime;
                let opcode = match op {
                    HostOp::NetAddrByte => 0,
                    HostOp::NetOnly => 1,
                    HostOp::NetConnect => 2,
                    HostOp::NetStatus => 3,
                    HostOp::NetSendByte => 4,
                    HostOp::NetRecvByte => 5,
                    HostOp::NetClose => 6,
                    HostOp::NetSend => 7,
                    HostOp::NetRecv => 8,
                    HostOp::NetAccept => 9,
                    HostOp::NetAcceptedOn => 10,
                    HostOp::NetTryAccept => 11,
                    HostOp::NetTrySend => 12,
                    HostOp::NetTryRecv => 13,
                    HostOp::NetPoll => 14,
                    HostOp::NetGroup => 15,
                    HostOp::GroupAdd => 16,
                    HostOp::GroupTake => 17,
                    HostOp::NetWait => 18,
                    _ => return Err(error(format_args!("host operation {} has no native lowering", op.import()))),
                };
                let mut args = vec![self.constant(opcode)];
                if let Some(receiver) = receiver {
                    let location = self.locate(receiver)?;
                    args.extend(self.read_location(location)?);
                }
                for operand in operands {
                    args.extend(self.expr(operand)?);
                }
                if !(2..=4).contains(&args.len()) {
                    return Err(error("invalid native Net operation operands"));
                }
                args.resize(4, self.constant(0));
                let value = self.runtime(Runtime::Net, &args)?
                    .ok_or_else(|| error("missing native Net operation result"))?;
                if expr.ty == Ty::Unit {
                    return Ok(vec![]);
                }
                value
            }
            #[cfg(witchy_native_aws_lc)]
            Logical::EntryHost(crate::compiler::host::Host::Signer | crate::compiler::host::Host::TlsSigner |
                crate::compiler::host::Host::Secret(crate::compiler::host::Right::Read | crate::compiler::host::Right::Write), slot) => self.constant(*slot),
            Logical::EntryHost(host, _) => {
                let bit = super::super::host_grant(*host)
                    .ok_or_else(|| error(format_args!("{} has no native launch grant", host.name())))?;
                if self.entry_grants.is_none()
                    && matches!(host, crate::compiler::host::Host::Clock | crate::compiler::host::Host::Rand)
                {
                    self.constant(bit)
                } else {
                    self.entry_grant(bit as i64)?
                }
            }
            Logical::Host(op, ..) => {
                return Err(error(format_args!("host operation {} has no native lowering", op.import())));
            }
            Logical::BoxOp(op, id, args) => return self.box_operation(expr.ty, *op, *id, args),
            Logical::TaskSpawn { allocator, bytes, entry } =>
                return self.spawn_task(expr.ty, allocator, bytes, entry),
            Logical::TaskStep(task, word) =>
                return self.step_task(expr.ty, task, word),
            Logical::TaskStatus(task) => {
                let handle = self.handle(task)?;
                self.runtime_value(super::storage::Runtime::TaskStatus, &[handle])?
            }
            Logical::TaskFinish(task) => {
                let handle = self.one(task)?;
                self.runtime(super::storage::Runtime::TaskFinish, &[handle])?;
                return Ok(vec![]);
            }
            Logical::Pause(word) => {
                let word = self.one(word)?;
                self.runtime_value(super::storage::Runtime::TaskPause, &[word])?
            }
            Logical::TaskWord =>
                self.runtime_value(super::storage::Runtime::TaskWord, &[])?,
            Logical::InTask =>
                self.runtime_value(super::storage::Runtime::TaskActive, &[])?,
            Logical::Address(..) | Logical::Stored(..) => {
                return Err(error(
                    "address of a multi-lane reference requires the Wasm target",
                ));
            }
        };
        Ok(vec![value])
    }

    /// Branching consumers can route short-circuit operands directly to their
    /// continuation blocks. Value consumers still materialize a Bool.
    pub(super) fn branch(&mut self, expr: &Expr, yes: Block, no: Block) -> Result<()> {
        match &expr.kind {
            Logical::Binary(op @ (Binary::And | Binary::Or), left, right) => {
                let next = self.fb.create_block();
                if matches!(op, Binary::And) {
                    self.branch(left, next, no)?;
                } else {
                    self.branch(left, yes, next)?;
                }
                self.switch_to(next);
                self.branch(right, yes, no)?;
            }
            Logical::Not(inner) => self.branch(inner, no, yes)?,
            _ => {
                let condition = self.one(expr)?;
                self.fb.ins().brif(condition, yes, &[], no, &[]);
            }
        }
        Ok(())
    }

    fn short_circuit(&mut self, op: Binary, left: &Expr, right: &Expr) -> Result<Value> {
        let condition = self.one(left)?;
        let right_block = self.fb.create_block();
        let join = self.fb.create_block();
        let result = self.fb.append_block_param(join, I64);
        if matches!(op, Binary::And) {
            self.fb
                .ins()
                .brif(condition, right_block, &[], join, &[condition.into()]);
        } else {
            self.fb
                .ins()
                .brif(condition, join, &[condition.into()], right_block, &[]);
        }
        self.switch_to(right_block);
        let value = self.one(right)?;
        self.fb.ins().jump(join, &[value.into()]);
        self.switch_to(join);
        Ok(result)
    }
}
