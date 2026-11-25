use super::{Body, I64, InstBuilder, Result, TRAP, Value, error};
use crate::compiler::logical_body::{Expr, StorageOp, Ty, Value as Logical};
use cranelift_codegen::ir::AbiParam;
use cranelift_module::{Linkage, Module};


/// Fixed signatures at the native compiler/runtime boundary. All carriers
/// use the Darwin integer calling convention, including native addresses.
#[derive(Clone, Copy)]
pub(super) enum Runtime {
    Allocate,
    Capacity,
    Release,
    Cell,
    ReadRange,
    Observe,
    TakePrefix,
    Transfer,
    Fill,
    ReadWord,
    InitWord,
    InitSlice,
    CopySlice,
    Net,
    BoxLoan,
    ReadInput,
    ClockNow,
    ClockTimer,
    ClockTimerEnd,
    RandNext,
    #[cfg(witchy_native_aws_lc)]
    P384Start,
    #[cfg(witchy_native_aws_lc)]
    P384Public,
    #[cfg(witchy_native_aws_lc)]
    P384Finish,
    #[cfg(witchy_native_aws_lc)]
    P384Close,
    #[cfg(witchy_native_aws_lc)]
    P384Agree,
    ArgsCount,
    ArgsSelect,
    ArgsByte,
    EnvNameByte,
    GrantName,
    GrantLookup,
    EnvLookup,
    EnvValueByte,
    FileStatus,
    PublicDigest,
    #[cfg(witchy_native_aws_lc)]
    PublicVerify,
    #[cfg(witchy_native_aws_lc)]
    PkixVerify,
    #[cfg(all(target_os = "macos", witchy_native_aws_lc))]
    TlsAead,
    #[cfg(all(target_os = "macos", witchy_native_aws_lc))]
    VerifyTag48,
    #[cfg(witchy_native_aws_lc)]
    SecretRead,
    #[cfg(witchy_native_aws_lc)]
    SecretSeal,
    #[cfg(witchy_native_aws_lc)]
    SecretStore,
    #[cfg(witchy_native_aws_lc)]
    SignerSign,
    #[cfg(witchy_native_aws_lc)]
    TlsSignerSign,
    #[cfg(witchy_native_aws_lc)]
    SignerPublic,
    FileByte,
    FileRead,
    WriteFile,
    Directory,
    Exec,
    FileClose,
    TaskSpawn,
    TaskStep,
    TaskStatus,
    TaskWord,
    TaskActive,
    TaskPause,
    TaskFinish,
}

impl Runtime {
    fn signature(self) -> (&'static str, usize, bool, bool) {
        let base = |name, count, returns| (name, count, returns, false);
        let platform_crypto = |name, count, returns| (name, count, returns, cfg!(witchy_native_aws_lc));
        #[cfg(witchy_native_aws_lc)]
        let aws_lc = |name, count, returns| (name, count, returns, true);
        match self {
            Self::TaskSpawn => base("witchy_task_spawn", 5, true),
            Self::TaskStep => base("witchy_task_step", 2, true),
            Self::TaskStatus => base("witchy_task_status", 1, true),
            Self::TaskWord => base("witchy_task_word", 0, true),
            Self::TaskActive => base("witchy_task_active", 0, true),
            Self::TaskPause => base("witchy_task_pause", 1, true),
            Self::TaskFinish => base("witchy_task_finish", 1, false),
            Self::Allocate => base("witchy_allocate", 4, true),
            Self::Capacity => base("witchy_capacity", 2, true),
            Self::Release => base("witchy_release", 2, true),
            Self::Cell => base("witchy_cell", 4, true),
            Self::ReadRange => base("witchy_read_range", 4, true),
            Self::Observe => base("witchy_observe", 5, true),
            Self::TakePrefix => base("witchy_take_prefix", 3, false),
            Self::Transfer => base("witchy_transfer", 6, false),
            Self::Fill => base("witchy_fill", 4, false),
            Self::ReadWord => base("witchy_read_word", 4, true),
            Self::InitSlice => base("witchy_init_slice", 5, false),
            Self::InitWord => base("witchy_init_word", 5, false),
            #[cfg(witchy_native_aws_lc)]
            Self::PublicVerify => aws_lc("witchy_public_mldsa_verify", 7, true),
            #[cfg(witchy_native_aws_lc)]
            Self::PkixVerify => aws_lc("witchy_pkix_verify", 10, true),
            #[cfg(all(target_os = "macos", witchy_native_aws_lc))]
            Self::TlsAead => aws_lc("witchy_tls13_aead", 11, true),
            #[cfg(all(target_os = "macos", witchy_native_aws_lc))]
            Self::VerifyTag48 => aws_lc("witchy_verify_tag48", 4, true),
            #[cfg(witchy_native_aws_lc)]
            Self::SecretRead => aws_lc("witchy_secret_read", 3, true),
            #[cfg(witchy_native_aws_lc)]
            Self::SecretSeal => aws_lc("witchy_secret_seal", 3, true),
            #[cfg(witchy_native_aws_lc)]
            Self::SecretStore => aws_lc("witchy_secret_store", 3, true),
            #[cfg(witchy_native_aws_lc)]
            Self::SignerSign => aws_lc("witchy_signer_sign", 5, true),
            #[cfg(witchy_native_aws_lc)]
            Self::TlsSignerSign => aws_lc("witchy_tls_signer_sign", 5, true),
            #[cfg(witchy_native_aws_lc)]
            Self::SignerPublic => aws_lc("witchy_signer_public", 3, true),
            Self::PublicDigest => platform_crypto("witchy_public_digest", 5, true),
            Self::FileStatus => base("witchy_file_status", 1, true),
            Self::FileByte => base("witchy_file_byte", 1, true),
            Self::Directory => base("witchy_directory", 3, true),
            Self::Exec => base("witchy_exec", 3, true),
            Self::WriteFile => base("witchy_write_file", 4, true),
            Self::FileRead => base("witchy_file_read", 3, true),
            Self::FileClose => base("witchy_file_close", 1, true),
            Self::GrantName => base("witchy_grant_name", 2, false),
            Self::GrantLookup => base("witchy_grant_lookup", 1, true),
            Self::EnvNameByte => base("witchy_env_name_byte", 2, false),
            Self::EnvLookup => base("witchy_env_lookup", 1, true),
            Self::EnvValueByte => base("witchy_env_value_byte", 2, true),
            Self::ArgsCount => base("witchy_args_count", 1, true),
            Self::ArgsSelect => base("witchy_args_select", 2, true),
            Self::ArgsByte => base("witchy_args_byte", 2, true),
            Self::RandNext => platform_crypto("witchy_rand_next", 1, true),
            #[cfg(witchy_native_aws_lc)]
            Self::P384Start => aws_lc("witchy_p384_start", 1, true),
            #[cfg(witchy_native_aws_lc)]
            Self::P384Public => aws_lc("witchy_p384_public", 3, true),
            #[cfg(witchy_native_aws_lc)]
            Self::P384Finish => aws_lc("witchy_p384_finish", 5, true),
            #[cfg(witchy_native_aws_lc)]
            Self::P384Close => aws_lc("witchy_p384_close", 1, true),
            #[cfg(witchy_native_aws_lc)]
            Self::P384Agree => aws_lc("witchy_p384_agree", 7, true),
            Self::ClockNow => base("witchy_clock_now_ms", 1, true),
            Self::ClockTimer => base("witchy_clock_timer", 2, true),
            Self::ClockTimerEnd => base("witchy_clock_timer_end", 2, false),
            Self::ReadInput => base("witchy_read", 2, true),
            Self::BoxLoan => base("witchy_box_loan", 2, true),
            Self::Net => base("witchy_native_net", 4, true),
            Self::CopySlice => base("witchy_copy_slice", 3, false),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum CellMode {
    Initialize = 0,
    Take = 1,
    Read = 2,
}

impl Body<'_, '_> {
    pub(super) fn runtime(
        &mut self,
        function: Runtime,
        arguments: &[Value],
    ) -> Result<Option<Value>> {
        let (name, count, returns, requires_aws_lc) = function.signature();
        self.requires_aws_lc |= requires_aws_lc;
        let mut signature = self.module.make_signature();
        signature.params.resize(count, AbiParam::new(I64));
        if returns {
            signature.returns.push(AbiParam::new(I64));
        }
        let id = self
            .module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(error)?;
        let reference = self.module.declare_func_in_func(id, self.fb.func);
        self.reads.clear();
        let call = self.fb.ins().call(reference, arguments);
        Ok(self.fb.inst_results(call).first().copied())
    }

    pub(super) fn runtime_value(&mut self, function: Runtime, arguments: &[Value]) -> Result<Value> {
        self.runtime(function, arguments)?
            .ok_or_else(|| error("missing native runtime result"))
    }

    pub(super) fn constant(&mut self, value: u64) -> Value {
        self.fb.ins().iconst(I64, value as i64)
    }

    pub(super) fn handle(&mut self, receiver: &Expr) -> Result<Value> {
        let address = self.one(receiver)?;
        Ok(self.load_word(address))
    }

    /// The bytes from one cell of a buffer of `element` to the next: what an allocation asks
    /// the runtime for, and what compiled code that addresses a cell itself steps by (cells.rs).
    /// An element of no bytes still takes one.
    pub(super) fn buffer_stride(&self, element: Ty) -> Result<u64> {
        Ok(u64::from(self.layout.storage(element)?.size.max(1)))
    }

    pub(super) fn allocation(
        &mut self,
        ty: Ty,
        allocator: &Expr,
        capacity: &Expr,
        type_id: u32,
        prefix_only: bool,
    ) -> Result<Vec<Value>> {
        let grant = self.handle(allocator)?;
        let count = self.one(capacity)?;
        let element = self.buffer_element(type_id - 1)?;
        let stride = self.buffer_stride(element)?;
        let stride = self.constant(stride);
        if prefix_only != self.dense(type_id - 1)? {
            return Err(error("a prefix allocation is a PrefixBuffer's"));
        }
        let kind = if prefix_only { crate::compiler::allocator::PREFIX_TYPE } else { 0 };
        let type_id = self.constant(u64::from(type_id) | kind);
        let outcome = self.runtime_value(Runtime::Allocate, &[grant, type_id, stride, count])?;
        let failed = self.allocation_failed(outcome);
        self.result(ty, failed, |_| Ok(vec![outcome]), |_| Ok(vec![outcome]))
    }

    // Allocations start at generation one. A zero high word denotes an error.
    pub(super) fn allocation_failed(&mut self, outcome: Value) -> Value {
        let generation = self.fb.ins().ushr_imm_s(outcome, 32);
        self.fb.ins().icmp_imm_s(super::IntCC::Equal, generation, 0)
    }

    pub(super) fn capacity(&mut self, receiver: &Expr, type_id: u32) -> Result<Value> {
        let handle = self.handle(receiver)?;
        let type_id = self.constant(u64::from(type_id));
        self.runtime_value(Runtime::Capacity, &[handle, type_id])
    }

    pub(super) fn release(&mut self, ty: Ty, owner: &Expr, type_id: u32) -> Result<Vec<Value>> {
        let handle = self.one(owner)?;
        let type_id = self.constant(self.buffer_kind(type_id - 1)?);
        let status = self.runtime_value(Runtime::Release, &[handle, type_id])?;
        self.result(ty, status, |_| Ok(vec![]), |_| Ok(vec![handle]))
    }

    /// Whether `buffer` is a `PrefixBuffer`.
    pub(super) fn dense(&self, buffer: u32) -> Result<bool> {
        self.layout.types.buffers().get(buffer as usize).map(|b| b.dense).ok_or_else(|| error("missing native buffer type"))
    }

    /// Refuse an operation that names a buffer's slot when the value's type is a prefix buffer.
    /// The checker refuses each of these in source; the backend does not lower one either, so the
    /// type's promise (no key, link or weak reference to a prefix buffer) does not rest on one layer.
    pub(super) fn not_dense(&self, ty: Ty) -> Result<()> {
        let buffer = match ty {
            Ty::Buffer(id) => Some(id),
            Ty::Borrow { pointee, .. } => match pointee.ty() { Ty::Buffer(id) => Some(id), _ => None },
            _ => None,
        };
        if let Some(id) = buffer { if self.dense(id)? { return Err(error("a prefix buffer has no key, link or single-cell operation in native code")); } }
        Ok(())
    }

    /// The type a runtime call is told the buffer has.
    pub(super) fn buffer_kind(&self, buffer: u32) -> Result<u64> {
        self.dense(buffer)?;
        Ok(u64::from(buffer) + 1)
    }

    pub(super) fn buffer_element(&self, buffer: u32) -> Result<Ty> {
        self.layout
            .types
            .buffers()
            .get(buffer as usize)
            .map(|b| b.element)
            .ok_or_else(|| error("missing native buffer element"))
    }

    pub(super) fn cell(
        &mut self,
        handle: Value,
        ty: Value,
        index: Value,
        mode: CellMode,
    ) -> Result<(Value, Value)> {
        if super::cells::inline() {
            let change = match mode {
                CellMode::Initialize => super::cells::Change::Initialize,
                CellMode::Take => super::cells::Change::Take,
                CellMode::Read => super::cells::Change::Peek,
            };
            return self.checked_cell(handle, ty, index, change);
        }
        let mode = self.constant(mode as u64);
        let outcome = self.runtime_value(Runtime::Cell, &[handle, ty, index, mode])?;
        let is_error = self.fb.ins().icmp_imm_s(super::IntCC::UnsignedLessThanOrEqual, outcome, 2);
        let zero = self.constant(0);
        let status = self.fb.ins().select(is_error, outcome, zero);
        Ok((status, outcome))
    }

    pub(super) fn storage(
        &mut self,
        result: Ty,
        op: StorageOp,
        receiver: &Expr,
        index: &Expr,
        value: Option<&Expr>,
        buffer: u32,
    ) -> Result<Vec<Value>> {
        use StorageOp::*;

        let token = self.handle(receiver)?;
        let index = self.one(index)?;
        let type_id = self.constant(self.buffer_kind(buffer)?);
        let element = self.buffer_element(buffer)?;
        let operand = || value.ok_or_else(|| error("missing native buffer operand"));

        match op {
            RefLink | MutLink => {
                self.not_dense(Ty::Buffer(buffer))?;
                let mismatch = self.fb.ins().icmp(super::IntCC::NotEqual, token, index);
                self.fb.ins().trapnz(mismatch, TRAP);
                let zero = self.constant(0);
                let one = self.constant(1);
                return Ok(vec![self.runtime_value(
                    Runtime::ReadRange,
                    &[token, type_id, zero, one],
                )?]);
            }
            GetUnchecked | RefUnchecked | MutUnchecked => {
                // Keep the logical frame charge for the checked cell address.
                self.charge_frame(1)?;
                let one = self.constant(1);
                let address = if super::cells::inline() {
                    self.proved_cells(token, buffer, element, index, one, super::cells::Change::Read)?
                } else {
                    self.runtime_value(Runtime::ReadRange, &[token, type_id, index, one])?
                };
                return Ok(if op == GetUnchecked {
                    self.load_typed(address, element)?
                } else {
                    vec![address]
                });
            }
            MovePrefixUnchecked | CopyIntoUnchecked => {
                let (destination, at) = if op == CopyIntoUnchecked {
                    let Logical::Aggregate(parts) = &operand()?.kind else {
                        return Err(error("missing buffer copy destination"));
                    };
                    let [target, at] = parts.as_slice() else {
                        return Err(error("missing buffer copy offset"));
                    };
                    (self.handle(target)?, self.one(at)?)
                } else {
                    (self.handle(operand()?)?, self.constant(0))
                };
                let take = self.constant(u64::from(op == MovePrefixUnchecked));
                self.runtime(
                    Runtime::Transfer,
                    &[token, destination, type_id, at, index, take],
                )?;
                return Ok(vec![]);
            }
            TakePrefixUnchecked => {
                if super::cells::inline() {
                    self.proved_dense_range(token, type_id, buffer, index, index, None)?;
                } else {
                    self.runtime(Runtime::TakePrefix, &[token, type_id, index])?;
                }
                return Ok(vec![]);
            }
            ReadWordUnchecked => {
                let count = self.one(operand()?)?;
                return Ok(vec![self.runtime_value(
                    Runtime::ReadWord,
                    &[token, type_id, index, count],
                )?]);
            }
            InitSliceUnchecked => {
                let lanes = self.abi_lanes(operand()?)?;
                let [address, count] = lanes.as_slice() else {
                    return Err(error("expected address and length of a shared slice"));
                };
                if super::cells::inline() {
                    self.proved_dense_range(token, type_id, buffer, index, *count, Some(*address))?;
                } else {
                    self.runtime(Runtime::InitSlice, &[token, type_id, index, *address, *count])?;
                }
                return Ok(vec![]);
            }
            InitWordUnchecked => {
                let values = self.abi_lanes(operand()?)?;
                let [word, count] = values.as_slice() else {
                    return Err(error("expected buffer word and byte count"));
                };
                self.runtime(Runtime::InitWord, &[token, type_id, index, *count, *word])?;
                return Ok(vec![]);
            }
            FillUnchecked => {
                let values = self.abi_lanes(operand()?)?;
                let cell = self.scratch(element)?;
                self.store_typed(cell, element, &values)?;
                self.runtime(Runtime::Fill, &[token, type_id, index, cell])?;
                return Ok(vec![]);
            }
            SliceUnchecked => {
                let end = self.one(operand()?)?;
                let reversed = self
                    .fb
                    .ins()
                    .icmp(super::IntCC::UnsignedLessThan, end, index);
                self.fb.ins().trapnz(reversed, TRAP);
                let count = self.fb.ins().isub(end, index);
                let address = if super::cells::inline() {
                    self.proved_cells(token, buffer, element, index, count, super::cells::Change::Read)?
                } else {
                    self.runtime_value(Runtime::ReadRange, &[token, type_id, index, count])?
                };
                return Ok(vec![address, count]);
            }
            _ => {}
        }

        let incoming = if matches!(op, TryInit | InitUnchecked | ReplaceUnchecked) {
            Some(self.abi_lanes(operand()?)?)
        } else {
            None
        };
        // A proved single cell: written in, with nothing for the proof to have shown left to test.
        if matches!(op, InitUnchecked | TakeUnchecked | ReplaceUnchecked) && super::cells::inline() {
            let change = match op { InitUnchecked => super::cells::Change::Initialize, TakeUnchecked => super::cells::Change::Take, _ => super::cells::Change::Peek };
            let one = self.constant(1);
            let addr = self.proved_cells(token, buffer, element, index, one, change)?;
            return Ok(match op {
                InitUnchecked => {
                    self.store_typed(addr, element, incoming.as_deref().expect("initialized payload"))?;
                    vec![]
                }
                TakeUnchecked => self.load_typed(addr, element)?,
                _ => {
                    let previous = self.load_typed(addr, element)?;
                    self.store_typed(addr, element, incoming.as_deref().expect("replacement payload"))?;
                    previous
                }
            });
        }
        let mode = match op {
            Init | TryInit | InitUnchecked => CellMode::Initialize,
            Take | TakeUnchecked => CellMode::Take,
            _ => CellMode::Read,
        };
        if !op.proof_admitted() || super::cells::inline() { self.not_dense(Ty::Buffer(buffer))?; }
        let (status, address) = self.cell(token, type_id, index, mode)?;
        if op == Initialized {
            let value = self.fb.ins().icmp_imm_s(super::IntCC::Equal, status, 0);
            return Ok(vec![self.fb.ins().uextend(I64, value)]);
        }

        if op.proof_admitted() {
            // Keep a native bounds and occupancy check regardless of source proof admission.
            self.fb.ins().trapnz(status, TRAP);
            let addr = address;
            return Ok(match op {
                InitUnchecked => {
                    self.store_typed(
                        addr,
                        element,
                        incoming.as_deref().expect("initialized payload"),
                    )?;
                    vec![]
                }
                TakeUnchecked => self.load_typed(addr, element)?,
                ReplaceUnchecked => {
                    let previous = self.load_typed(addr, element)?;
                    self.store_typed(addr, element, incoming.as_deref().expect("replacement payload"))?;
                    previous
                }
                _ => return Err(error("unsupported proof-admitted buffer operation")),
            });
        }

        self.result(
            result,
            status,
            |b| {
                // init/swap evaluate a copyable payload only after the guard succeeds.
                let late = if matches!(op, Init | Swap) {
                    Some(b.abi_lanes(operand()?)?)
                } else {
                    None
                };
                let result = match op {
                    Read | Take | Swap => b.load_typed(address, element)?,
                    Ref => vec![address],
                    _ => vec![],
                };
                if let Some(values) = late.as_deref().or(incoming.as_deref()) {
                    b.store_typed(address, element, values)?;
                }
                Ok(result)
            },
            |_| Ok(incoming.clone().unwrap_or_else(|| vec![status])),
        )
    }
}
