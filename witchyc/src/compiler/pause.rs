//! Pausing (docs/design-async.md, "How a pause works"): which concrete functions may pause,
//! and the one rewrite that makes their bodies resumable.
//!
//! A task pauses by unwinding: every function on its stack saves its Wasm locals and the
//! number of the call it was in, innermost first, and returns. Stepping the task again calls
//! its first function, which restores its locals and goes straight back into that call, and so
//! on down to the pause, which returns. Going back in must run nothing a second time, so in a
//! function that may pause each such call (a *site*) is its own statement, `t = call(a, b)`,
//! whose arguments are locals: a statement before it is skipped on the way back in, and one
//! that holds it is entered. What a statement evaluated before the call is evaluated first, into
//! locals, by statements of its own, so the order of evaluation is the source's.
//!
//! Nothing here is seen by the checker or a proof: a pause is invisible to the function that
//! pauses, which continues where it was with the values it had.
use super::ir::{Binary, Expr, Program, Stmt, Ty, Value};
use super::layout::Layouts;
use super::place::{Place, Projection};
use crate::diagnostic::Diagnostic;

type Result<T> = std::result::Result<T, Diagnostic>;

/// What the emitter needs to know about tasks in a program.
#[derive(Debug, Default)]
pub(super) struct Tasks {
    /// Per function: whether it may pause, and so is emitted resumable.
    pub pausing: Vec<bool>,
    /// The functions a task starts with, one per kind of task: a step calls the kind's.
    pub entries: Vec<Entry>,
    /// The program makes, runs or pauses a task: frames are bounded by globals, which a step
    /// moves to the task's own stack.
    pub used: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Entry { pub function: u32, pub input: Ty }

fn refuse(message: &'static str) -> Diagnostic { Diagnostic::new("W0401", 0..0, message) }

/// The expressions a value evaluates, in the order it evaluates them. A task's entry call is
/// not made where it is written, so its arguments are what a spawn evaluates.
pub(super) fn children(value: &Value) -> Vec<&Expr> {
    match value {
        Value::StaticStr(_) | Value::Unit | Value::U64(_) | Value::Bool(_) | Value::EntryConsole | Value::EntryInput | Value::EntryAllocator | Value::TaskWord | Value::InTask | Value::EntryHost(..)
        | Value::ReadPlace(_) | Value::Address(_) | Value::Move { .. } | Value::Extract { .. } | Value::Read(..) | Value::Exchange(..) => vec![],
        Value::TryAlloc { allocator, capacity, .. } => vec![allocator, capacity],
        Value::AllocationKey(v, _) | Value::Capacity(v, _) | Value::TryRelease(v, _) | Value::Discard(v) | Value::Sum { payload: v, .. } | Value::Repeat(v, _) | Value::Retag(v, _)
        | Value::Project(v, _) | Value::Widen(v) | Value::Not(v) | Value::Stored(_, v) | Value::Print(_, v, _)
        | Value::TaskFinish(v, _) | Value::TaskStatus(v) | Value::Pause(v) => vec![v],
        Value::TaskStep(v, w) => vec![v, w],
        Value::Storage { receiver, index, value, .. } => { let mut all: Vec<&Expr> = vec![receiver, index]; if let Some(v) = value { all.push(v); } all }
        Value::Aggregate(vs) | Value::Signed(_, vs) | Value::Float(_, vs) | Value::Bits(_, vs) | Value::BoxOp(_, _, vs) | Value::Call(_, vs) | Value::Host(_, _, vs) => vs.iter().collect(),
        Value::Index(a, b, _) | Value::SliceAt(a, b, ..) | Value::SliceIndex(a, b, ..) | Value::SliceCopy(a, b, _) | Value::SliceReadLe64(a, b) | Value::Binary(_, a, b) => vec![a, b],
        Value::SlicePart(a, b, c, ..) | Value::SliceWithin(a, b, c, ..) => vec![a, b, c],
        Value::TaskSpawn { allocator, bytes, entry, .. } => {
            let mut all: Vec<&Expr> = vec![allocator, bytes];
            if let Value::Call(_, args) = &entry.kind { all.extend(args.iter()); }
            all
        }
    }
}
fn children_mut(value: &mut Value) -> Vec<&mut Expr> {
    match value {
        Value::StaticStr(_) | Value::Unit | Value::U64(_) | Value::Bool(_) | Value::EntryConsole | Value::EntryInput | Value::EntryAllocator | Value::TaskWord | Value::InTask | Value::EntryHost(..)
        | Value::ReadPlace(_) | Value::Address(_) | Value::Move { .. } | Value::Extract { .. } | Value::Read(..) | Value::Exchange(..) => vec![],
        Value::TryAlloc { allocator, capacity, .. } => vec![allocator, capacity],
        Value::AllocationKey(v, _) | Value::Capacity(v, _) | Value::TryRelease(v, _) | Value::Discard(v) | Value::Sum { payload: v, .. } | Value::Repeat(v, _) | Value::Retag(v, _)
        | Value::Project(v, _) | Value::Widen(v) | Value::Not(v) | Value::Stored(_, v) | Value::Print(_, v, _)
        | Value::TaskFinish(v, _) | Value::TaskStatus(v) | Value::Pause(v) => vec![v],
        Value::TaskStep(v, w) => vec![v, w],
        Value::Storage { receiver, index, value, .. } => { let mut all: Vec<&mut Expr> = vec![receiver, index]; if let Some(v) = value { all.push(v); } all }
        Value::Aggregate(vs) | Value::Signed(_, vs) | Value::Float(_, vs) | Value::Bits(_, vs) | Value::BoxOp(_, _, vs) | Value::Call(_, vs) | Value::Host(_, _, vs) => vs.iter_mut().collect(),
        Value::Index(a, b, _) | Value::SliceAt(a, b, ..) | Value::SliceIndex(a, b, ..) | Value::SliceCopy(a, b, _) | Value::SliceReadLe64(a, b) | Value::Binary(_, a, b) => vec![a, b],
        Value::SlicePart(a, b, c, ..) | Value::SliceWithin(a, b, c, ..) => vec![a, b, c],
        Value::TaskSpawn { allocator, bytes, entry, .. } => {
            let mut all: Vec<&mut Expr> = vec![allocator, bytes];
            if let Value::Call(_, args) = &mut entry.kind { all.extend(args.iter_mut()); }
            all
        }
    }
}
/// The places a value reads or writes; an index into one is evaluated where the place is.
pub(super) fn places(value: &Value) -> Vec<&Place> {
    match value {
        Value::ReadPlace(p) | Value::Address(p) | Value::Move { place: p, .. } | Value::Extract { place: p, .. } | Value::Read(p, _) | Value::Stored(p, _) | Value::Print(p, _, _) => vec![p],
        Value::Host(_, Some(p), _) => vec![p],
        Value::Exchange(a, b) => vec![a, b],
        // Written out, not `_`: a new value that holds a place must be added above, or the trap
        // report (traps.rs) would not see its indexes (review 2026-11-21).
        // `AllocationKey` and `BoxOp` hold operands only (ir.rs: an expression and a list of
        // them, no `Place`); an index inside an operand is reached through `children`.
        Value::StaticStr(_) | Value::Unit | Value::U64(_) | Value::Bool(_) | Value::EntryConsole | Value::EntryInput | Value::EntryAllocator | Value::EntryHost(..)
        | Value::TryAlloc { .. } | Value::AllocationKey(..) | Value::BoxOp(..) | Value::Capacity(..) | Value::TryRelease(..) | Value::Storage { .. } | Value::Discard(_) | Value::Aggregate(_)
        | Value::Sum { .. } | Value::Repeat(..) | Value::Retag(..) | Value::Project(..) | Value::Index(..) | Value::SliceAt(..) | Value::SliceIndex(..)
        | Value::SlicePart(..) | Value::SliceWithin(..) | Value::SliceCopy(..) | Value::SliceReadLe64(..) | Value::Widen(_) | Value::Signed(..) | Value::Float(..) | Value::Bits(..) | Value::Not(_) | Value::Binary(..)
        | Value::Call(..) | Value::Host(_, None, _) | Value::TaskSpawn { .. } | Value::TaskStep(..) | Value::TaskStatus(_) | Value::TaskWord
        | Value::InTask | Value::TaskFinish(..) | Value::Pause(_) => vec![],
    }
}
pub(super) fn indices(place: &Place) -> impl Iterator<Item = &Expr> {
    place.projections.iter().filter_map(|p| if let Projection::Index { value, .. } = p { Some(value) } else { None })
}

/// Whether an expression in `body`, at any depth, is one `wanted` picks.
pub(super) fn any_expr(body: &[Stmt], wanted: &dyn Fn(&Expr) -> bool) -> bool {
    fn deep(e: &Expr, wanted: &dyn Fn(&Expr) -> bool) -> bool {
        wanted(e) || children(&e.kind).into_iter().any(|c| deep(c, wanted)) || places(&e.kind).into_iter().any(|p| indices(p).any(|i| deep(i, wanted)))
    }
    body.iter().any(|stmt| match stmt {
        Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(e, _) => deep(e, wanted),
        Stmt::StorePlace(p, _, e) | Stmt::Replace(_, p, e) => indices(p).any(|i| deep(i, wanted)) || deep(e, wanted),
        Stmt::If(c, a, b) => deep(c, wanted) || any_expr(a, wanted) || any_expr(b, wanted),
        Stmt::While(c, b) => deep(c, wanted) || any_expr(b, wanted),
        Stmt::Match { value, arms } | Stmt::MatchRef { value, arms, .. } => deep(value, wanted) || arms.iter().any(|a| any_expr(&a.body, wanted)),
        Stmt::Expire(_) | Stmt::Break | Stmt::Continue | Stmt::Trap => false,
    })
}

/// A call that may pause, or a pause: where a task can stop.
pub(super) fn site(expr: &Expr, pausing: &[bool]) -> bool {
    match &expr.kind { Value::Call(f, _) => pausing[*f as usize], Value::Pause(_) => true, _ => false }
}
fn has(expr: &Expr, pausing: &[bool]) -> bool {
    site(expr, pausing) || children(&expr.kind).into_iter().any(|e| has(e, pausing))
        || places(&expr.kind).into_iter().any(|p| indices(p).any(|e| has(e, pausing)))
}
/// The sites in a statement, in the order they are emitted.
pub(super) fn count(stmt: &Stmt, pausing: &[bool]) -> u32 {
    fn expr(e: &Expr, pausing: &[bool]) -> u32 {
        u32::from(site(e, pausing)) + children(&e.kind).into_iter().map(|e| expr(e, pausing)).sum::<u32>()
            + places(&e.kind).into_iter().flat_map(indices).map(|e| expr(e, pausing)).sum::<u32>()
    }
    let block = |body: &[Stmt]| body.iter().map(|s| count(s, pausing)).sum::<u32>();
    match stmt {
        Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(e, _) => expr(e, pausing),
        Stmt::StorePlace(p, _, e) | Stmt::Replace(_, p, e) => indices(p).map(|i| expr(i, pausing)).sum::<u32>() + expr(e, pausing),
        Stmt::If(c, a, b) => expr(c, pausing) + block(a) + block(b),
        Stmt::While(c, body) => expr(c, pausing) + block(body),
        Stmt::Match { value, arms } | Stmt::MatchRef { value, arms, .. } => expr(value, pausing) + arms.iter().map(|a| block(&a.body)).sum::<u32>(),
        Stmt::Expire(_) | Stmt::Break | Stmt::Continue | Stmt::Trap => 0,
    }
}

/// Which functions may pause, from the calls they make; then the rewrite of each that may.
pub(super) fn lower(program: &mut Program) -> Result<()> {
    let n = program.functions.len();
    let mut uses = false;
    let mut entries = Vec::new();
    fn scan(e: &Expr, direct: &mut bool, calls: &mut Vec<u32>, uses: &mut bool, entries: &mut Vec<Entry>) {
        match &e.kind {
            Value::Pause(_) => { *direct = true; *uses = true; }
            Value::Call(f, _) => calls.push(*f),
            Value::TaskStep(..) | Value::TaskFinish(..) | Value::TaskStatus(_) | Value::TaskWord | Value::InTask => *uses = true,
            Value::TaskSpawn { entry, .. } => {
                *uses = true;
                if let Value::Call(f, args) = &entry.kind && let [input] = args.as_slice() {
                    let entry = Entry { function: *f, input: input.ty };
                    if !entries.contains(&entry) { entries.push(entry); }
                }
            }
            _ => {}
        }
        for child in children(&e.kind) { scan(child, direct, calls, uses, entries); }
        for place in places(&e.kind) { for index in indices(place) { scan(index, direct, calls, uses, entries); } }
    }
    fn walk(body: &[Stmt], f: &mut dyn FnMut(&Expr)) {
        for stmt in body {
            match stmt {
                Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(e, _) => f(e),
                Stmt::StorePlace(p, _, e) | Stmt::Replace(_, p, e) => { for i in indices(p) { f(i); } f(e); }
                Stmt::If(c, a, b) => { f(c); walk(a, f); walk(b, f); }
                Stmt::While(c, body) => { f(c); walk(body, f); }
                Stmt::Match { value, arms } | Stmt::MatchRef { value, arms, .. } => { f(value); for a in arms { walk(&a.body, f); } }
                Stmt::Expire(_) | Stmt::Break | Stmt::Continue | Stmt::Trap => {}
            }
        }
    }
    let mut direct = vec![false; n];
    let mut calls = vec![Vec::new(); n];
    for (i, function) in program.functions.iter().enumerate() {
        walk(&function.body, &mut |e| scan(e, &mut direct[i], &mut calls[i], &mut uses, &mut entries));
    }
    let mut pausing = direct;
    loop {
        let mut changed = false;
        for i in 0..n {
            if !pausing[i] && calls[i].iter().any(|g| pausing[*g as usize]) { pausing[i] = true; changed = true; }
        }
        if !changed { break; }
    }
    if pausing[program.main as usize] { return Err(refuse("`main` may pause, but nothing would resume it: run pausing code in a task, `Task.spawn`")); }
    for (i, function) in program.functions.iter_mut().enumerate() {
        if !pausing[i] { continue; }
        let body = std::mem::take(&mut function.body);
        let mut flatten = Flatten { pausing: &pausing, layouts: &program.layouts, locals: &mut function.locals, prelude: Vec::new() };
        function.body = flatten.block(body)?;
    }
    program.tasks = Tasks { pausing, entries, used: uses };
    Ok(())
}

/// The same place again, once its indices are locals (or numbers).
fn duplicate(place: &Place) -> Place {
    let projections = place.projections.iter().map(|p| match p {
        Projection::Field { slot, byte_offset } => Projection::Field { slot: *slot, byte_offset: *byte_offset },
        Projection::Index { value, length, slots, stride } => Projection::Index { value: match &value.kind {
            Value::ReadPlace(read) => Expr { ty: value.ty, kind: Value::ReadPlace(duplicate(read)) },
            Value::U64(n) => Expr { ty: value.ty, kind: Value::U64(*n) },
            _ => unreachable!("an index is a local by now"),
        }, length: *length, slots: *slots, stride: *stride },
    }).collect();
    Place { root: place.root, root_ty: place.root_ty, ty: place.ty, pointer: place.pointer, projections }
}

struct Flatten<'a> {
    pausing: &'a [bool],
    layouts: &'a Layouts,
    locals: &'a mut Vec<Ty>,
    /// Statements that go before the one being rewritten.
    prelude: Vec<Stmt>,
}
impl Flatten<'_> {
    fn has(&self, e: &Expr) -> bool { has(e, self.pausing) }
    /// The value is evaluated now, by a statement of its own, and read from a local where it was.
    fn spill(&mut self, e: &mut Expr) {
        if matches!(e.kind, Value::StaticStr(_) | Value::Unit | Value::U64(_) | Value::Bool(_)) { return; }
        let ty = e.ty;
        let taken = std::mem::replace(e, Expr { ty: Ty::Unit, kind: Value::Unit });
        if self.layouts.flat(ty).is_empty() {
            self.prelude.push(Stmt::Eval(taken));
            *e = Expr { ty, kind: Value::Unit };
            return;
        }
        let start = self.locals.len() as u32;
        self.locals.extend(self.layouts.flat(ty));
        self.prelude.push(Stmt::Set(start, taken));
        *e = Expr { ty, kind: Value::ReadPlace(Place { root: start, root_ty: ty, ty, pointer: false, projections: Vec::new() }) };
    }
    /// After this, `e` holds no site: each went before it, as a statement of its own.
    fn expr(&mut self, e: &mut Expr) -> Result<()> {
        if !self.has(e) { return Ok(()); }
        if places(&e.kind).into_iter().any(|p| indices(p).any(|i| self.has(i))) {
            return Err(refuse("a call that may pause cannot be the index of a place yet: bind its result with `let` first"));
        }
        if matches!(e.kind, Value::TaskSpawn { .. } | Value::TaskStep(..) | Value::TaskFinish(..)) {
            return Err(refuse("a call that may pause cannot be an argument of a task operation yet: bind its result with `let` first"));
        }
        // `a && b`, `a || b` with a site in `b`: `b` runs only when `a` says so, as an `if`
        // (adversarial review round 26: it ran whatever `a` was).
        if let Value::Binary(op @ (Binary::And | Binary::Or), left, right) = &mut e.kind && self.has(right) {
            let and = matches!(op, Binary::And);
            self.expr(left)?;
            let start = self.locals.len() as u32;
            self.locals.push(Ty::Bool);
            let taken = std::mem::replace(left.as_mut(), Expr { ty: Ty::Bool, kind: Value::Unit });
            self.prelude.push(Stmt::Set(start, taken));
            let read = || Expr { ty: Ty::Bool, kind: Value::ReadPlace(Place { root: start, root_ty: Ty::Bool, ty: Ty::Bool, pointer: false, projections: Vec::new() }) };
            let outer = std::mem::take(&mut self.prelude);
            self.expr(right)?;
            let value = std::mem::replace(right.as_mut(), Expr { ty: Ty::Bool, kind: Value::Unit });
            let mut inner = std::mem::replace(&mut self.prelude, outer);
            inner.push(Stmt::Set(start, value));
            let condition = if and { read() } else { Expr { ty: Ty::Bool, kind: Value::Not(Box::new(read())) } };
            self.prelude.push(Stmt::If(condition, inner, Vec::new()));
            *e = read();
            return Ok(());
        }
        let site = site(e, self.pausing);
        let flags: Vec<bool> = children(&e.kind).into_iter().map(|c| self.has(c)).collect();
        let last = flags.iter().rposition(|f| *f);
        for (i, child) in children_mut(&mut e.kind).into_iter().enumerate() {
            // A site's arguments, and whatever is evaluated before a site, are evaluated first.
            if site || last.is_some_and(|last| i < last) { self.expr(child)?; self.spill(child); }
            else if last == Some(i) { self.expr(child)?; }
        }
        if site { self.spill(e); }
        Ok(())
    }
    fn block(&mut self, body: Vec<Stmt>) -> Result<Vec<Stmt>> {
        let mut out = Vec::new();
        for stmt in body {
            if count(&stmt, self.pausing) == 0 { out.push(stmt); continue; }
            let stmt = match stmt {
                Stmt::Set(local, mut e) => { self.expr(&mut e)?; Some(Stmt::Set(local, e)) }
                // A site statement stays as it is: its arguments are made locals above.
                Stmt::Eval(mut e) => { self.expr(&mut e)?; if matches!(e.kind, Value::Unit) { None } else { Some(Stmt::Eval(e)) } }
                Stmt::Return(mut e, expired) => { self.expr(&mut e)?; Some(Stmt::Return(e, expired)) }
                Stmt::Replace(slot, place, mut e) => {
                    if indices(&place).any(|i| self.has(i)) { return Err(refuse("a call that may pause cannot be the index of a place yet: bind its result with `let` first")); }
                    self.expr(&mut e)?; Some(Stmt::Replace(slot, place, e))
                }
                // The place's indices are evaluated first, as where it is found comes before the
                // value (emit.rs `locate`); then the value, into a local (adversarial review round 25:
                // the value went first, so a value that changed an index stored to another cell).
                // A compound assignment reads the old value next, before the value (round 26):
                // `x op= v` is `x = old op v` with `old` read first.
                Stmt::StorePlace(mut place, op, mut e) => {
                    if indices(&place).any(|i| self.has(i)) { return Err(refuse("a call that may pause cannot be the index of a place yet: bind its result with `let` first")); }
                    for projection in &mut place.projections { if let Projection::Index { value, .. } = projection { self.spill(value); } }
                    let old = op.map(|_| { let mut old = Expr { ty: place.ty, kind: Value::ReadPlace(duplicate(&place)) }; self.spill(&mut old); old });
                    self.expr(&mut e)?; self.spill(&mut e);
                    match (op, old) {
                        (Some(op), Some(old)) => Some(Stmt::StorePlace(place, None, Expr { ty: e.ty, kind: Value::Binary(op, Box::new(old), Box::new(e)) })),
                        _ => Some(Stmt::StorePlace(place, op, e)),
                    }
                }
                // A condition or scrutinee is a local, so going back in takes the branch it took.
                Stmt::If(mut c, a, b) => {
                    self.expr(&mut c)?; self.spill(&mut c);
                    let prelude = std::mem::take(&mut self.prelude);
                    let (a, b) = (self.block(a)?, self.block(b)?);
                    self.prelude = prelude;
                    Some(Stmt::If(c, a, b))
                }
                // `while c: body` is `loop: t = c; if !t: break; body`.
                Stmt::While(mut c, body) => {
                    let outer = std::mem::take(&mut self.prelude);
                    self.expr(&mut c)?; self.spill(&mut c);
                    let mut looped = std::mem::replace(&mut self.prelude, outer);
                    looped.push(Stmt::If(Expr { ty: Ty::Bool, kind: Value::Not(Box::new(c)) }, vec![Stmt::Break], Vec::new()));
                    looped.extend(self.block(body)?);
                    Some(Stmt::While(Expr { ty: Ty::Bool, kind: Value::Bool(true) }, looped))
                }
                Stmt::Match { mut value, arms } => {
                    self.expr(&mut value)?; self.spill(&mut value);
                    let prelude = std::mem::take(&mut self.prelude);
                    let arms = arms.into_iter().map(|mut arm| { arm.body = self.block(std::mem::take(&mut arm.body))?; Ok(arm) }).collect::<Result<Vec<_>>>()?;
                    self.prelude = prelude;
                    Some(Stmt::Match { value, arms })
                }
                // The stored tag is read once into a local: an arm may change the variant before
                // its site (adversarial review round 26).
                Stmt::MatchRef { mut value, offsets, arms, .. } => {
                    self.expr(&mut value)?; self.spill(&mut value);
                    let prelude = std::mem::take(&mut self.prelude);
                    let arms = arms.into_iter().map(|mut arm| { arm.body = self.block(std::mem::take(&mut arm.body))?; Ok(arm) }).collect::<Result<Vec<_>>>()?;
                    self.prelude = prelude;
                    let tag = self.locals.len() as u32;
                    self.locals.push(Ty::Tag);
                    Some(Stmt::MatchRef { value, offsets, arms, tag: Some(tag) })
                }
                other @ (Stmt::Expire(_) | Stmt::Break | Stmt::Continue | Stmt::Trap) => Some(other),
            };
            out.append(&mut self.prelude);
            out.extend(stmt);
        }
        Ok(out)
    }
}

/// The locals of a function that may pause which must be saved when it pauses: those read
/// after some site before being written again, and the conditions and scrutinees of the
/// statements around a site, which going back in reads again. Everything else is either recomputed going back
/// in (the emitter's own temporaries, from the locals they were made from) or not read again.
/// Slots are the function's Wasm locals; `count` is how many the IR has.
pub(super) fn saved(body: &[Stmt], count: usize, pausing: &[bool], layouts: &Layouts) -> Vec<bool> {
    let mut live = Liveness { pausing, layouts, saved: vec![false; count], count };
    live.block(body, &vec![false; count], None);
    live.saved
}
type Set = Vec<bool>;
struct Liveness<'a> { pausing: &'a [bool], layouts: &'a Layouts, saved: Set, count: usize }
fn union(a: &mut Set, b: &Set) { for (x, y) in a.iter_mut().zip(b) { *x |= *y; } }
impl Liveness<'_> {
    /// The Wasm locals a value of `ty` rooted at `root` occupies.
    fn slots(&self, root: u32, ty: Ty) -> std::ops::Range<usize> {
        let start = (root as usize).min(self.count);
        start..(start + self.layouts.flat(ty).len()).min(self.count)
    }
    fn read_place(&self, place: &Place, set: &mut Set) {
        for slot in self.slots(place.root, place.root_ty) { set[slot] = true; }
        for index in indices(place) { self.read(index, set); }
    }
    fn read(&self, e: &Expr, set: &mut Set) {
        for place in places(&e.kind) { self.read_place(place, set); }
        for child in children(&e.kind) { self.read(child, set); }
    }
    /// Live before `body`, given what is live after it, at a `break` (`exit`) and a `continue`.
    fn block(&mut self, body: &[Stmt], out: &Set, jumps: Option<(&Set, &Set)>) -> Set {
        let mut live = out.clone();
        for stmt in body.iter().rev() { live = self.stmt(stmt, &live, jumps); }
        live
    }
    fn stmt(&mut self, stmt: &Stmt, out: &Set, jumps: Option<(&Set, &Set)>) -> Set {
        match stmt {
            Stmt::Set(local, e) => {
                let mut live = out.clone();
                for slot in self.slots(*local, e.ty) { live[slot] = false; }
                if site(e, self.pausing) { union(&mut self.saved, &live); }
                self.read(e, &mut live);
                live
            }
            Stmt::Eval(e) => {
                if site(e, self.pausing) { union(&mut self.saved, out); }
                let mut live = out.clone();
                self.read(e, &mut live);
                live
            }
            Stmt::Return(e, _) => { let mut live = vec![false; self.count]; self.read(e, &mut live); live }
            Stmt::StorePlace(place, _, e) | Stmt::Replace(_, place, e) => {
                let mut live = out.clone();
                self.read_place(place, &mut live);
                self.read(e, &mut live);
                live
            }
            Stmt::If(c, a, b) => {
                // Going back in reads the condition again to take the same branch.
                if count(stmt, self.pausing) > 0 { let mut read = vec![false; self.count]; self.read(c, &mut read); union(&mut self.saved, &read); }
                let mut live = self.block(a, out, jumps);
                union(&mut live, &self.block(b, out, jumps));
                self.read(c, &mut live);
                live
            }
            Stmt::While(c, body) => {
                // The loop's head: what the condition reads, what is live on leaving, and what the
                // body needs; to a fixed point, since the body runs again.
                let mut head = out.clone();
                self.read(c, &mut head);
                loop {
                    let mut next = out.clone();
                    self.read(c, &mut next);
                    union(&mut next, &self.block(body, &head, Some((out, &head))));
                    if next == head { break; }
                    head = next;
                }
                head
            }
            Stmt::Match { value, arms } | Stmt::MatchRef { value, arms, .. } => {
                // Going back in reads the scrutinee again to take the same arm, and the tag kept
                // for a match through a reference.
                if count(stmt, self.pausing) > 0 {
                    let mut read = vec![false; self.count]; self.read(value, &mut read);
                    if let Stmt::MatchRef { tag: Some(tag), .. } = stmt && (*tag as usize) < self.count { read[*tag as usize] = true; }
                    union(&mut self.saved, &read);
                }
                let mut live = vec![false; self.count];
                for arm in arms {
                    let mut entry = self.block(&arm.body, out, jumps);
                    if let Some(binding) = arm.binding { for slot in self.slots(binding, arm.payload) { entry[slot] = false; } }
                    union(&mut live, &entry);
                }
                self.read(value, &mut live);
                live
            }
            Stmt::Break => jumps.map_or_else(|| out.clone(), |(exit, _)| exit.clone()),
            Stmt::Continue => jumps.map_or_else(|| out.clone(), |(_, head)| head.clone()),
            Stmt::Trap => vec![false; self.count],
            Stmt::Expire(_) => out.clone(),
        }
    }
}
