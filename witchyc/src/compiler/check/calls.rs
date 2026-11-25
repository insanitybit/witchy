mod memory;
mod named;

use super::diagnostics::read_only;
use super::*;

impl Checker<'_> {
    pub(super) fn call_expression(&mut self, expr: &a::Expr, callee: &a::Expr, args: &[a::Expr], expected: Option<Ty>) -> Result<Expr> {
        let (t, kind) = match &callee.kind {
            a::Expression::Name(name) if name == "#static_utf8_literal" => {
                let [a::Expr { kind: a::Expression::Integer(index), .. }] = args else {
                    return Err(error("static UTF-8 backing requires a generated literal index"));
                };
                let ty = self.layouts.str_borrow();
                let bytes = self.layouts.literal_bytes(*index).unwrap_or_else(|| "".into());
                (ty, Value::StaticStr(bytes))
            }
            a::Expression::Name(name) if name == "#validated_utf8_view" => {
                // This private token occurs only at the success exit of the compiler's
                // checked byte-to-text conversion template.
                let [source] = args else { return Err(error("validated UTF-8 view takes one slice")); };
                let Ty::Slice(bytes) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                let source = self.argument(source, Ty::Borrow { mutable: false, pointee: Pointee::Slice(bytes) })?;
                let ty = self.layouts.str_borrow();
                (ty, Value::Widen(Box::new(source)))
            }
            a::Expression::Name(name) if name == "discard" && !self.scope.contains_key(name) => {
                let [arg] = args else { return Err(error("discard takes one value")); };
                let value = self.expr(arg)?;
                let p = self.layouts.properties(value.ty);
                if !p.copy && !p.implicit_discard { return Err(error("`discard(value)` drops a value that holds no owner; one that holds an owner is closed through its type: `value.discard()` or `value.close()`")); }
                (Ty::Unit, Value::Discard(Box::new(value)))
            }
            // `memory.exchange(&mut a, &mut b)`: the one way to put a new owner where a `&mut` reference
            // points, since both places hold a value before and after. It needs no heap.
            a::Expression::Field(receiver, method)
                if method == "exchange" && matches!(&receiver.kind, a::Expression::Name(name) if name == "memory")
                    && !self.scope.contains_key("memory") && !self.signatures.contains_key("memory")
                    && !self.layouts.has_record("memory") => {
                let [left, right] = args else { return Err(error("memory.exchange takes two `&mut` references")); };
                if self.modelled() && self.record_proof.is_none() { return Err(error("this memory proof model does not support `memory.exchange`")); }
                let mut places = Vec::new();
                for arg in [left, right] {
                    let place = match &arg.kind {
                        a::Expression::Unary("&mut", target) => self.place(target)?,
                        a::Expression::Name(_) => self.borrowed_place(arg)?,
                        _ => return Err(error("memory.exchange takes two `&mut` references")),
                    };
                    if !place.mutable { return Err(error("memory.exchange needs two values that can change: a `var`, or what a `&mut` reference points at")); }
                    // The same authority as an assignment to the place: a field of a sealed or
                    // invariant-bearing type is written only where its type allows, and nothing is lent.
                    self.unsealed_write(&place, false)?;
                    self.access(place.identity, true)?;
                    self.available(&place)?;
                    places.push(place);
                }
                // Two fields of one value are different places when their paths part at a field
                // before either passes through an element; otherwise the two must not share a value.
                let parted = places[0].projections.iter().zip(&places[1].projections)
                    .map(|pair| match pair { (Projection::Field { ordinal: one }, Projection::Field { ordinal: other }) => Some(one != other), _ => None })
                    .find(|step| *step != Some(false)) == Some(Some(true));
                if !(places[0].identity == places[1].identity && parted) {
                    let saved = self.loans.clone();
                    let both = self.loan(places[0].identity, true).and_then(|()| self.loan(places[1].identity, true));
                    self.loans = saved;
                    both?;
                }
                let (right, left) = (places.pop().expect("two places"), places.pop().expect("two places"));
                if left.ty != right.ty { return Err(error("memory.exchange takes two values of one type")); }
                if self.layouts.lends(left.ty) { return Err(error("memory.exchange takes storable values, not values that hold a reference")); }
                (Ty::Unit, Value::Exchange(left, right))
            }
            a::Expression::Name(name) if matches!(name.as_str(), "Ok" | "Err")
                && !self.scope.contains_key(name) => {
                let ty = expected.ok_or_else(|| error("Result constructors require a concrete expected Result type"))?;
                let arms = self.layouts.result_sum(ty).ok_or_else(|| error("Result constructor requires a Result context"))?.arms.clone();
                let [payload] = args else { return Err(error("Result constructor requires one payload")); };
                let arm = u8::from(name == "Err");
                let payload = self.expect(payload, arms[arm as usize])?;
                (ty, Value::Sum { arm, payload: Box::new(payload) })
            }
            a::Expression::Name(_) | a::Expression::Field(..) if let Some(named) = self.constructor(callee) => {
                let (ty, arm) = self.built(named, expected)?;
                let payload_ty = self.layouts.sum(ty).expect("a sum").arms[arm as usize];
                if payload_ty == Ty::Unit { return Err(error("this variant holds nothing: write its name alone")); }
                // `Variant(a, b)`: the values together, by position.
                let together;
                let payload = match args {
                    [] => return Err(error("this variant holds a value")),
                    [one] => one,
                    many => {
                        together = a::Expr { kind: a::Expression::Record(many.iter().enumerate().map(|(index, value)| (index.to_string(), value.clone())).collect()), span: many[0].span.clone() };
                        &together
                    }
                };
                let payload = self.expect(payload, payload_ty)?;
                (ty, Value::Sum { arm, payload: Box::new(payload) })
            }
            a::Expression::Name(name)
                if self.layouts.has_record(name)
                    && !self.scope.contains_key(name)
                    =>
            {
                self.visible(name, &callee.span)?;
                let ty = match self.layouts.constructor(name, expected) {
                    Ok(ty) => ty,
                    // A generic record whose type is not given: what its field values are may settle it.
                    Err(refusal) => {
                        let mut visits = 0;
                        let mut fields = Vec::with_capacity(args.len());
                        for arg in args { fields.push(self.infer_hint(arg, None, 0, &mut visits)?); }
                        self.layouts.constructed(name, &fields)?.ok_or(refusal)?
                    }
                };
                // Built only as the value returned, of the result's type: the contract shows its invariant.
                let ok_payload = self.returned_payload_at.as_ref() == Some(&expr.span) && self.layouts.result_sum(self.result).is_some_and(|sum| sum.arms.first() == Some(&ty));
                self.unsealed_covered(ty, self.returning && ((expr.span == self.returned_at && ty == self.result) || ok_payload))?;
                let fields = self.layouts.get(ty).unwrap().fields.clone();
                if fields.len() != args.len() {
                    return Err(error(format!("record constructor arity mismatch: `{}` has {} field{}, and {} {} given",
                        name.split('@').next().unwrap_or(name), fields.len(), if fields.len() == 1 { "" } else { "s" }, args.len(), if args.len() == 1 { "is" } else { "are" })));
                }
                let values = args
                    .iter()
                    .zip(fields)
                    .map(|(arg, field)| self.expect(arg, field.ty))
                    .collect::<Result<Vec<_>>>()?;
                (ty, Value::Aggregate(values))
            }
            a::Expression::Name(name)
                if name != "main"
                    && !self.scope.contains_key(name)
                    =>
            {
                self.named_call(name, &callee.span, &expr.span, args, expected)?
            }
            a::Expression::Field(receiver, method)
                if method == "vacant" && matches!(&receiver.kind, a::Expression::Name(name) if name == "memory")
                    && !self.scope.contains_key("memory") && !self.signatures.contains_key("memory")
                    && !self.layouts.has_record("memory") => {
                return Err(error("`memory.vacant(value)` is a statement of its own: it ends the value and gives nothing back"));
            }
            a::Expression::Field(receiver, method)
                if matches!(&receiver.kind, a::Expression::Name(name) if name == "memory")
                    && !self.scope.contains_key("memory") && !self.signatures.contains_key("memory")
                    && !self.layouts.has_record("memory") => {
                self.memory_call(method, args, expected)?
            }
            // Slices (docs/design-slices.md): `a.slice()` of a fixed array; `len`, `at` and
            // `part` of a `&[T]`. Each checks its bounds where it runs.
            a::Expression::Field(receiver, method) if (method == "slice" || method == "slice_mut") && args.is_empty() && self.hint(receiver).is_some_and(|t| matches!(self.layouts.get(t).map(|a| &a.kind), Some(Kind::Array(..)))) => {
                // `slice_mut()` lends the array uniquely: its elements may be written through it.
                let mutable = method == "slice_mut";
                let borrowed = self.expr(&a::Expr { kind: a::Expression::Unary(if mutable { "&mut" } else { "&" }, receiver.clone()), span: receiver.span.clone() })?;
                let Value::Address(place) = borrowed.kind else { return Err(error("`slice()` is taken of a named array: bind it with `let` first")) };
                let Some(Kind::Array(element, length)) = self.layouts.get(place.ty).map(|a| a.kind.clone()) else { unreachable!("checked above") };
                let Ty::Slice(id) = self.layouts.slice_of(element)? else { unreachable!() };
                (Ty::Borrow { mutable, pointee: Pointee::Slice(id) }, Value::SliceOf(place, length))
            }
            a::Expression::Field(receiver, method) if matches!(method.as_str(), "len" | "at" | "part" | "part_proved" | "index" | "cell" | "copy_from" | "as_bytes" | "read_le64" | "#walk" | "#walk_mut" | "#counted") && matches!(self.hint(receiver).or_else(|| self.call_hint(receiver)), Some(Ty::Borrow { pointee: Pointee::Slice(_), .. })) => {
                // As for a call: a result that holds no reference (`len`, `s[i]`) ends the loan
                // reading the slice took, so `s[0] + s[1]` reads a unique slice twice.
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let slice = self.expr(receiver)?;
                let Ty::Borrow { pointee: Pointee::Slice(id), mutable } = slice.ty else { unreachable!("checked above") };
                let utf8 = self.layouts.is_str(id);
                if utf8 && !matches!(method.as_str(), "len" | "as_bytes" | "part" | "part_proved") {
                    return Err(error("use as_bytes() to access the bytes of borrowed UTF-8 text"));
                }

                let (ty, value) = match (method.as_str(), args) {
                    ("len", []) => (Ty::U64, Value::SliceLen(Box::new(slice))),
                    ("as_bytes", []) if utf8 => {
                        let Ty::Slice(bytes) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                        (Ty::Borrow { mutable: false, pointee: Pointee::Slice(bytes) }, Value::Widen(Box::new(slice)))
                    }
                    ("read_le64", [index]) if self.layouts.slice_element(id) == Ty::Unsigned(Scalar::U8) => {
                        let index = self.expect(index, Ty::U64)?;
                        (Ty::U64, Value::SliceReadLe64(Box::new(slice), Box::new(index)))
                    }
                    ("read_le64", [_]) => return Err(error("read_le64() requires a byte slice")),
                    ("as_bytes", []) => return Err(error("as_bytes() requires borrowed UTF-8 text")),
                    // `s[i]` (written `s.index(i)` by the rewrite of `[]`): a copy; past the end traps.
                    ("index", [index]) => {
                        let element = self.layouts.slice_element(id);
                        if !self.layouts.properties(element).copy { return Err(error("`s[i]` copies an element: for one that is not Copy, use `s.at(i)`, which lends it")); }
                        let index = self.expect(index, Ty::U64)?;
                        (element, Value::SliceIndex(Box::new(slice), Box::new(index), true))
                    }
                    // `s[i]` in `for i in a..s.len()` (iteration.rs `COUNTED`): below the length
                    // read once, which a slice keeps, so no test.
                    (crate::compiler::iteration::COUNTED, [index]) if !mutable && self.layouts.properties(self.layouts.slice_element(id)).copy => {
                        let index = self.expect(index, Ty::U64)?;
                        (self.layouts.slice_element(id), Value::SliceIndex(Box::new(slice), Box::new(index), false))
                    }
                    // `Result<&T, U64>`, as a Vec's `at`: `for item in slice` walks it.
                    ("at", [index]) => {
                        let index = self.expect(index, Ty::U64)?;
                        let element = self.layouts.slice_element(id);
                        let pointee = Pointee::from_ty(element).ok_or_else(|| error("a slice's element is storable"))?;
                        let result = self.layouts.result([Ty::Borrow { mutable, pointee }, Ty::U64])?;
                        (result, Value::SliceAt(Box::new(slice), Box::new(index), crate::compiler::logical_body::Reach::Result))
                    }
                    // `s.cell(i)`: a reference to element `i`, shared or unique as the slice is;
                    // past the end traps. `s[i] = v` is written with it.
                    // The element of a `for` over a shared slice (iteration.rs `Way::Slice`), at a
                    // counter below the length read once: no test.
                    (crate::compiler::iteration::WALK_MUT, [index]) if mutable => {
                        let index = self.expect(index, Ty::U64)?;
                        let pointee = Pointee::from_ty(self.layouts.slice_element(id)).ok_or_else(|| error("a slice's element is storable"))?;
                        (Ty::Borrow { mutable: true, pointee }, Value::SliceAt(Box::new(slice), Box::new(index), crate::compiler::logical_body::Reach::Walk))
                    }
                    (crate::compiler::iteration::WALK, [index]) if !mutable => {
                        let index = self.expect(index, Ty::U64)?;
                        let pointee = Pointee::from_ty(self.layouts.slice_element(id)).ok_or_else(|| error("a slice's element is storable"))?;
                        (Ty::Borrow { mutable: false, pointee }, Value::SliceAt(Box::new(slice), Box::new(index), crate::compiler::logical_body::Reach::Walk))
                    }
                    ("cell", [index]) => {
                        let index = self.expect(index, Ty::U64)?;
                        let pointee = Pointee::from_ty(self.layouts.slice_element(id)).ok_or_else(|| error("a slice's element is storable"))?;
                        (Ty::Borrow { mutable, pointee }, Value::SliceAt(Box::new(slice), Box::new(index), crate::compiler::logical_body::Reach::Trap))
                    }
                    ("part", [from, to]) => {
                        let from = self.expect(from, Ty::U64)?;
                        let to = self.expect(to, Ty::U64)?;
                        (slice.ty, Value::SlicePart(Box::new(slice), Box::new(from), Box::new(to)))
                    }
                    ("part_proved", [from, to]) => {
                        let from = self.expect(from, Ty::U64)?;
                        let to = self.expect(to, Ty::U64)?;
                        (slice.ty, Value::SliceWithin(Box::new(slice), Box::new(from), Box::new(to)))
                    }
                    // `target.copy_from(source)`: the source's elements over the target's, in one
                    // loop of whole words, after one test that the lengths are equal (else a
                    // trap): what `for i in 0..n: target[i] = source[i]` does with a test and a
                    // write an element. The target is unique and stays lent while the source
                    // is read, so the two are never the same elements. An element is one plain
                    // number, so any bytes copied are a value of it.
                    ("copy_from", [source]) => {
                        if !mutable { return Err(error("`copy_from` writes the slice: it takes a `&mut [T]`")); }
                        let element = self.layouts.slice_element(id);
                        if !matches!(element, Ty::Unsigned(_) | Ty::I64 | Ty::F64) { return Err(error("`copy_from` copies numbers (U8, U16, U32, U64, I64, F64): write other elements one at a time")); }
                        let source = self.expect(source, Ty::Borrow { mutable: false, pointee: Pointee::Slice(id) })?;
                        (Ty::Unit, Value::SliceCopy(Box::new(slice), Box::new(source)))
                    }
                    _ => return Err(error("a slice has `len()`, `at(index)`, `cell(index)`, `s[index]`, `part(from, to)` and, a unique one, `copy_from(source)`")),
                };
                self.keep(saved, mark, ty);
                (ty, value)
            }
            a::Expression::Field(receiver, method)
                if matches!(&receiver.kind, a::Expression::Name(n) if n == "U64")
                && !self.scope.contains_key("U64") && Bits::word_method(method).is_some() => {
                let (op, arity) = Bits::word_method(method).unwrap();
                if args.len() != arity { return Err(error("wrong number of word operands")); }
                let (input, output) = self.word_method_types(op)?;
                let values = args.iter().map(|arg| self.expect(arg, input)).collect::<Result<Vec<_>>>()?;
                (output, Value::Bits(op, values))
            }
            // `I64.from_bits(u)` is the I64 with the 64 bits of a U64, and `I64.bits(i)` those bits
            // again. Neither traps; the checked conversions are written with them in the library.
            a::Expression::Field(receiver, method) if matches!(&receiver.kind, a::Expression::Name(name) if name == "I64") && !self.scope.contains_key("I64")
                && matches!(method.as_str(), "from_bits" | "bits") => {
                let [arg] = args else { return Err(error("an I64 conversion takes one value")); };
                if method == "from_bits" { let value = self.expect(arg, Ty::U64)?; self.signed(Signed::FromBits, vec![value])? }
                else { let value = self.expect(arg, Ty::I64)?; self.signed(Signed::Bits, vec![value])? }
            }
            // `F64.from_u64(n)`, `F64.sqrt(x)` and the rest (docs/wasm-f64.md): conversions are
            // written out, as for I64; only `to_i64` traps (NaN, or outside I64).
            a::Expression::Field(receiver, method) if matches!(&receiver.kind, a::Expression::Name(name) if name == "F64") && !self.scope.contains_key("F64")
                && matches!(method.as_str(), "from_bits" | "bits" | "from_u64" | "from_i64" | "to_i64" | "sqrt" | "abs" | "floor") => {
                use crate::compiler::logical_body::Float as F;
                let [arg] = args else { return Err(error("an F64 conversion or function takes one value")); };
                let (op, input) = match method.as_str() {
                    "from_bits" => (F::FromBits, Ty::U64), "from_u64" => (F::FromU64, Ty::U64), "from_i64" => (F::FromI64, Ty::I64),
                    "bits" => (F::Bits, Ty::F64), "to_i64" => (F::ToI64, Ty::F64), "sqrt" => (F::Sqrt, Ty::F64), "abs" => (F::Abs, Ty::F64), _ => (F::Floor, Ty::F64),
                };
                let value = self.expect(arg, input)?;
                self.float(op, vec![value])?
            }
            // `U8.truncate(value)` keeps the low bits: value modulo 2^width. Total, no trap.
            a::Expression::Field(receiver, method) if method == "truncate" => {
                let a::Expression::Name(name) = &receiver.kind else {
                    return Err(error("expected scalar type in truncating conversion"));
                };
                if self.scope.contains_key(name) {
                    return Err(error("shadowed scalar conversion"));
                }
                let scalar = Scalar::named(name).filter(|s| s.narrow_index().is_some())
                    .ok_or_else(|| error("truncating conversion targets U8, U16 or U32"))?;
                let [arg] = args else {
                    return Err(error("truncating conversion takes one U64"));
                };
                let value = self.expect(arg, Ty::U64)?;
                let modulus = Expr { ty: Ty::U64, kind: Value::U64(scalar.layout().maximum() + 1) };
                let low = Expr { ty: Ty::U64, kind: Value::Binary(Binary::Rem, Box::new(value), Box::new(modulus)) };
                (Ty::Unsigned(scalar), Value::Widen(Box::new(low)))
            }
            a::Expression::Field(receiver, method) if method == "to_u64" => {
                let a::Expression::Name(name) = &receiver.kind else {
                    return Err(error("expected scalar type in widening conversion"));
                };
                if self.scope.contains_key(name) {
                    return Err(error("shadowed scalar conversion"));
                }
                // `F64.to_u64(x)`: there is none; say the two steps there are.
                if name == "F64" {
                    return Err(error("`F64.to_u64` does not exist: `F64.to_i64(x)` truncates toward zero (and traps on NaN or outside I64), then `.to_u64()` gives a Result, Err when it is negative"));
                }
                let scalar = Scalar::named(name)
                    .ok_or_else(|| error(format!("`{name}.to_u64` widens an unsigned type (U8, U16, U32); `{name}` is not one")))?;
                let [arg] = args else {
                    return Err(error("widening conversion takes one scalar"));
                };
                (
                    Ty::U64,
                    Value::Widen(Box::new(self.expect(arg, Ty::Unsigned(scalar))?)),
                )
            }
            // `input.next_byte()`: the next byte of the run's input as a number, or 256 when it has ended.
            // `input.next_run()`: up to seven bytes, lowest first, their count in the top byte (0: ended);
            // a run stops after a newline or a byte of 128 or more.
            a::Expression::Field(receiver, method) if (method == "next_byte" || method == "next_run") && args.is_empty()
                && matches!(&receiver.kind, a::Expression::Name(name) if self.scope.get(name).is_some_and(|local|
                    matches!(local.ty, Ty::Input | Ty::Console | Ty::Borrow { pointee: Pointee::Input | Pointee::Console, .. }))) => {
                if self.scope.get(match &receiver.kind { a::Expression::Name(name) => name, _ => unreachable!() }).is_some_and(|local| matches!(local.ty, Ty::Console | Ty::Borrow { pointee: Pointee::Console, .. })) {
                    return Err(error(format!("{method} requires available Console<Read> authority")));
                }
                let place = if self.scope.get(match &receiver.kind { a::Expression::Name(name) => name, _ => unreachable!() }).is_some_and(|local| matches!(local.ty, Ty::Borrow { .. })) {
                    self.borrowed_place(receiver)?
                } else { self.place(receiver)? };
                self.available(&place)?;
                let saved = self.loans.clone();
                let lent = self.loan(place.identity, false);
                self.loans = saved;
                lent?;
                (Ty::U64, Value::Read(place, method == "next_run"))
            }
            a::Expression::Field(receiver, method) if method == "print_int" || method == "print_byte" || method == "print_word" => {
                // One number and a newline, one byte as it is, or eight bytes of a word. Library code
                // writes text through these as values, so the host never reads guest memory.
                let wanted = if method == "print_byte" { Ty::Unsigned(Scalar::U8) } else { Ty::U64 };
                let place = if let a::Expression::Name(name) = &receiver.kind {
                    if self.scope.get(name).is_some_and(|local| matches!(local.ty, Ty::Borrow { .. })) {
                        self.borrowed_place(receiver)?
                    } else { self.place(receiver)? }
                } else { self.place(receiver)? };
                if place.ty != Ty::Console { return Err(error(format!("{method} requires available Console<Write> authority"))); }
                self.available(&place)?;
                let [arg] = args else { return Err(error(match method.as_str() { "print_byte" => "print_byte takes one U8", "print_word" => "print_word takes one U64: eight bytes, the lowest first", _ => "print_int takes one U64" })); };
                let saved = self.loans.clone();
                self.loan(place.identity, false)?;
                let value = self.expect(arg, wanted);
                self.loans = saved;
                (Ty::Unit, Value::Print(place, Box::new(value?), method == "print_word"))
            }
            // `Task.spawn(&allocator, bytes, work, input)`: a task that will run the function
            // `work(input)` on a stack of `bytes` (library/wasm-tasks.wy). The input is moved
            // into the task, so it refers to nothing outside itself: the input holds no
            // references. `Err(input)` when there is no memory. A named function, not a
            // closure: a closure's input would be a type argument of its call trait, which a
            // capability (a socket a task serves) never is. The call is checked as written,
            // `work(input)`; it runs at the task's first step, so it is no pause of this function.
            a::Expression::Field(receiver, method) if method == "spawn" && matches!(&receiver.kind, a::Expression::Name(name) if name == "Task") && !self.scope.contains_key("Task") => {
                if self.contracted { return Err(error("a function with a contract cannot run, make or pause a task")); }
                // `Task.spawn(&allocator, bytes, closure)`: work that takes over what it needs
                // (`fn() -> (): serve(move socket)`). Its function is the closure record's own
                // `call_once`, a named non-generic function whose input is the record.
                if let [allocator, bytes, work] = args {
                    self.layouts.enable_heap();
                    let saved = self.loans.clone();
                    let allocator = self.argument(allocator, Ty::Borrow { mutable: false, pointee: Pointee::Allocator });
                    self.loans = saved;
                    let allocator = allocator?;
                    let bytes = self.expect(bytes, Ty::U64)?;
                    let mark = self.sites.len();
                    let call = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(work.clone()), "start_task".into()), span: work.span.clone() }), vec![]), span: expr.span.clone() };
                    let entry = self.expr(&call)?;
                    self.sites.truncate(mark);
                    let Value::Call(ref instance) = entry.kind else { return Err(error("Task.spawn with two arguments runs a closure that takes over what it needs: `fn() -> (): serve(move socket)`")) };
                    let [given] = instance.args.as_slice() else { return Err(error("Task.spawn runs a closure with no arguments")) };
                    let (name, definition) = self.signatures.iter().find(|(_, sig)| sig.definition == instance.definition).ok_or_else(|| error("Task.spawn runs a closure that takes over what it needs"))?;
                    // A closure's own `call_once`, or, in generic code given a `W: WitchyOnce0<()>`,
                    // the trait's, which each instance of that code resolves to the closure's own.
                    let trait_method = name.split('@').next() == Some("WitchyOnce0.start_task");
                    if !trait_method && (!definition.generics.is_empty() || definition.requires) { return Err(error("Task.spawn runs a closure's own `call_once`")); }
                    // The work gives nothing back: a closure's own `call_once` returns `()`, or a
                    // type parameter's bound is `WitchyOnce0<()>`.
                    let gives_nothing = match self.layouts.nominal(given.ty) {
                        Some(record) if record.starts_with("Closure#") => self.signatures.get(&format!("{record}.call_once")).is_some_and(|sig| sig.result == Ty::Unit),
                        _ => match given.ty {
                            Ty::Param { index, .. } => self.signatures.values().find(|sig| sig.definition == self.definition)
                                .and_then(|sig| sig.bounds.get(index as usize)).is_some_and(|bounds| bounds.iter().any(|b| b == "WitchyOnce0<()>")),
                            _ => false,
                        },
                    };
                    if !gives_nothing { return Err(error("a task's work gives nothing back: what it makes, it hands on through what it took over")); }
                    let input_ty = given.ty;
                    if self.layouts.lends(input_ty) || matches!(input_ty, Ty::Borrow { .. }) {
                        return Err(error("a task's input moves into the task, so it holds no references"));
                    }
                    let ty = self.layouts.result([Ty::Host(crate::compiler::host::Host::Task), input_ty])?;
                    return Ok(Expr { ty, kind: Value::TaskSpawn { allocator: Box::new(allocator), bytes: Box::new(bytes), entry: Box::new(entry) } });
                }
                let [allocator, bytes, work, input] = args else { return Err(error("Task.spawn takes an allocator, a stack size in bytes, a function and its input")); };
                // The name means what a call by it means here: this file's own function first.
                if !matches!(&work.kind, a::Expression::Name(name) if !self.scope.contains_key(name) && self.signatures.get(&self.function_named(name)).is_some_and(|sig| sig.generics.is_empty())) {
                    return Err(error("Task.spawn runs a function named here, one that is not generic: `Task.spawn(&allocator, 4096, serve, move socket)`"));
                }
                // Its input arrives at the task's first step, where no caller's facts can show
                // a `requires`; a body proved assuming one would run unchecked (review round 30).
                if matches!(&work.kind, a::Expression::Name(name) if self.signatures.get(&self.function_named(name)).is_some_and(|sig| sig.requires)) {
                    return Err(error("a task's function has no `requires`: nothing shows it when the task starts; check the input in the function instead"));
                }
                self.layouts.enable_heap();
                let saved = self.loans.clone();
                let allocator = self.argument(allocator, Ty::Borrow { mutable: false, pointee: Pointee::Allocator });
                self.loans = saved;
                let allocator = allocator?;
                let bytes = self.expect(bytes, Ty::U64)?;
                let mark = self.sites.len();
                let call = a::Expr { kind: a::Expression::Call(Box::new(work.clone()), vec![input.clone()]), span: expr.span.clone() };
                let entry = self.expr(&call)?;
                self.sites.truncate(mark);
                let Value::Call(ref instance) = entry.kind else { return Err(error("Task.spawn runs a function of one parameter")) };
                let [given] = instance.args.as_slice() else { return Err(error("Task.spawn runs a function of one parameter, its input")) };
                if entry.ty != Ty::Unit { return Err(error("a task's function returns nothing: what it makes, it hands on through its input")); }
                let input_ty = given.ty;
                if self.layouts.lends(input_ty) || matches!(input_ty, Ty::Borrow { .. }) {
                    return Err(error("a task's input moves into the task, so it holds no references"));
                }
                let ty = self.layouts.result([Ty::Host(crate::compiler::host::Host::Task), input_ty])?;
                (ty, Value::TaskSpawn { allocator: Box::new(allocator), bytes: Box::new(bytes), entry: Box::new(entry) })
            }
            // `provider.raw_sha256(data, digest)`: a public operation (host.rs `Family::Crypto`),
            // computed by the host on the slices given. No receiver, so no authority: any
            // function may call one, a `pure fn` too. Each slice is lent for the call only.
            a::Expression::Field(receiver, method) if matches!(&receiver.kind, a::Expression::Name(name) if name == "provider")
                && !self.scope.contains_key("provider") && !self.signatures.contains_key("provider") && !self.layouts.has_record("provider") => {
                let Some((op, slices)) = crate::compiler::host::HostOp::public(method) else { return Err(error("unknown public crypto operation: use `import crypto`")) };
                if args.len() != slices.len() { return Err(error("a public crypto operation takes one slice per input and output")); }
                let saved = self.loans.clone();
                let mut given = Vec::new();
                for (arg, mutable) in args.iter().zip(slices.iter()) {
                    let Ty::Slice(id) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                    match self.expect(arg, Ty::Borrow { mutable: *mutable, pointee: Pointee::Slice(id) }) {
                        Ok(slice) => given.push(slice),
                        Err(why) => { self.loans = saved; return Err(why); }
                    }
                }
                self.loans = saved;
                (Ty::U64, Value::Host(op, None, given))
            }
            // `dir.raw_open_file()` and the rest: one host operation on a directory or file
            // (host.rs). The handle's right decides which exist.
            a::Expression::Field(receiver, method) if method.starts_with("raw_") || matches!(method.as_str(), "now_ms" | "next_u64") => {
                use crate::compiler::host::{HostOp, Out};
                let by_reference = matches!(&receiver.kind, a::Expression::Name(name) if self.scope.get(name).is_some_and(|local| matches!(local.ty, Ty::Borrow { .. })));
                let place = if by_reference { self.borrowed_place(receiver)? } else { self.place(receiver)? };
                let Ty::Host(host) = place.ty else { return Err(error("no method of this name on the value's type")) };
                let Some((op, operand, out)) = HostOp::method(method, host) else {
                    return Err(error("this directory or file has no such operation: its right does not allow it"));
                };
                let wanted = match operand { crate::compiler::host::Operand::Slices(slices) => slices.len(), _ => usize::from(matches!(operand, crate::compiler::host::Operand::Given(_) | crate::compiler::host::Operand::Bytes(_) | crate::compiler::host::Operand::Words | crate::compiler::host::Operand::Handle(_))) };
                if args.len() != wanted { return Err(error("a raw host operation takes one number, slice or handle, or nothing (a signer's `raw_sign`, two slices)")); }
                let result = match out { Out::Nothing => Ty::Unit, Out::Number => Ty::U64, Out::Handle(h) => Ty::Host(h) };
                let saved = self.loans.clone();
                let (receiver_place, receiver_value) = if op.consumes() {
                    if by_reference { return Err(error("this host operation consumes the handle itself")); }
                    let moved = a::Expr { kind: a::Expression::Unary("move", receiver.clone()), span: receiver.span.clone() };
                    (None, Some(self.expect(&moved, Ty::Host(host))?))
                } else {
                    self.available(&place)?;
                    self.loan(place.identity, false)?;
                    (Some(place), None)
                };
                // Two slices (`signer.raw_sign(message, signature)`), each lent for the call.
                let mut given = Vec::new();
                if let crate::compiler::host::Operand::Slices(slices) = operand {
                    for (arg, mutable) in args.iter().zip(slices.iter()) {
                        let Ty::Slice(id) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                        match self.expect(arg, Ty::Borrow { mutable: *mutable, pointee: Pointee::Slice(id) }) {
                            Ok(slice) => given.push(slice),
                            Err(why) => { self.loans = saved; return Err(why); }
                        }
                    }
                }
                let number = match (operand, args.first()) {
                    (crate::compiler::host::Operand::Given(Scalar::U64), Some(arg)) => self.expect(arg, Ty::U64),
                    (crate::compiler::host::Operand::Given(scalar), Some(arg)) => self.expect(arg, Ty::Unsigned(scalar)).map(|e| Expr { ty: Ty::U64, kind: Value::Widen(Box::new(e)) }),
                    (crate::compiler::host::Operand::Fixed(code), _) => Ok(Expr { ty: Ty::U64, kind: Value::U64(code) }),
                    // The slice is lent for the call only: the result is a number.
                    (crate::compiler::host::Operand::Bytes(mutable), Some(arg)) => {
                        let Ty::Slice(id) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                        self.expect(arg, Ty::Borrow { mutable, pointee: Pointee::Slice(id) })
                    }
                    (crate::compiler::host::Operand::Words, Some(arg)) => {
                        let Ty::Slice(id) = self.layouts.slice_of(Ty::U64)? else { unreachable!() };
                        self.expect(arg, Ty::Borrow { mutable: true, pointee: Pointee::Slice(id) })
                    }
                    // A handle given to the host: the value is moved, as any argument taken by value.
                    (crate::compiler::host::Operand::Handle(given), Some(arg)) => {
                        let moved = if matches!(&arg.kind, a::Expression::Unary("move", _)) { arg.clone() } else { a::Expr { kind: a::Expression::Unary("move", Box::new(arg.clone())), span: arg.span.clone() } };
                        self.expect(&moved, Ty::Host(given))
                    }
                    _ => Ok(Expr { ty: Ty::U64, kind: Value::U64(0) }),
                };
                self.loans = saved;
                let operands = if matches!(operand, crate::compiler::host::Operand::Slices(_)) { given } else { vec![number?] };
                let mut operands = operands;
                if let Some(handle) = receiver_value { operands.insert(0, handle); }
                (result, Value::Host(op, receiver_place, operands))
            }
            // Every method and function that exists was resolved above. An operator on a
            // program's type calls its trait's method by name (`Add.add`), so say which.
            a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(t) if t == "Scale" && !self.scope.contains_key(t)) =>
                return Err(error("`*` of a value by a number calls `Scale.scale`: implement `Scale` for the type (`import ops`)")),
            a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(t) if matches!(t.as_str(), "Add" | "Sub" | "Mul") && !self.scope.contains_key(t)) =>
                return Err(error("`+`, `-` and `*` on a value that is not a number call `Add.add`, `Sub.sub` and `Mul.mul`: implement the trait for the type (`import ops`)")),
            a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(t) if t == "Duration" && !self.scope.contains_key(t)) =>
                return Err(error("a duration (`30s`, `250ms`) is a `Duration`, from `import time`")),
            a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(t) if t == "Before" && !self.scope.contains_key(t)) =>
                return Err(error("`<`, `<=`, `>` and `>=` on a value that is not a number call `Before.before`: implement `Before` for the type (`derive(Before)`, `import sort`)")),
            a::Expression::Field(_, method) => {
                // The types that do have a method of this name, that this file may call.
                let suffix = format!(".{method}");
                let mut owners: Vec<&str> = self.signatures.iter()
                    .filter(|(key, sig)| key.ends_with(&suffix) && !key.contains('@') && !key.contains('#') && (sig.public || sig.module == self.module))
                    .filter_map(|(key, _)| key.strip_suffix(&suffix)).filter(|owner| !owner.contains('.')).collect();
                owners.sort(); owners.dedup(); owners.truncate(3);
                // `line.as_slice().part(0, n)` on a fixed array: the call that has no type is the
                // one on the array, wherever in the chain it is, so say so there and say what an
                // array has (it blamed `part`, and named the types that have one).
                {
                    let mut call = expr;
                    for _ in 0..16 {
                        let a::Expression::Call(inner_callee, _) = &call.kind else { break };
                        let a::Expression::Field(inner_receiver, inner_method) = &inner_callee.kind else { break };
                        let array = self.hint(inner_receiver).map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other })
                            .filter(|ty| matches!(self.layouts.get(*ty).map(|a| &a.kind), Some(Kind::Array(..))));
                        if let Some(array) = array {
                            let shown = self.layouts.type_name(array).unwrap_or_else(|_| "an array".into());
                            let instead = match inner_method.as_str() {
                                "as_slice" => ": an array's slice is `.slice()`; `.as_slice()` is a Vec's",
                                "as_slice_mut" | "as_mut_slice" => ": an array's slice to write through is `.slice_mut()`",
                                _ => "",
                            };
                            return Err(Diagnostic::new("W0401", inner_callee.span.clone(), format!("a fixed array (`{shown}`) has no method `{inner_method}`{instead}. An array has `.slice()`, `.slice_mut()`, `.len()` and indexing")));
                        }
                        call = inner_receiver.as_ref();
                    }
                }
                // `v[i].eq(&k)` on a Vec<String>: `v[i]` copies, and that element is not copied, so
                // it has no type here (it said `eq` is a method of `Bool`, `Eq`, `I64`).
                if let a::Expression::Call(callee, _) = &expr.kind && let a::Expression::Field(receiver, _) = &callee.kind
                    && let a::Expression::Index(container, _) = &receiver.kind && self.hint(receiver).is_none() && self.hint(container).is_some() {
                    return Err(error(format!("`x[i]` copies the element, and this one is not copied (it holds an owner): call the method through a reference, `x.cell(i).{method}(...)`")));
                }
                if owners.is_empty() {
                    // `Type.name(...)`, or `value.name(...)`: what that type does have, near the name written.
                    if let a::Expression::Call(callee, _) = &expr.kind && let a::Expression::Field(receiver, _) = &callee.kind {
                        let owner = match &receiver.kind {
                            a::Expression::Name(owner) if !self.scope.contains_key(owner) => Some(owner.clone()),
                            _ => self.hint(receiver).map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other })
                                .and_then(|ty| self.layouts.nominal(ty).map(str::to_owned)),
                        };
                        if let Some(owner) = owner {
                            // A function of that name, not a method: `write_span(&console, s)`.
                            let shown = owner.split('@').next().unwrap_or(&owner).to_string();
                            if let Some(message) = read_only(&shown, method) { return Err(error(message)); }
                            if self.signatures.get(method.as_str()).is_some_and(|sig| sig.public || sig.module == self.module) {
                                return Err(error(format!("`{shown}` has no method `{method}`; `{method}` is a function, not a method: call it as `{method}(...)`, the value among its arguments")));
                            }
                            let near = self.near_methods(&owner, method);
                            if !near.is_empty() {
                                let listed = near.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
                                let shown = owner.split('@').next().unwrap_or(&owner);
                                return Err(error(format!("`{shown}` has no method `{method}`; it has {listed}")));
                            }
                            // Nothing near the name: still which type, and that it has no such method.
                            return Err(error(format!("`{shown}` has no method `{method}`, and none near that name; a function that is not a method is called as `name(value)`")));
                        }
                    }
                    return Err(error("no method of this name on the value's type; a function that is not a method is called as `name(value)`"));
                }
                // `text.span().len()` without `import span`: the receiver is itself a method call
                // that has no type, because its method is in a standard module not imported.
                // Say that, at that call, instead of naming the types that have a `len`.
                if let a::Expression::Call(callee, _) = &expr.kind && let a::Expression::Field(receiver, _) = &callee.kind && self.hint(receiver).is_none() {
                    let mut inner = receiver.as_ref();
                    for _ in 0..16 {
                        let a::Expression::Call(inner_callee, _) = &inner.kind else { break };
                        let a::Expression::Field(inner_receiver, inner_method) = &inner_callee.kind else { break };
                        let owner = match &inner_receiver.kind {
                            a::Expression::Name(owner) if !self.scope.contains_key(owner) => Some(owner.clone()),
                            _ => self.hint(inner_receiver).map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other })
                                .and_then(|ty| self.layouts.type_name(ty).ok()),
                        };
                        if let Some(owner) = owner {
                            let base = owner.split(['<', '@']).next().unwrap_or(&owner).to_string();
                            if let Some(module) = crate::compiler::files::module_declaring_method(&base, inner_method) {
                                return Err(Diagnostic::new("W0401", inner_callee.span.clone(), format!("the receiver's type has no method of this name: `{base}.{inner_method}` is in the standard library's `{module}`: add `import {module}`")));
                            }
                            break;
                        }
                        inner = inner_receiver.as_ref();
                    }
                }
                let listed = owners.iter().map(|o| format!("`{o}`")).collect::<Vec<_>>().join(", ");
                if let a::Expression::Call(callee, _) = &expr.kind && let a::Expression::Field(receiver, _) = &callee.kind
                    && let Some(shown) = self.hint(receiver).map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other }).and_then(|ty| self.layouts.type_name(ty).ok())
                    && let Some(message) = read_only(shown.split('@').next().unwrap_or(&shown), method) { return Err(error(message)); }
                // `Nothing.discard()`: `Name.method()` calls a type's method, and a variant is no type.
                if let a::Expression::Call(callee, _) = &expr.kind && let a::Expression::Field(receiver, _) = &callee.kind
                    && let a::Expression::Name(owner) = &receiver.kind && !self.scope.contains_key(owner) && self.layouts.variant(owner, None).is_ok() {
                    return Err(error(format!("`{owner}.{method}()` names a type's method, and `{owner}` is a variant: bind it first, `let value = {owner}`, then `value.{method}()`")));
                }
                return Err(error(format!("no method of this name on the value's type; `{method}` is a method of {listed}: call it on such a value, as `value.{method}()`")));
            }
            a::Expression::Name(_) => return Err(error("no function of this name can be called here; `main` is not callable")),
            _ => return Err(error("a call names a function or a method")),
        };
        self.finish_expression(t, kind)
    }
}
