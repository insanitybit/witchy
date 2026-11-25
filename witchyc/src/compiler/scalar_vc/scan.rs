//! Structural scans for loop effects and proof coverage.
use super::*;

pub(super) fn loop_effects(condition: &Expr, body: &[Stmt]) -> (BTreeSet<u32>, bool) {
    let mut assigned = BTreeSet::new();
    let mut breaks = false;
    let mut scan = Scan::Loop {
        assigned: &mut assigned,
        breaks: &mut breaks,
    };
    scan.statements(body, true);
    scan.expression(condition);
    (assigned, breaks)
}

/// The locals that evaluating these expressions may assign, lend to change, or move.
pub(super) fn assigned_by(expressions: &[Expr]) -> BTreeSet<u32> {
    let mut assigned = BTreeSet::new();
    let mut breaks = false;
    let mut scan = Scan::Loop { assigned: &mut assigned, breaks: &mut breaks };
    for e in expressions { scan.expression(e); }
    assigned
}

pub(super) struct Coverage {
    pub(super) arithmetic: bool,
    pub(super) indexes: bool,
    pub(super) slices: bool,
    pub(super) boxes: bool,
    has_slice_reads: bool,
}

pub(super) fn coverage(body: &[Stmt], covered: &BTreeSet<usize>) -> Coverage {
    let mut result = Coverage {
        arithmetic: true,
        indexes: true,
        slices: true,
        boxes: true,
        has_slice_reads: false,
    };
    Scan::Coverage {
        covered,
        result: &mut result,
    }
    .statements(body, false);
    result.slices &= result.has_slice_reads;
    result
}

enum Scan<'a> {
    Loop {
        assigned: &'a mut BTreeSet<u32>,
        breaks: &'a mut bool,
    },
    Coverage {
        covered: &'a BTreeSet<usize>,
        result: &'a mut Coverage,
    },
}

impl Scan<'_> {
    fn assign(&mut self, local: u32) {
        if let Self::Loop { assigned, .. } = self {
            assigned.insert(local);
        }
    }

    fn site(&mut self, node: usize) {
        let Self::Coverage { covered, result } = self else {
            return;
        };
        let complete = match node & 3 {
            BOX_NODE => &mut result.boxes,
            SLICE_NODE => {
                result.has_slice_reads = true;
                &mut result.slices
            }
            INDEX_NODE => &mut result.indexes,
            _ => &mut result.arithmetic,
        };
        if *complete {
            *complete = covered.contains(&node);
        }
    }

    fn statements(&mut self, statements: &[Stmt], own_loop: bool) {
        for statement in statements {
            match statement {
                Stmt::Set(id, e) => {
                    self.assign(*id);
                    self.expression(e);
                }
                Stmt::StorePlace(place, op, e) => {
                    if matches!(op, Some(Binary::Add | Binary::Sub | Binary::Mul))
                        && matches!(place.ty, Ty::Unsigned(_))
                    {
                        self.site(statement as *const Stmt as usize);
                    }
                    self.assign(place.root);
                    self.place(place);
                    self.expression(e);
                }
                Stmt::Eval(e) | Stmt::Return(e, _, _) => self.expression(e),
                Stmt::Replace(local, place, e) => {
                    self.assign(*local);
                    self.assign(place.root);
                    self.place(place);
                    self.expression(e);
                }
                Stmt::If(condition, yes, no) => {
                    self.expression(condition);
                    self.statements(yes, own_loop);
                    self.statements(no, own_loop);
                }
                Stmt::While(condition, body, _) => {
                    self.expression(condition);
                    self.statements(body, false);
                }
                Stmt::Match { value, arms } => {
                    self.expression(value);
                    for arm in arms {
                        if let Some(binding) = arm.binding {
                            self.assign(binding);
                        }
                        self.statements(&arm.body, own_loop);
                    }
                }
                Stmt::Break if own_loop => {
                    if let Self::Loop { breaks, .. } = self {
                        **breaks = true;
                    }
                }
                // A continue returns to the head. A nested break leaves its own loop.
                Stmt::Expire(_) | Stmt::Break | Stmt::Continue | Stmt::Trap(_) => {}
            }
        }
    }

    fn index(&mut self, value: &Expr, length: u32) {
        if !matches!(value.kind, Value::U64(k) if k < length as u64) {
            self.site(value as *const Expr as usize | INDEX_NODE);
        }
    }

    fn place(&mut self, place: &Place) {
        for projection in &place.projections {
            if let Projection::Index { value, length } = projection {
                self.index(value, *length);
                self.expression(value);
            }
        }
    }

    fn expression(&mut self, e: &Expr) {
        if matches!(e.kind, Value::BoxOp(op, _, _) if op.proved()) {
            self.site(e as *const Expr as usize | BOX_NODE);
        }
        if matches!(
            e.kind,
            Value::Binary(Binary::Add | Binary::Sub | Binary::Mul, _, _)
        ) && matches!(e.ty, Ty::Unsigned(_))
        {
            self.site(e as *const Expr as usize);
        }
        match &e.kind {
            Value::StaticStr(_)
            | Value::Unit
            | Value::U64(_)
            | Value::Bool(_)
            | Value::EntryConsole
            | Value::EntryInput
            | Value::EntryAllocator
            | Value::TaskWord
            | Value::InTask
            | Value::EntryHost(..) => {}
            Value::ReadPlace(place) | Value::Move { place, .. } | Value::Address(place) => {
                self.place(place);
                if matches!(e.ty, Ty::Borrow { mutable: true, .. }) {
                    self.assign(place.root);
                }
            }
            Value::Extract { place, .. } => self.place(place),
            Value::Stored(place, inner) => {
                self.assign(place.root);
                self.place(place);
                self.expression(inner);
            }
            Value::TryAlloc {
                allocator,
                capacity,
                ..
            } => {
                self.expression(allocator);
                self.expression(capacity);
            }
            Value::AllocationKey(inner, _)
            | Value::Capacity(inner, _)
            | Value::TryRelease(inner, _)
            | Value::Discard(inner)
            | Value::ProofUtf8(inner) | Value::Widen(inner)
            | Value::Not(inner)
            | Value::Project(inner, _)
            | Value::Retag(inner, _)
            | Value::Repeat(inner, _) => self.expression(inner),
            Value::Storage {
                receiver,
                index,
                value,
                ..
            } => {
                self.expression(receiver);
                self.expression(index);
                if let Some(value) = value {
                    self.expression(value);
                }
            }
            Value::Aggregate(fields)
            | Value::Signed(_, fields)
            | Value::Float(_, fields)
            | Value::Bits(_, fields)
            | Value::BoxOp(_, _, fields) => {
                for field in fields {
                    self.expression(field);
                }
            }
            Value::Sum { payload, .. } => self.expression(payload),
            Value::Index(a, b, length) => {
                self.index(b, *length);
                self.expression(a);
                self.expression(b);
            }
            Value::SliceReadLe64(a, b) => {
                self.site(&**b as *const Expr as usize | SLICE_NODE);
                self.expression(a);
                self.expression(b);
            }
            Value::Binary(_, a, b) | Value::SliceAt(a, b, _) | Value::SliceCopy(a, b) => {
                self.expression(a);
                self.expression(b);
            }
            // A read with no test (a counted `for`) needs no goal to lose one.
            Value::SliceIndex(a, b, tested) => {
                if *tested {
                    self.site(&**b as *const Expr as usize | SLICE_NODE);
                }
                self.expression(a);
                self.expression(b);
            }
            Value::SliceOf(place, _) => {
                self.place(place);
                if matches!(e.ty, Ty::Borrow { mutable: true, .. }) {
                    self.assign(place.root);
                }
            }
            Value::SliceLen(inner) => self.expression(inner),
            Value::SliceWithin(a, b, c) => {
                self.site(&**b as *const Expr as usize | SLICE_NODE);
                self.expression(a);
                self.expression(b);
                self.expression(c);
            }
            Value::SlicePart(a, b, c) => {
                self.expression(a);
                self.expression(b);
                self.expression(c);
            }
            Value::Call(call) => {
                for argument in &call.args {
                    self.expression(argument);
                }
            }
            Value::Print(place, value, _) => {
                self.place(place);
                self.expression(value);
            }
            Value::Read(place, _) => self.place(place),
            Value::Host(_, place, operands) => {
                if let Some(place) = place {
                    self.place(place);
                }
                for operand in operands {
                    self.expression(operand);
                }
            }
            Value::Exchange(left, right) => {
                for place in [left, right] {
                    self.assign(place.root);
                    self.place(place);
                }
            }
            Value::TaskSpawn {
                allocator,
                bytes,
                entry,
            } => {
                for value in [allocator, bytes, entry] {
                    self.expression(value);
                }
            }
            Value::TaskStep(value, word) => {
                self.expression(value);
                self.expression(word);
            }
            Value::TaskFinish(value) | Value::TaskStatus(value) | Value::Pause(value) => {
                self.expression(value)
            }
        }
    }
}
