use super::*;

impl Emitter<'_> {
    /// After a site: when the call paused, note which site this is and leave for the save.
    pub(super) fn unwinding(&mut self, out: &mut Vec<u8>) {
        indexed(out, 0x23, STATE); Self::i32_const(out, UNWIND); out.extend([0x46, 0x04, 0x40]);
        Self::i32_const(out, self.site); indexed(out, 0x21, self.resume);
        out.push(0x0c); unsigned(out, self.depth + self.labels + 1);
        out.push(0x0b);
        self.site += 1;
    }
    /// A function that may pause:
    ///
    /// ```text
    /// if STATE == REWIND: restore the locals from the save area   else: the prologue
    /// block: body                                ;; a site that paused leaves this block
    ///        (return)
    /// save the locals to the save area; return zeros
    /// ```
    /// The locals are saved innermost function first and restored outermost first: the save
    /// area is a stack. Every local is saved, the site number with them.
    pub(super) fn resumable_body(&mut self, out: &mut Vec<u8>, f: &Function) {
        self.resume = self.temporary(Ty::Bool);
        let cursor = self.temporary(Ty::Bool);
        let mut prologue = Vec::new();
        self.prologue(&mut prologue, f.params.len() as u32);
        let mut body = vec![0x02, 0x40];
        self.block(&mut body, &f.body);
        if f.result != Ty::Unit { body.push(0x00); } else { self.restore(&mut body); body.push(0x0f); }
        body.push(0x0b);
        // What is saved: the locals live after some site (pause.rs `saved`), the site number,
        // and the frame's pointers. The emitter's other temporaries are made again going back in.
        let live = crate::compiler::pause::saved(&f.body, f.locals.len(), &self.tasks.pausing, self.layouts);
        // A local that lives in the frame is read and written there, and the frame stays on the
        // task's stack across the pause (its pointers are saved): it is not saved again (review:
        // an indexed [U8; 130] took 1040 bytes of save area and trapped in a 1 KiB task).
        let mut kept: Vec<u32> = (0..f.locals.len() as u32).filter(|slot| live[*slot as usize] && !self.frame.slots.contains_key(slot)).collect();
        kept.push(self.resume);
        kept.extend(self.old_sp.into_iter().chain(self.base));
        let types: Vec<u8> = self.locals.iter().map(|t| valtype(*t)).collect();
        let size = 8 * kept.len() as u32;
        // Addresses are computed, every access at offset zero (the host's profile asks it).
        let slot_at = |out: &mut Vec<u8>, position: usize| { get(out, cursor); if position > 0 { Self::i32_const(out, 8 * position as u32); out.push(0x6a); } };
        let access = |out: &mut Vec<u8>, slot: u32, store: bool| {
            // By the local's own type: an f64 went through i32 (review of F64: invalid Wasm).
            let op = match (types[slot as usize], store) { (0x7e, false) => [0x29, 3], (0x7e, true) => [0x37, 3], (0x7c, false) => [0x2b, 3], (0x7c, true) => [0x39, 3], (_, false) => [0x28, 2], (_, true) => [0x36, 2] };
            out.extend(op); out.push(0);
        };
        let header = |out: &mut Vec<u8>| { indexed(out, 0x23, TASK); Self::i32_const(out, CURSOR); out.push(0x6a); };
        // Back in: take this function's record off the save area.
        let mut restore = Vec::new();
        header(&mut restore); restore.extend([0x28, 2, 0]);
        Self::i32_const(&mut restore, size); restore.push(0x6b); indexed(&mut restore, 0x21, cursor);
        header(&mut restore); get(&mut restore, cursor); restore.extend([0x36, 2, 0]);
        for (position, slot) in kept.iter().enumerate() {
            slot_at(&mut restore, position); access(&mut restore, *slot, false); indexed(&mut restore, 0x21, *slot);
        }
        // Out: put it on, within the save area.
        let mut save = Vec::new();
        header(&mut save); save.extend([0x28, 2, 0]); indexed(&mut save, 0x21, cursor);
        get(&mut save, cursor); Self::i32_const(&mut save, size); save.push(0x6a); indexed(&mut save, 0x23, LOW); save.push(0x4b); trap_if(&mut save);
        for (position, slot) in kept.iter().enumerate() { slot_at(&mut save, position); get(&mut save, *slot); access(&mut save, *slot, true); }
        header(&mut save); get(&mut save, cursor); Self::i32_const(&mut save, size); save.push(0x6a); save.extend([0x36, 2, 0]);
        for ty in self.layouts.flat(f.result) { Self::zero_carrier(&mut save, ty); }
        indexed(out, 0x23, STATE); Self::i32_const(out, REWIND); out.extend([0x46, 0x04, 0x40]);
        out.extend(restore);
        out.push(0x05);
        out.extend(prologue);
        out.push(0x0b);
        out.extend(body);
        out.extend(save);
    }
    /// The block's address, as an i64 local, from a task's token (as `proved_cell` finds a cell).
    pub(super) fn task_base(&mut self, out: &mut Vec<u8>, token: u32) -> u32 {
        Self::allocation_base(out, token);
        let base = self.temporary(Ty::U64);
        indexed(out, 0x21, base);
        base
    }
    /// Bytes of the block before the save area: the header and the input.
    fn entry_layout(&self, entry: crate::compiler::pause::Entry) -> u32 {
        INPUT + (self.layouts.storage(entry.input).expect("a stored input").0.div_ceil(16) * 16) as u32
    }
    /// `Task.spawn`: a block of `INPUT + input + 2 * bytes`, its header set and the input moved
    /// in. `Err(input)` when there is no memory.
    pub(super) fn spawn(&mut self, out: &mut Vec<u8>, result: Ty, allocator: &Expr, bytes: &Expr, entry: &Expr, buffer: u32) {
        let Value::Call(function, args) = &entry.kind else { unreachable!("a task's entry is a call") };
        let [input] = args.as_slice() else { unreachable!("an entry takes its input") };
        let entry = crate::compiler::pause::Entry { function: *function, input: input.ty };
        let kind = self.tasks.entries.iter().position(|e| *e == entry).expect("every entry is listed") as u32;
        let start = self.entry_layout(entry);
        self.expr(out, allocator);
        let permission = self.temporary(Ty::Bool); indexed(out, 0x21, permission);
        let size = self.capture(out, bytes);
        let given = self.capture(out, input);
        // A stack of 512 bytes to 1 MiB, in sixteens; the save area is as large.
        get(out, size); out.extend([0x42, 15, 0x83, 0x42, 0, 0x52]); trap_if(out);
        get(out, size); out.push(0x42); signed(out, 512); out.push(0x54); trap_if(out);
        get(out, size); out.push(0x42); signed(out, 1 << 20); out.push(0x56); trap_if(out);
        get(out, permission); Self::memory(out, Ty::Allocator, false);
        out.push(0x42); signed(out, buffer as i64);
        out.push(0x42); signed(out, start as i64); get(out, size); out.extend([0x42, 1, 0x86, 0x7c, 0x42, 3, 0x88]);
        call(out, self.indices.try_alloc);
        let _error = self.temporary(Ty::U64); indexed(out, 0x21, _error);
        let token = self.temporary(Ty::U64); indexed(out, 0x21, token);
        let tag = self.temporary(Ty::Tag); indexed(out, 0x21, tag);
        let output = self.result_locals(out, result);
        let starts = self.layouts.sum(result).expect("Result<Task, A>").starts.clone();
        get(out, tag); out.extend([0x45, 0x04, 0x40]);
        let base = self.task_base(out, token);
        let word = |out: &mut Vec<u8>, offset: u32, value: &dyn Fn(&mut Vec<u8>)| { Self::heap_address(out, base, offset as u64); value(out); out.extend([0x36, 2, 0]); };
        let at = |out: &mut Vec<u8>, plus: u32, sizes: u32| {
            get(out, base); out.push(0x42); signed(out, plus as i64); out.push(0x7c);
            for _ in 0..sizes { get(out, size); out.push(0x7c); }
            out.push(0xa7);
        };
        word(out, STATUS, &|out| Self::i32_const(out, 0));
        word(out, KIND, &|out| Self::i32_const(out, kind));
        word(out, SAVED_SP, &|out| Self::i32_const(out, 0));
        word(out, CURSOR, &|out| at(out, start, 0));
        word(out, STACK_LOW, &|out| at(out, start, 1));
        word(out, STACK_HIGH, &|out| at(out, start, 2));
        Self::heap_address(out, base, TOKEN as u64); out.extend([0x42, 0, 0x37, 3, 0]);
        // The input, moved in.
        let cells = self.layouts.cells(entry.input).expect("input cells");
        let guards = self.layouts.guards(entry.input);
        let to = self.temporary(Ty::U64);
        get(out, base); out.push(0x42); signed(out, INPUT as i64); out.push(0x7c); indexed(out, 0x21, to);
        Self::store_cells(out, &cells, &guards, to, given);
        get(out, token); indexed(out, 0x21, output + starts[0]);
        out.push(0x05);
        self.set_tag(out, output, 1);
        for slot in 0..self.layouts.flat(entry.input).len() as u32 { get(out, given + slot); indexed(out, 0x21, output + starts[1] + slot); }
        out.push(0x0b);
        for slot in 0..self.layouts.flat(result).len() as u32 { get(out, output + slot); }
    }
    /// The local a variant number is about to be written into must be a tag word. A sum whose
    /// layout has none (`zero_arm`: zero is one variant, a nonzero owner the other) reaching an
    /// operation that builds its result by hand is a compiler defect; it stops the compiler here
    /// instead of leaving the engine to refuse the module (review 2026-11-21, `try_init`).
    pub(super) fn tag_word(&self, local: u32) {
        assert_eq!(self.locals.get(local as usize), Some(&Ty::Tag), "a variant number is written only into a tag word");
    }
    pub(super) fn set_tag(&self, out: &mut Vec<u8>, local: u32, variant: u32) {
        self.tag_word(local);
        Self::i32_const(out, variant); indexed(out, 0x21, local);
    }
    /// Locals for a value of `ty`, each zero.
    pub(super) fn result_locals(&mut self, out: &mut Vec<u8>, ty: Ty) -> u32 {
        let flat = self.layouts.flat(ty);
        let output = self.locals.len() as u32;
        self.locals.extend(&flat);
        for (slot, carrier) in flat.iter().enumerate() { Self::zero_carrier(out, *carrier); indexed(out, 0x21, output + slot as u32); }
        output
    }
    /// `memory.step(&mut task)`: the frames are bounded by the task's stack while it runs; its
    /// kind's entry is called, which goes back in where it paused, if it did. `Err(token)` when
    /// it pauses again, `Ok(())` when it finishes. What was running before is as it was.
    pub(super) fn step(&mut self, out: &mut Vec<u8>, result: Ty, task: &Expr, word: &Expr) {
        // The word first, as it is checked (on the caller's stack, before the task's is chosen),
        // then the task its place holds now: the word's own calls may have changed which.
        self.expr(out, word);
        let handed = self.temporary(Ty::U64); indexed(out, 0x21, handed);
        self.expr(out, task); Self::memory(out, Ty::Host(crate::compiler::host::Host::Task), false);
        let token = self.temporary(Ty::U64); indexed(out, 0x21, token);
        let base = self.task_base(out, token);
        let saved: Vec<(u32, u32)> = [TASK, 0, LOW, HIGH].into_iter().map(|g| (g, self.temporary(Ty::Bool))).collect();
        for (global, local) in &saved { indexed(out, 0x23, *global); indexed(out, 0x21, *local); }
        let load = |out: &mut Vec<u8>, offset: u32| { Self::heap_address(out, base, offset as u64); out.extend([0x28, 2, 0]); };
        let status = self.temporary(Ty::Bool);
        load(out, STATUS); indexed(out, 0x21, status);
        get(out, status); Self::i32_const(out, 2); out.push(0x46); trap_if(out);
        get(out, base); out.push(0xa7); indexed(out, 0x24, TASK);
        load(out, STACK_LOW); indexed(out, 0x24, LOW);
        load(out, STACK_HIGH); indexed(out, 0x24, HIGH);
        // The word goes in the token's place for a paused task's pause to return, and for a new
        // one to read at its start (`first_word`).
        Self::heap_address(out, base, TOKEN as u64); get(out, handed); out.extend([0x37, 3, 0]);
        get(out, status); Self::i32_const(out, 1); out.extend([0x46, 0x04, 0x40]);
        load(out, SAVED_SP); indexed(out, 0x24, 0);
        Self::i32_const(out, REWIND); indexed(out, 0x24, STATE);
        out.push(0x05);
        indexed(out, 0x23, HIGH); indexed(out, 0x24, 0);
        out.push(0x0b);
        let kind = self.temporary(Ty::Bool);
        load(out, KIND); indexed(out, 0x21, kind);
        for (ordinal, entry) in self.tasks.entries.clone().into_iter().enumerate() {
            get(out, kind); Self::i32_const(out, ordinal as u32); out.extend([0x46, 0x04, 0x40]);
            let cells = self.layouts.cells(entry.input).expect("input cells");
            let guards = self.layouts.guards(entry.input);
            let at = self.temporary(Ty::U64);
            get(out, base); out.push(0x42); signed(out, INPUT as i64); out.push(0x7c); indexed(out, 0x21, at);
            let input = self.locals.len() as u32; self.locals.extend(self.layouts.flat(entry.input));
            Self::load_cells(out, &cells, &guards, at, Some(input));
            for slot in 0..self.layouts.flat(entry.input).len() as u32 { get(out, input + slot); }
            call(out, entry.function + self.indices.source);
            out.push(0x0b);
        }
        let output = self.result_locals(out, result);
        let starts = self.layouts.sum(result).expect("Result<(), U64>").starts.clone();
        indexed(out, 0x23, STATE); Self::i32_const(out, UNWIND); out.extend([0x46, 0x04, 0x40]);
        Self::i32_const(out, NORMAL); indexed(out, 0x24, STATE);
        Self::heap_address(out, base, SAVED_SP as u64); indexed(out, 0x23, 0); out.extend([0x36, 2, 0]);
        Self::heap_address(out, base, STATUS as u64); Self::i32_const(out, 1); out.extend([0x36, 2, 0]);
        self.set_tag(out, output, 1);
        Self::heap_address(out, base, TOKEN as u64); out.extend([0x29, 3, 0]); indexed(out, 0x21, output + starts[1]);
        out.push(0x05);
        Self::heap_address(out, base, STATUS as u64); Self::i32_const(out, 2); out.extend([0x36, 2, 0]);
        out.push(0x0b);
        for (global, local) in &saved { get(out, *local); indexed(out, 0x24, *global); }
        for slot in 0..self.layouts.flat(result).len() as u32 { get(out, output + slot); }
    }
}
