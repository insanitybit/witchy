//! Type and ownership checks for the `memory` namespace primitives.
use super::*;

impl Checker<'_> {
    pub(super) fn memory_call(
        &mut self,
        method: &str,
        args: &[a::Expr],
        expected: Option<Ty>,
    ) -> Result<(Ty, Value)> {
        if method == "utf8_unchecked" {
            if !self.memory_proof {
                return Err(self.proof_required("refining bytes to UTF-8 requires a memory proof"));
            }
            let [source] = args else { return Err(error("utf8_unchecked takes one shared byte slice")); };
            let Ty::Slice(bytes) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
            let source = self.argument(source, Ty::Borrow { mutable: false, pointee: Pointee::Slice(bytes) })?;
            return Ok((self.layouts.str_borrow(), Value::ProofUtf8(Box::new(source))));
        }
        self.layouts.enable_heap();
        // A PrefixBuffer is storage whose owner alone names it and whose full cells are its
        // first ones: it issues no key, link or weak reference, and has no cell-by-cell state
        // to ask about. The type says so, whatever helper the buffer has been passed through.
        let identity = matches!(method, "allocation_key" | "buffer_pointer" | "ref_link" | "mut_link");
        if identity || matches!(method, "initialized" | "init" | "read" | "take" | "swap" | "ref" | "mut" | "try_init") {
            if let Some(Ok(id)) = args.first().map(|arg| self.buffer_argument(arg)) {
                if self.layouts.buffers()[id as usize].dense {
                    return Err(error(if identity { format!("memory.{method} names an allocation, and a PrefixBuffer issues no key, link or weak reference: use a Buffer") }
                        else { format!("memory.{method} asks about one cell, and a PrefixBuffer's full cells are its first ones: use a Buffer") }));
                }
            }
        }
        Ok(match method {
            "null_weak" => {
                let Some(ty @ Ty::Weak(_)) = expected else { return Err(error("null_weak requires Weak<T> context")); };
                if !args.is_empty() { return Err(error("null_weak takes no arguments")); }
                (ty, Value::U64(0))
            }
            "weak_bits" => {
                let [arg] = args else { return Err(error("weak_bits takes a typed weak pointer")); };
                let weak = self.expr(arg)?;
                let Ty::Weak(id) = weak.ty else { return Err(error("weak_bits takes a typed weak pointer")); };
                (Ty::Unsigned(Scalar::U32), Value::BoxOp(crate::compiler::operations::BoxOp::WeakBits, id, vec![weak]))
            }
            "null_box" => {
                let Some(ty @ Ty::Boxed(_)) = expected else { return Err(error("null_box requires Boxed<T> context")); };
                if !args.is_empty() { return Err(error("null_box takes no arguments")); }
                (ty, Value::U64(0))
            }
            "try_box" => {
                let Some(ty) = expected else { return Err(error("try_box requires Result<Boxed<T>, T> context")); };
                let Some([Ty::Boxed(id), item]) = self.layouts.result_sum(ty).map(|s| s.arms) else {
                    return Err(error("try_box requires Result<Boxed<T>, T> context"));
                };
                if self.layouts.buffers()[id as usize].element != item { return Err(error("try_box returns the original item on failure")); }
                let [grant, value] = args else { return Err(error("try_box takes an allocator loan and a value")); };
                self.element_limits.push(item);
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let grant = self.argument(grant, Ty::Borrow { mutable: false, pointee: Pointee::Allocator })?;
                let value = self.expect(value, item)?;
                self.keep(saved, mark, ty);
                (ty, Value::BoxOp(crate::compiler::operations::BoxOp::New, id, vec![grant, value]))
            }
            "unbox" | "end_null_box" => {
                let [arg] = args else { return Err(error("unbox consumes one Boxed<T>")); };
                let owner = self.expr(arg)?;
                let Ty::Boxed(id) = owner.ty else { return Err(error("unbox consumes one Boxed<T>")); };
                if method == "end_null_box" {
                    if !self.scalar_proof { return Err(self.proof_required("ending an empty box requires a scalar contract and proof")); }
                    (Ty::Unit, Value::BoxOp(crate::compiler::operations::BoxOp::EndNull, id, vec![owner]))
                } else {
                    let item = self.layouts.buffers()[id as usize].element;
                    self.element_limits.push(item);
                    let ty = self.layouts.result([item, Ty::Unit])?;
                    (ty, Value::BoxOp(crate::compiler::operations::BoxOp::Take, id, vec![owner]))
                }
            }
            "box_address" | "box_ref" | "box_mut" | "lend_box" | "lend_box_mut" | "weaken_box" | "follow_weak" | "follow_weak_mut" => {
                use crate::compiler::operations::BoxOp;
                let follow = matches!(method, "follow_weak" | "follow_weak_mut");
                if args.len() != if follow { 2 } else { 1 } { return Err(error("a boxed operation takes its owner loan and, for follow, a weak pointer")); }
                let arg = &args[0];
                let ty = match &arg.kind {
                    a::Expression::Unary("&" | "&mut", target) => self.hint(target),
                    _ => self.hint(arg).and_then(|ty| match ty { Ty::Borrow { pointee, .. } => Some(pointee.ty()), _ => None }),
                };
                let Some(Ty::Boxed(id)) = ty else { return Err(error("a boxed loan requires a Boxed<T> reference")); };
                let mutable = matches!(method, "box_mut" | "lend_box_mut" | "follow_weak_mut");
                let op = match method {
                    "weaken_box" => BoxOp::Weaken, "follow_weak" => BoxOp::Follow, "follow_weak_mut" => BoxOp::FollowMut,
                    "lend_box" => BoxOp::Lend, "lend_box_mut" => BoxOp::LendMut,
                    "box_ref" => BoxOp::Ref, "box_mut" => BoxOp::Mut, _ => BoxOp::Address,
                };
                if op.proved() && !self.scalar_proof { return Err(self.proof_required("a direct box loan requires a scalar contract and proof")); }
                let result = if op == BoxOp::Weaken { Ty::Weak(id) } else if op == BoxOp::Address { Ty::Unsigned(Scalar::U32) } else {
                    let pointee = Pointee::from_ty(self.layouts.buffers()[id as usize].element).ok_or_else(|| error("box element must be storable"))?;
                    let loan = Ty::Borrow { mutable, pointee };
                    if op.proved() { loan } else { self.layouts.result([loan, Ty::Unit])? }
                };
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let owner = self.argument(arg, Ty::Borrow { mutable, pointee: Pointee::Boxed(id) })?;
                let mut operands = vec![owner];
                if follow { operands.push(self.expect(&args[1], Ty::Weak(id))?); }
                self.keep(saved, mark, result);
                (result, Value::BoxOp(op, id, operands))
            }
            "try_alloc" | "try_alloc_prefix" => {
                let ty = expected.ok_or_else(|| error("try_alloc requires Result<Buffer<T>, U64> context"))?;
                let arms = self.layouts.result_sum(ty).ok_or_else(|| error("try_alloc requires Result<Buffer<T>, U64> context"))?.arms;
                let [Ty::Buffer(buffer), error_ty] = arms else { return Err(error("try_alloc requires Result<Buffer<T>, U64> context")); };
                if error_ty != Ty::U64 { return Err(error("try_alloc error type must be U64")); }
                let [allocator, capacity] = args else { return Err(error("try_alloc takes allocator borrow and capacity")); };
                let saved = self.loans.clone();
                let allocator = self.argument(allocator, Ty::Borrow { mutable: false, pointee: Pointee::Allocator });
                let result = allocator.and_then(|allocator| Ok((allocator, self.expect(capacity, Ty::U64)?)));
                self.loans = saved;
                let (allocator, capacity) = result?;
                // The type says which storage is asked for, in any module: what kept prefix
                // storage to the standard Vec was the cell-by-cell operations, and a
                // PrefixBuffer is refused those above.
                let prefix_only = self.layouts.buffers()[buffer as usize].dense;
                if prefix_only != (method == "try_alloc_prefix") {
                    return Err(error(if prefix_only { "memory.try_alloc makes a Buffer; a PrefixBuffer is made by memory.try_alloc_prefix" }
                        else { "memory.try_alloc_prefix makes a PrefixBuffer; a Buffer is made by memory.try_alloc" }));
                }
                (ty, Value::TryAlloc { allocator: Box::new(allocator), capacity: Box::new(capacity), type_id: buffer + 1, prefix_only })
            }
            "lend_enclosing" => {
                let [walk, member, edge, focus] = args else { return Err(error("lend_enclosing takes a structural loan, a member, its link and a shared focus")); };
                if !self.scalar_proof { return Err(self.proof_required("lending from a recursive footprint requires a scalar proof")); }
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let walk = self.expr(walk)?;
                let Ty::Borrow { mutable: false, pointee } = walk.ty else { return Err(error("the structural witness must be a shared loan")); };
                let member = self.expect(member, walk.ty)?;
                let edge = self.expr(edge)?;
                let Ty::Weak(id) = edge.ty else { return Err(error("the member link must be a typed weak pointer")); };
                if self.layouts.buffers()[id as usize].element != pointee.ty() { return Err(error("member and link must designate the same element type")); }
                let focus = self.expect(focus, walk.ty)?;
                let result = walk.ty;
                self.keep(saved, mark, result);
                (result, Value::BoxOp(crate::compiler::operations::BoxOp::Enclosing, id, vec![walk, member, edge, focus]))
            }
            "follow_loan" | "follow_loan_mut" => {
                let [witness, link] = args else { return Err(error("follow_loan takes a shared element loan and a typed weak pointer")); };
                if !self.scalar_proof { return Err(self.proof_required("following a weak pointer requires a proof")); }
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let witness = self.expr(witness)?;
                let Ty::Borrow { mutable, pointee } = witness.ty else { return Err(error("following a pointer requires an element loan")); };
                if mutable != (method == "follow_loan_mut") { return Err(error("weak access requires compatible loan mutability")); }
                let link = self.expr(link)?;
                let Ty::Weak(id) = link.ty else { return Err(error("follow_loan needs a typed weak pointer")); };
                if self.layouts.buffers()[id as usize].element != pointee.ty() { return Err(error("weak pointer and loan have different element types")); }
                let result = witness.ty;
                self.keep(saved, mark, result);
                let op = if mutable { crate::compiler::operations::BoxOp::FollowLoanMut } else { crate::compiler::operations::BoxOp::FollowLoan };
                (result, Value::BoxOp(op, id, vec![witness, link]))
            }
            "buffer_pointer" => {
                let [arg] = args else { return Err(error("buffer_pointer takes one shared Buffer reference")); };
                let id = self.buffer_argument(arg)?;
                let saved = self.loans.clone();
                let owner = self.argument(arg, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(id) });
                self.loans = saved;
                (Ty::Weak(id), Value::BoxOp(crate::compiler::operations::BoxOp::FromBuffer, id, vec![owner?]))
            }
            "capacity" | "allocation_key" => {
                let [arg] = args else { return Err(error("a buffer query takes one shared Buffer reference")); };
                let buffer = self.buffer_argument(arg)?;
                let saved = self.loans.clone();
                let value = self.argument(arg, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(buffer) });
                self.loans = saved;
                let value = Box::new(value?);
                (Ty::U64, if method == "allocation_key" { Value::AllocationKey(value, buffer + 1) } else { Value::Capacity(value, buffer + 1) })
            }
            // Tasks (library/wasm-tasks.wy). A function with a contract has no proof model
            // for a pause, so it neither pauses nor runs a task.
            "pause" | "step" | "finish" | "status" | "first_word" if self.contracted => return Err(error("a function with a contract cannot run, make or pause a task")),
            // `memory.pause(token)`: only where the signature says the function may pause,
            // so the effect check (effects.rs) sees every pause through signatures.
            "pause" => {
                if !self.suspends { return Err(error("`memory.pause` is only in a function whose signature says it may pause: `-> T suspends`")); }
                // It returns the word the step that resumes the task hands it.
                let [token] = args else { return Err(error("memory.pause takes one U64, what the task waits for")); };
                (Ty::U64, Value::Pause(Box::new(self.expect(token, Ty::U64)?)))
            }
            // `memory.step(task, word)`: runs a `&mut Task` until it finishes, Ok(()), or
            // pauses, Err(token); a paused task's pause returns `word`.
            "step" => {
                let [task, word] = args else { return Err(error("memory.step takes a `&mut Task` and the U64 its pause returns")); };
                let word = self.expect(word, Ty::U64)?;
                let saved = self.loans.clone();
                let task = self.argument(task, Ty::Borrow { mutable: true, pointee: Pointee::Host(crate::compiler::host::Host::Task) });
                self.loans = saved;
                let ty = self.layouts.result([Ty::Unit, Ty::U64])?;
                (ty, Value::TaskStep(Box::new(task?), Box::new(word)))
            }
            // `memory.first_word()`: in a task, before its first pause, the word its first
            // step handed in (CANCELLED when it was cancelled before it began).
            "first_word" => {
                if !args.is_empty() { return Err(error("memory.first_word takes nothing")); }
                (Ty::U64, Value::TaskWord)
            }
            // `memory.in_task()`: whether this code runs inside a task (`run_task`, which
            // blocks until its work finishes, refuses to run there: review).
            "in_task" => {
                if !args.is_empty() { return Err(error("memory.in_task takes nothing")); }
                (Ty::Bool, Value::InTask)
            }
            // `memory.status(&task)`: 0 not yet stepped, 1 paused, 2 finished.
            "status" => {
                let [task] = args else { return Err(error("memory.status takes one `&Task`")); };
                let saved = self.loans.clone();
                let task = self.argument(task, Ty::Borrow { mutable: false, pointee: Pointee::Host(crate::compiler::host::Host::Task) });
                self.loans = saved;
                (Ty::U64, Value::TaskStatus(Box::new(task?)))
            }
            // `memory.finish(task)`: gives back a finished task's memory; an unfinished one traps.
            "finish" => {
                let [task] = args else { return Err(error("memory.finish consumes one Task")); };
                let moved = if matches!(&task.kind, a::Expression::Unary("move", _)) { task.clone() } else { a::Expr { kind: a::Expression::Unary("move", Box::new(task.clone())), span: task.span.clone() } };
                (Ty::Unit, Value::TaskFinish(Box::new(self.expect(&moved, Ty::Host(crate::compiler::host::Host::Task))?)))
            }
            // `memory.share(&allocator)`: another handle to the one heap, for a holder
            // that must own one (a task's input). It adds no authority: whoever lends
            // `&Allocator` already lets the borrower allocate.
            "share" if self.contracted => return Err(error("a function with a contract cannot share an allocator")),
            // The clock and randomness too: each has one value per run, and a task given its
            // own learns nothing a lent `&Clock` or `&Rand` would not tell it.
            "share" => {
                let [given] = args else { return Err(error("memory.share takes one `&Allocator`, `&Clock` or `&Rand`")); };
                use crate::compiler::host::Host;
                let (ty, pointee, value) = match self.hint(given) {
                    Some(Ty::Borrow { pointee: Pointee::Host(host @ (Host::Clock | Host::Rand)), .. }) => (Ty::Host(host), Pointee::Host(host), Value::EntryHost(host, host.code())),
                    _ => (Ty::Allocator, Pointee::Allocator, Value::EntryAllocator),
                };
                let saved = self.loans.clone();
                let checked = self.argument(given, Ty::Borrow { mutable: false, pointee });
                self.loans = saved;
                checked?;
                (ty, value)
            }
            "release" => return Err(error("memory.release was replaced by memory.try_release")),
            "try_release" => {
                let [arg] = args else { return Err(error("try_release consumes one Buffer")); };
                let value = self.expr(arg)?;
                let Ty::Buffer(buffer) = value.ty else { return Err(error("try_release consumes an owned Buffer value")); };
                let ty = self.layouts.result([Ty::Unit, value.ty])?;
                (ty, Value::TryRelease(Box::new(value), buffer + 1))
            }
            "get_unchecked" | "initialized" => {
                let [arg, index] = args else {
                    return Err(error("memory proof read/probe takes shared Buffer reference and U64 index"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                // A read copies the cell; a probe only looks at its occupancy.
                if method == "get_unchecked" { self.layouts.copy_storage(element)?; }
                let (op, ty) = if method == "get_unchecked" {
                    self.element_limits.push(element);
                    (StorageOp::GetUnchecked, element)
                } else { (StorageOp::Initialized, Ty::Bool) };
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(index, Ty::U64)?;
                    Ok((receiver, index))
                })();
                self.loans = saved;
                let (receiver, index) = checked?;
                (ty, Value::Storage { op, receiver: Box::new(receiver), index: Box::new(index), value: None, buffer })
            }
            "ref_unchecked" | "mut_unchecked" | "ref_link" | "mut_link" => {
                let (mutable, op) = match method {
                    "ref_unchecked" => (false, StorageOp::RefUnchecked),
                    "mut_unchecked" => (true, StorageOp::MutUnchecked),
                    "ref_link" => (false, StorageOp::RefLink),
                    _ => (true, StorageOp::MutLink),
                };
                let [arg, position] = args else {
                    return Err(error("a cell loan takes a Buffer reference and an index or allocation key"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                let pointee = Pointee::from_ty(element).ok_or_else(|| error("a cell loan needs a storable element"))?;
                let ty = Ty::Borrow { mutable, pointee };
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable, pointee: Pointee::Buffer(buffer) })?;
                    Ok((receiver, self.expect(position, Ty::U64)?))
                })();
                if checked.is_err() { self.loans = saved.clone(); }
                let (receiver, index) = checked?;
                // Keep the owner's loan until the last use of the resulting reference.
                self.keep(saved, mark, ty);
                (ty, Value::Storage { op, receiver: Box::new(receiver), index: Box::new(index), value: None, buffer })
            }
            // `memory.slice_unchecked(&buffer, from, to)`: cells `from..to` as a slice. Its
            // precondition (from <= to <= capacity, each cell initialized) is proved.
            // `slice_mut_unchecked(&mut buffer, from, to)` is the same, lent uniquely: its
            // elements may be written, never left without a value (the proof is the same).
            "slice_unchecked" | "slice_mut_unchecked" => {
                let mutable = method == "slice_mut_unchecked";
                let [arg, from, to] = args else {
                    return Err(error("memory.slice_unchecked takes a Buffer reference and two U64 positions"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                let Ty::Slice(id) = self.layouts.slice_of(element)? else { unreachable!() };
                let ty = Ty::Borrow { mutable, pointee: Pointee::Slice(id) };
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable, pointee: Pointee::Buffer(buffer) })?;
                    let from = self.expect(from, Ty::U64)?;
                    let to = self.expect(to, Ty::U64)?;
                    Ok((receiver, from, to))
                })();
                if checked.is_err() { self.loans = saved.clone(); }
                let (receiver, from, to) = checked?;
                // The cells stay where they are: the buffer is lent while the slice is used.
                self.keep(saved, mark, ty);
                (ty, Value::Storage { op: StorageOp::SliceUnchecked, receiver: Box::new(receiver), index: Box::new(from), value: Some(Box::new(to)), buffer })
            }
            "init_unchecked" => {
                let [arg, index, item] = args else {
                    return Err(error("memory.init_unchecked takes exclusive Buffer reference, U64 index and item"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                self.element_limits.push(element);
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(index, Ty::U64)?;
                    // Unlike checked init, payload ALWAYS executes; the exclusive
                    // receiver loan remains live through index and payload checking.
                    let value = self.expect(item, element)?;
                    Ok((receiver, index, value))
                })();
                self.loans = saved;
                let (receiver, index, value) = checked?;
                (Ty::Unit, Value::Storage { op: StorageOp::InitUnchecked, receiver: Box::new(receiver),
                    index: Box::new(index), value: Some(Box::new(value)), buffer })
            }
            "move_prefix_unchecked" => {
                let [from, to, count] = args else {
                    return Err(error("memory.move_prefix_unchecked takes two exclusive Buffer references of one element type and a U64 count"));
                };
                let buffer = self.buffer_argument(from)?;
                if self.buffer_argument(to)? != buffer {
                    return Err(error("memory.move_prefix_unchecked moves between Buffers of one element type"));
                }
                let element = self.layouts.buffers()[buffer as usize].element;
                self.element_limits.push(element);
                let saved = self.loans.clone();
                // Both buffers are lent uniquely at once: the loan check refuses one
                // buffer passed twice.
                let checked: Result<_> = (|| {
                    let source = self.argument(from, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let target = self.argument(to, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let count = self.expect(count, Ty::U64)?;
                    Ok((source, target, count))
                })();
                self.loans = saved;
                let (source, target, count) = checked?;
                (Ty::Unit, Value::Storage { op: StorageOp::MovePrefixUnchecked, receiver: Box::new(source),
                    index: Box::new(count), value: Some(Box::new(target)), buffer })
            }
            "take_prefix_unchecked" => {
                let [arg, count] = args else {
                    return Err(error("memory.take_prefix_unchecked takes an exclusive Buffer reference and a U64 count"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                // Emptying an owner's cell would lose the owner.
                if !self.layouts.properties(element).copy {
                    return Err(error("memory.take_prefix_unchecked empties Copy elements only: an element that holds an owner is taken out, not dropped"));
                }
                self.element_limits.push(element);
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let count = self.expect(count, Ty::U64)?;
                    Ok((receiver, count))
                })();
                self.loans = saved;
                let (receiver, count) = checked?;
                (Ty::Unit, Value::Storage { op: StorageOp::TakePrefixUnchecked, receiver: Box::new(receiver),
                    index: Box::new(count), value: None, buffer })
            }
            "read_word_unchecked" => {
                let [arg, at, count] = args else {
                    return Err(error("memory.read_word_unchecked takes a Buffer<U8> reference, a U64 position and a U64 count"));
                };
                let buffer = self.buffer_argument(arg)?;
                if self.layouts.buffers()[buffer as usize].element != Ty::Unsigned(crate::compiler::scalar::Scalar::U8) {
                    return Err(error("memory.read_word_unchecked reads bytes: its buffer is a Buffer<U8>"));
                }
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(buffer) })?;
                    let at = self.expect(at, Ty::U64)?;
                    let count = self.expect(count, Ty::U64)?;
                    Ok((receiver, at, count))
                })();
                self.loans = saved;
                let (receiver, at, count) = checked?;
                (Ty::U64, Value::Storage { op: StorageOp::ReadWordUnchecked, receiver: Box::new(receiver),
                    index: Box::new(at), value: Some(Box::new(count)), buffer })
            }
            "init_word_unchecked" => {
                let [arg, at, word, count] = args else {
                    return Err(error("memory.init_word_unchecked takes an exclusive Buffer<U8> reference, a U64 position, a U64 word and a U64 count"));
                };
                let buffer = self.buffer_argument(arg)?;
                if self.layouts.buffers()[buffer as usize].element != Ty::Unsigned(crate::compiler::scalar::Scalar::U8) {
                    return Err(error("memory.init_word_unchecked writes bytes: its buffer is a Buffer<U8>"));
                }
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let at = self.expect(at, Ty::U64)?;
                    let word = self.expect(word, Ty::U64)?;
                    let count = self.expect(count, Ty::U64)?;
                    Ok((receiver, at, word, count))
                })();
                self.loans = saved;
                let (receiver, at, word, count) = checked?;
                let pair = Expr { ty: Ty::Unit, kind: Value::Aggregate(vec![word, count]) };
                (Ty::Unit, Value::Storage { op: StorageOp::InitWordUnchecked, receiver: Box::new(receiver),
                    index: Box::new(at), value: Some(Box::new(pair)), buffer })
            }
            "fill_unchecked" => {
                let [arg, count, item] = args else {
                    return Err(error("memory.fill_unchecked takes an exclusive Buffer reference, a U64 count and an item"));
                };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                // One value in many cells is a copy of it.
                if !self.layouts.properties(element).copy {
                    return Err(error("memory.fill_unchecked fills with Copy elements only: an owner cannot be in two cells"));
                }
                self.element_limits.push(element);
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let count = self.expect(count, Ty::U64)?;
                    let value = self.expect(item, element)?;
                    Ok((receiver, count, value))
                })();
                self.loans = saved;
                let (receiver, count, value) = checked?;
                (Ty::Unit, Value::Storage { op: StorageOp::FillUnchecked, receiver: Box::new(receiver),
                    index: Box::new(count), value: Some(Box::new(value)), buffer })
            }
            "init_slice_unchecked" => {
                let [target, at, source] = args else {
                    return Err(error("memory.init_slice_unchecked takes a mutable buffer, an offset and a shared slice"));
                };
                let buffer = self.buffer_argument(target)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                if !matches!(element, Ty::Unsigned(s) if s.layout().maximum() < u64::MAX) {
                    return Err(error("memory.init_slice_unchecked requires a narrow unsigned element type"));
                }
                let Ty::Slice(slice) = self.layouts.slice_of(element)? else { unreachable!("slice type") };
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let target = self.argument(target, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let at = self.expect(at, Ty::U64)?;
                    let source = self.argument(source, Ty::Borrow { mutable: false, pointee: Pointee::Slice(slice) })?;
                    Ok((target, at, source))
                })();
                self.loans = saved;
                let (target, at, source) = checked?;
                (Ty::Unit, Value::Storage { op: StorageOp::InitSliceUnchecked, receiver: Box::new(target),
                    index: Box::new(at), value: Some(Box::new(source)), buffer })
            }
            "copy_into_unchecked" => {
                let [from, to, at, count] = args else {
                    return Err(error("memory.copy_into_unchecked takes a Buffer reference, an exclusive Buffer reference of the same element type, a U64 offset and a U64 count"));
                };
                let buffer = self.buffer_argument(from)?;
                if self.buffer_argument(to)? != buffer {
                    return Err(error("memory.copy_into_unchecked copies between Buffers of one element type"));
                }
                let element = self.layouts.buffers()[buffer as usize].element;
                // A copy of an owner would be a second owner of one thing.
                if !self.layouts.properties(element).copy {
                    return Err(error("memory.copy_into_unchecked copies Copy elements only: an element that holds an owner is moved, not copied"));
                }
                self.element_limits.push(element);
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let source = self.argument(from, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(buffer) })?;
                    let target = self.argument(to, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let at = self.expect(at, Ty::U64)?;
                    let count = self.expect(count, Ty::U64)?;
                    Ok((source, target, at, count))
                })();
                self.loans = saved;
                let (source, target, at, count) = checked?;
                let pair = Expr { ty: Ty::Unit, kind: Value::Aggregate(vec![target, at]) };
                (Ty::Unit, Value::Storage { op: StorageOp::CopyIntoUnchecked, receiver: Box::new(source),
                    index: Box::new(count), value: Some(Box::new(pair)), buffer })
            }
            "take_unchecked" | "replace_unchecked" => {
                let replacing = method == "replace_unchecked";
                if args.len() != if replacing { 3 } else { 2 } {
                    return Err(error("unchecked take or replace takes an exclusive Buffer loan, index and (for replace) the new value"));
                }
                let buffer = self.buffer_argument(&args[0])?;
                let element = self.layouts.buffers()[buffer as usize].element;
                self.element_limits.push(element);
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(&args[0], Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(&args[1], Ty::U64)?;
                    let value = args.get(2).map(|item| self.expect(item, element).map(Box::new)).transpose()?;
                    Ok((receiver, index, value))
                })();
                self.loans = saved;
                let (receiver, index, value) = checked?;
                let op = if replacing { StorageOp::ReplaceUnchecked } else { StorageOp::TakeUnchecked };
                (element, Value::Storage { op, receiver: Box::new(receiver),
                    index: Box::new(index), value, buffer })
            }
            "init" | "read" | "take" | "swap" => {
                let op = match method { "init" => StorageOp::Init, "read" => StorageOp::Read,
                    "take" => StorageOp::Take, _ => StorageOp::Swap };
                let payload = matches!(op, StorageOp::Init | StorageOp::Swap);
                if args.len() != if payload { 3 } else { 2 } {
                    return Err(error("typed storage operation takes Buffer borrow, index and optional value"));
                }
                let buffer = self.buffer_argument(&args[0])?;
                let element = self.layouts.buffers()[buffer as usize].element;
                // Only take moves the cell out. A read copies it, and a guarded
                // init or swap drops its payload when the guard fails.
                if op != StorageOp::Take { self.layouts.copy_storage(element)?; }
                self.element_limits.push(element);
                let ty = self.layouts.result([if op == StorageOp::Init { Ty::Unit } else { element }, Ty::U64])?;
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(&args[0], Ty::Borrow { mutable: op != StorageOp::Read,
                        pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(&args[1], Ty::U64)?;
                    // Receiver and index ALWAYS execute. Only payload is skipped on normal guard Err.
                    let skipped = self.flow.clone();
                    let value = if payload { Some(Box::new(self.expect(&args[2], element)?)) } else { None };
                    if payload { self.flow = Flow::join(&skipped, &[(&skipped, false), (&self.flow, false)]); }
                    Ok((receiver, index, value))
                })();
                self.loans = saved; // retain enclosing call loans even after an intrinsic error
                let (receiver, index, value) = checked?;
                (ty, Value::Storage { op, receiver: Box::new(receiver), index: Box::new(index), value, buffer })
            }
            "ref" => {
                let [arg, index] = args else { return Err(error("memory.ref takes shared Buffer reference and U64 index")); };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                let pointee = Pointee::from_ty(element).ok_or_else(|| error("memory.ref needs a storable element"))?;
                self.element_limits.push(element);
                let ty = self.layouts.result([Ty::Borrow { mutable: false, pointee }, Ty::U64])?;
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: false, pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(index, Ty::U64)?;
                    Ok((receiver, index))
                })();
                if checked.is_err() { self.loans = saved.clone(); }
                let (receiver, index) = checked?;
                // The buffer stays lent for as long as the reference is used.
                self.keep(saved, mark, ty);
                (ty, Value::Storage { op: StorageOp::Ref, receiver: Box::new(receiver), index: Box::new(index), value: None, buffer })
            }
            "mut" => {
                let [arg, index] = args else { return Err(error("memory.mut takes exclusive Buffer reference and U64 index")); };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                let pointee = Pointee::from_ty(element).ok_or_else(|| error("memory.mut needs a storable element"))?;
                self.element_limits.push(element);
                let ty = self.layouts.result([Ty::Borrow { mutable: true, pointee }, Ty::U64])?;
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    if let Value::Address(place) = &receiver.kind { self.lasting(place, false)?; }
                    let index = self.expect(index, Ty::U64)?;
                    Ok((receiver, index))
                })();
                if checked.is_err() { self.loans = saved.clone(); }
                let (receiver, index) = checked?;
                // The buffer stays uniquely lent for as long as the reference is used.
                self.keep(saved, mark, ty);
                (ty, Value::Storage { op: StorageOp::Ref, receiver: Box::new(receiver), index: Box::new(index), value: None, buffer })
            }
            "try_init" => {
                let [arg, index, item] = args else { return Err(error("memory.try_init takes exclusive Buffer reference, U64 index and item")); };
                let buffer = self.buffer_argument(arg)?;
                let element = self.layouts.buffers()[buffer as usize].element;
                self.element_limits.push(element);
                // The item is handed back when the cell is out of range or already full, so it may be an owner.
                let ty = self.layouts.result([Ty::Unit, element])?;
                let saved = self.loans.clone();
                let checked: Result<_> = (|| {
                    let receiver = self.argument(arg, Ty::Borrow { mutable: true, pointee: Pointee::Buffer(buffer) })?;
                    let index = self.expect(index, Ty::U64)?;
                    let item = self.expect(item, element)?;
                    Ok((receiver, index, item))
                })();
                self.loans = saved;
                let (receiver, index, item) = checked?;
                (ty, Value::Storage { op: StorageOp::TryInit, receiver: Box::new(receiver), index: Box::new(index), value: Some(Box::new(item)), buffer })
            }
            _ => return Err(error(format!("unsupported memory primitive: `memory.{method}`"))),
        })
    }
}
