use super::diagnostics::{moved_named, unknown_field};
use super::*;

impl Checker<'_> {
    pub(super) fn expr_inner(&mut self, expr: &a::Expr, expected: Option<Ty>) -> Result<Expr> {
        if let Some((call, brand)) = self.opening && std::ptr::eq(call, expr) { self.fresh_for = Some(brand); }
        // `brand value as name:` (parser): the value is a call of a function with a `Fresh` type
        // parameter, which gets a brand new at this opening (docs/design-brands.md).
        if let a::Expression::Call(callee, args) = &expr.kind && matches!(&callee.kind, a::Expression::Name(n) if n == crate::parser::OPENING) {
            let [inner] = args.as_slice() else { return Err(error("an opening opens one value")); };
            let brand = self.opening_brand(expr, inner);
            let (saved, before) = (self.opening.replace((inner as *const a::Expr, brand)), self.opened);
            self.opened = false;
            let value = self.expr_inner(inner, expected);
            let opened = self.opened;
            self.opening = saved; self.opened = before; self.fresh_for = None;
            let value = value?;
            if !opened { return Err(error("`brand` opens a call of a function whose type parameter is `Fresh` (as `brand roads.nodes() as nodes:`); this value makes no new numbering, so bind it with `let`")); }
            return Ok(value);
        }
        self.count()?;
        if expected.is_none_or(|ty| matches!(ty, Ty::Borrow { mutable: false, pointee: Pointee::Slice(id) } if self.layouts.is_str(id)))
            && let a::Expression::Call(callee, args) = &expr.kind
            && matches!(&callee.kind, a::Expression::Name(name) if name == "Text")
            && let [a::Expr { kind: a::Expression::Integer(index), .. }] = args.as_slice()
            && let Some(bytes) = self.layouts.literal_bytes(*index)
        {
            // Program data has no stack or heap loan to carry.
            let ty = self.layouts.str_borrow();
            return self.finish_expression(ty, Value::StaticStr(bytes));
        }

        if let Some(call) = self.celled(expr)? { return self.expr_inner(&call, expected); }
        if let Some(guarded) = self.guarded_reads(expr) { return self.expr_inner(&guarded, expected); }
        // `"${name}"` with `name` a shared reference to a Copy value (a loop's `&Text`, `&U64`): the
        // format holds the value, as a type argument cannot be a reference.
        if let a::Expression::Call(callee, arguments) = &expr.kind && matches!(&callee.kind, a::Expression::Name(n) if n.starts_with("Format#"))
            && arguments.iter().any(|argument| self.copied_through(argument)) {
            let arguments = arguments.iter().map(|argument| if self.copied_through(argument) {
                a::Expr { kind: a::Expression::Unary("*", Box::new(argument.clone())), span: argument.span.clone() }
            } else { argument.clone() }).collect();
            let call = a::Expr { kind: a::Expression::Call(callee.clone(), arguments), span: expr.span.clone() };
            return self.expr_inner(&call, expected);
        }
        if let Some(call) = self.method_call(expr)? { return self.expr_inner(&call, expected); }
        if let Some(explicit) = self.through(expr) { return self.expr_inner(&explicit, expected); }
        if let Some(inner) = self.collapsed(expr, expected) { return self.expr_inner(inner, expected); }
        if let Some(text) = expected.and_then(|t| self.text_as_str(expr, t)) { return self.expr_inner(&text, expected); }
        let (t, kind) = match &expr.kind {
            a::Expression::SourceSite(_) => return Err(error("source_site() requires a source-site parameter on a direct function call")),
            a::Expression::Unit => (Ty::Unit, Value::Unit),
            // `1.5`: the F64 with these bits.
            a::Expression::Float(bits) => self.float(crate::compiler::logical_body::Float::FromBits, vec![Expr { ty: Ty::U64, kind: Value::U64(*bits) }])?,
            // `-x` on an F64 negates it (exactly, never trapping).
            a::Expression::Unary("-", value) if expected == Some(Ty::F64) || self.floating(value) => {
                let value = self.expect(value, Ty::F64)?;
                self.float(crate::compiler::logical_body::Float::Neg, vec![value])?
            }
            a::Expression::Integer(n) if expected == Some(Ty::I64) => {
                if *n > i64::MAX as u64 { return Err(error("integer literal exceeds the range of I64")); }
                self.signed(Signed::FromBits, vec![Expr { ty: Ty::U64, kind: Value::U64(*n) }])?
            }
            // `-5` is an I64 wherever it is written; `-x` is `0 - x`, which traps for the least I64.
            a::Expression::Unary("-", value) => {
                if let a::Expression::Integer(n) = &value.kind {
                    if *n > 1 << 63 { return Err(error("integer literal exceeds the range of I64")); }
                    self.signed(Signed::FromBits, vec![Expr { ty: Ty::U64, kind: Value::U64(n.wrapping_neg()) }])?
                } else {
                    let zero = Expr { ty: Ty::I64, kind: Value::Signed(Signed::FromBits, vec![Expr { ty: Ty::U64, kind: Value::U64(0) }]) };
                    let value = self.expect(value, Ty::I64)?;
                    self.signed(Signed::Sub, vec![zero, value])?
                }
            }
            a::Expression::Integer(n) => {
                let scalar = match expected {
                    Some(Ty::Unsigned(scalar)) => scalar,
                    _ => Scalar::U64,
                };
                if *n > scalar.layout().maximum() {
                    return Err(Diagnostic::new("W0401", expr.span.clone(), format!("integer literal exceeds scalar range: the most a {:?} holds is {}", scalar, scalar.layout().maximum())));
                }
                (Ty::Unsigned(scalar), Value::U64(*n))
            }
            a::Expression::Bool(b) => (Ty::Bool, Value::Bool(*b)),
            a::Expression::Inject(tag, payload) => {
                if self.modelled() { return Err(error("a function with a contract cannot build an anonymous sum yet")); }
                let Some(ty) = expected.filter(|t| self.layouts.sum(*t).is_some_and(|sum| sum.anonymous)) else {
                    return Err(error("a `.Tag` value takes its type from where it is used: annotate the binding, or use it where an anonymous sum is expected"));
                };
                let sum = self.layouts.sum(ty).expect("an anonymous sum");
                let Some(arm) = sum.names.iter().position(|name| name == tag) else { return Err(error("the anonymous sum expected here has no such tag")); };
                let holds = sum.arms[arm];
                let payload = match payload {
                    None if holds == Ty::Unit => Expr { ty: Ty::Unit, kind: Value::Unit },
                    None => return Err(error("this tag holds a value: write it in parentheses")),
                    Some(_) if holds == Ty::Unit => return Err(error("this tag holds nothing: write its name alone")),
                    Some(payload) => self.expect(payload, holds)?,
                };
                (ty, Value::Sum { arm: arm as u8, payload: Box::new(payload) })
            }
            a::Expression::Field(receiver, variant) if matches!(&receiver.kind, a::Expression::Name(sum) if !self.scope.contains_key(sum) && self.layouts.has_sum(sum)) && self.constructor(expr).is_none() => {
                let a::Expression::Name(sum) = &receiver.kind else { unreachable!() };
                return Err(Diagnostic::new("W0401", expr.span.clone(), format!("`{sum}` has no variant `{variant}`")));
            }
            a::Expression::Name(_) | a::Expression::Field(..) if let Some(named) = self.constructor(expr) => {
                let (ty, arm) = self.built(named, expected)?;
                if self.layouts.sum(ty).expect("a sum").arms[arm as usize] != Ty::Unit { return Err(error("this variant holds a value: write it in parentheses")); }
                (ty, Value::Sum { arm, payload: Box::new(Expr { ty: Ty::Unit, kind: Value::Unit }) })
            }
            a::Expression::Name(name) if self.scope.get(name).is_some_and(|local| local.ty == Ty::Unit) =>
                (Ty::Unit, Value::Unit), // active Unit arm binder has no carrier or place
            a::Expression::Name(name) if self.scope.get(name).is_some_and(|local| matches!(local.ty, Ty::Borrow { mutable: false, .. })) => {
                // A shared reference is a value: a parameter's is lent on, a local's carries its loans.
                let local = self.scope[name];
                if local.id < self.parameters {
                    self.loan(local.id, false)?;
                    self.carried.push(local.id);
                    // A slice is two words, address and count: read whole, not as a pointer.
                    if matches!(local.ty, Ty::Borrow { pointee: Pointee::Slice(_), .. }) {
                        (local.ty, Value::ReadPlace(Place { root: local.id, identity: local.id, root_ty: local.ty, ty: local.ty, pointer: false, mutable: false, projections: Vec::new() }))
                    } else { (local.ty, Value::Address(self.borrowed_place(expr)?)) }
                } else {
                    self.inherit(local.id);
                    (local.ty, Value::ReadPlace(Place { root: local.id, identity: local.id, root_ty: local.ty, ty: local.ty,
                        pointer: false, mutable: false, projections: Vec::new() }))
                }
            }
            a::Expression::Name(name) if self.scope.get(name).is_some_and(|local| matches!(local.ty, Ty::Borrow { mutable: true, .. })) => {
                // A unique reference passed on as a value is lent, not copied: the name cannot be
                // used again while the value made from it is.
                let local = self.scope[name];
                self.loan(local.id, true)?;
                self.carried.push(local.id | UNIQUE);
                if local.id < self.parameters && !matches!(local.ty, Ty::Borrow { pointee: Pointee::Slice(_), .. }) {
                    (local.ty, Value::Address(self.borrowed_place(expr)?))
                } else {
                    self.inherit(local.id);
                    (local.ty, Value::ReadPlace(Place { root: local.id, identity: local.id, root_ty: local.ty, ty: local.ty,
                        pointer: false, mutable: false, projections: Vec::new() }))
                }
            }
            _ if Self::place_syntax(expr) => {
                // expr_inner already counted the outer node. Recursive place
                // children retain the same visit accounting as value projections.
                let place = self.nested(|this| this.place_inner(expr))?;
                self.available(&place)?;
                // A field is read beside loans of the value's other fields.
                let id = self.loan_id(&place);
                self.access(id, false)?;
                if !self.layouts.properties(place.ty).copy {
                    // Name the value in the advice when it is a variable (`match move length:`).
                    if let a::Expression::Name(name) = &expr.kind {
                        return Err(error(format!("affine values require explicit move: `{name}` holds an owner, which is not copied; hand it on with `move {name}`, or lend it with `&{name}`")));
                    }
                    return Err(error("affine values require explicit move; borrowed owners cannot be copied: hand the owner on with `move name`, or lend it with `&name`"));
                }
                // Through a reference to a view, a reference read out depends on what the
                // reference holds, which includes the view's own loans.
                if !place.pointer || self.layouts.lends(place.ty) { self.inherit(place.root); }
                // Through a parameter, what is read points where the caller's value does: the value
                // depends on a loan of the parameter, so a `return` is judged by where it came from
                // (review of `borrows`: `return Source((*view).text)` carried no loan at all, and a
                // caller trusting the clause freed what the view pointed into).
                if place.pointer && self.layouts.lends(place.ty) && place.root < self.parameters && !self.holds.contains_key(&place.root) {
                    let entry = (place.root, vec![THROUGH]);
                    let index = match self.paths.iter().position(|known| *known == entry) { Some(i) => i, None => { self.paths.push(entry); self.paths.len() - 1 } };
                    let root = FIELD_LOAN + index as u32;
                    self.lend_again(&[root]);
                    self.carried.push(root);
                }
                (place.ty, Value::ReadPlace(place))
            }
            a::Expression::Record(fields) => {
                let hints = expected.filter(|ty| self.layouts.is_anonymous(*ty)).map(|ty| self.layouts.get(ty).unwrap().fields.clone());
                let mut values: Vec<(String, Expr)> = Vec::new();
                for (label, value) in fields {
                    let hint = hints.as_ref().and_then(|fields| fields.iter().find(|field| field.name == *label)).map(|field| field.ty);
                    let value = self.expr_expected(value, hint)?;
                    values.push((label.clone(), value));
                }
                // Fields are stored in label order. Writing them in another order is fine
                // while no operand has an effect whose position could be observed.
                let ordered = values.windows(2).all(|pair| pair[0].0 < pair[1].0);
                // `[0; 4]` is as quiet as its value (it counted as an effect).
                fn quiet(value: &Expr) -> bool {
                    match &value.kind {
                        Value::ReadPlace(_) | Value::Move { .. } | Value::U64(_) | Value::Bool(_) => true,
                        Value::Repeat(inner, _) => quiet(inner),
                        _ => false,
                    }
                }
                let quiet = values.iter().all(|(_, value)| quiet(value));
                if !ordered && !quiet {
                    // The order to write, which is alphabetical, not the type's declared order.
                    let mut labels: Vec<&str> = values.iter().map(|(label, _)| label.as_str()).collect();
                    labels.sort();
                    let order = labels.join(", ");
                    return Err(error(format!("anonymous record operands with effects are evaluated in alphabetical label order ({order}): write them in that order, or bind them to names first")));
                }
                values.sort_by(|a, b| a.0.cmp(&b.0));
                let ty = self.layouts.anonymous(values.iter().map(|(label, value)| (label.clone(), value.ty)).collect())?;
                (ty, Value::Aggregate(values.into_iter().map(|(_, value)| value).collect()))
            }
            a::Expression::Unary("move", value) => {
                if let a::Expression::Field(base, field) = &value.kind {
                    let a::Expression::Name(name) = &base.kind else {
                        return Err(error("owner extraction requires a direct field of a bound owned record"));
                    };
                    let local = *self.scope.get(name).ok_or_else(|| error("unknown owner extraction binding"))?;
                    let owner = self.layouts.sole_owner_field(local.ty).ok_or_else(|| error("owner extraction requires a bound one-owner record; open a record with several owners using `let Name(a, b) = move value`"))?;
                    if owner.name != *field { return Err(error("owner extraction requires the sole non-Copy field")); }
                    self.unsealed(local.ty)?;
                    let earlier = self.moved_at.get(&local.id).copied();
                    self.flow.require_owned(local.id).map_err(|d| moved_named(d, name, earlier))?;
                    self.access(local.id, true)?;
                    let place = self.place(value)?;
                    self.flow.consume(local.id).map_err(|d| moved_named(d, name, earlier))?; // consumes WHOLE wrapper, Copy remainder discarded
                    self.moved_at.insert(local.id, if value.span.start > 0 { value.span.start } else { self.at.start });
                    return Ok(Expr { ty: place.ty, kind: Value::Extract { wrapper: local.id, place } });
                }
                let a::Expression::Name(name) = &value.kind else { return Err(error("move requires a bound name")); };
                let local = *self.scope.get(name).ok_or_else(|| error(format!("unknown move binding: no variable or parameter named `{name}` is in scope here")))?;
                // The hidden name of an `if` value (early_exit.rs writes `move` of it): a shared
                // reference is read, as any name holding one is, and keeps the loans the choice carried.
                if name.starts_with("?if") && matches!(local.ty, Ty::Borrow { mutable: false, .. }) { return self.expr(value); }
                if matches!(local.ty, Ty::Borrow { .. }) { return Err(error("cannot move a borrowed reference or owner through it")); }
                if self.layouts.properties(local.ty).copy { return self.expr(value); }
                let earlier = self.moved_at.get(&local.id).copied();
                self.flow.require_owned(local.id).map_err(|d| moved_named(d, name, earlier))?;
                self.access(local.id, true)?;
                let place = self.place(value)?;
                self.flow.consume(local.id).map_err(|d| moved_named(d, name, earlier))?;
                self.moved_at.insert(local.id, if value.span.start > 0 { value.span.start } else { self.at.start });
                self.inherit(local.id);
                (local.ty, Value::Move { slot: local.id, place })
            }
            a::Expression::Unary("!", value) => (
                Ty::Bool,
                Value::Not(Box::new(self.expect(value, Ty::Bool)?)),
            ),
            a::Expression::Unary("~", value) => {
                let ty = expected.filter(|t| matches!(t, Ty::Unsigned(_))).or(self.infer_hint(value, None, 0, &mut 0)?).unwrap_or(Ty::U64);
                let operand = self.expect(value, ty)?;
                self.bits(crate::compiler::logical_body::Bits::Not, ty, vec![operand])?
            }
            a::Expression::Binary(op, left, right) => {
                let arithmetic = matches!(*op, "+" | "-" | "*" | "/" | "%");
                let left_hint=self.infer_hint(left,None,0,&mut 0)?;
                let right_hint=self.infer_hint(right,None,0,&mut 0)?;
                let scalar = if arithmetic {
                    expected.filter(|t| matches!(t, Ty::Unsigned(_) | Ty::I64 | Ty::F64))
                } else {
                    None
                }
                .or(left_hint)
                .or(right_hint)
                .unwrap_or(Ty::U64);
                // I64 has its own operations: both sides are I64, and no proof reads the result.
                let signed = match *op { "+" => Some(Signed::Add), "-" => Some(Signed::Sub), "*" => Some(Signed::Mul), "/" => Some(Signed::Div), "%" => Some(Signed::Rem),
                    "==" => Some(Signed::Eq), "!=" => Some(Signed::Ne), "<" => Some(Signed::Lt), "<=" => Some(Signed::Le), ">" => Some(Signed::Gt), ">=" => Some(Signed::Ge), _ => None };
                // F64 likewise, without `%`.
                use crate::compiler::logical_body::Float as F;
                let floating = match *op { "+" => Some(F::Add), "-" => Some(F::Sub), "*" => Some(F::Mul), "/" => Some(F::Div),
                    "==" => Some(F::Eq), "!=" => Some(F::Ne), "<" => Some(F::Lt), "<=" => Some(F::Le), ">" => Some(F::Gt), ">=" => Some(F::Ge), _ => None };
                if matches!(*op, "&" | "|" | "^" | "<<" | ">>") {
                    use crate::compiler::logical_body::Bits as B;
                    let shift = matches!(*op, "<<" | ">>");
                    let unsigned = |t: &Ty| matches!(t, Ty::Unsigned(_));
                    let ty = expected.filter(unsigned).or(left_hint.filter(unsigned)).or(if shift { None } else { right_hint.filter(unsigned) }).unwrap_or(Ty::U64);
                    let left = self.expect(left, ty)?;
                    let right = self.expect(right, if shift { Ty::U64 } else { ty })?;
                    let op = match *op { "&" => B::And, "|" => B::Or, "^" => B::Xor, "<<" => B::Shl, _ => B::Shr };
                    let (ty, kind) = self.bits(op, ty, vec![left, right])?;
                    return Ok(Expr { ty, kind });
                }
                if scalar == Ty::F64 {
                    let Some(op) = floating else { return Err(error("F64 has + - * / and the six comparisons; `%` and bit operations are on integers")); };
                    let left = self.expect(left, Ty::F64)?;
                    let right = self.expect(right, Ty::F64)?;
                    let (ty, kind) = self.float(op, vec![left, right])?;
                    return Ok(Expr { ty, kind });
                }
                if let (Ty::I64, Some(op)) = (scalar, signed) {
                    let left = self.expect(left, Ty::I64)?;
                    let right = self.expect(right, Ty::I64)?;
                    let (ty, kind) = self.signed(op, vec![left, right])?;
                    return Ok(Expr { ty, kind });
                }
                let (op, input, result) = match *op {
                    "+" => (Binary::Add, scalar, scalar),
                    "-" => (Binary::Sub, scalar, scalar),
                    "*" => (Binary::Mul, scalar, scalar),
                    "/" => (Binary::Div, scalar, scalar),
                    "%" => (Binary::Rem, scalar, scalar),
                    "<" => (Binary::Lt, scalar, Ty::Bool),
                    "<=" => (Binary::Le, scalar, Ty::Bool),
                    ">" => (Binary::Gt, scalar, Ty::Bool),
                    ">=" => (Binary::Ge, scalar, Ty::Bool),
                    "&&" => (Binary::And, Ty::Bool, Ty::Bool),
                    "||" => (Binary::Or, Ty::Bool, Ty::Bool),
                    "==" | "!=" if matches!(scalar, Ty::Unsigned(_) | Ty::Bool) => (
                        if *op == "==" { Binary::Eq } else { Binary::Ne },
                        scalar,
                        Ty::Bool,
                    ),
                    _ => return Err(error("unsupported scalar operation")),
                };
                if !matches!(op, Binary::Eq | Binary::Ne | Binary::And | Binary::Or)
                    && !matches!(input, Ty::Unsigned(_))
                {
                    // Say what the operand is: most often a reference a `for` gave (`for c in deck`).
                    let side = if left_hint.is_some() { left } else { right };
                    let named = match &side.kind { a::Expression::Name(n) => format!("`{n}`"), _ => "this value".into() };
                    let message = match input {
                        Ty::Borrow { pointee, .. } if matches!(pointee.ty(), Ty::Unsigned(_) | Ty::I64 | Ty::F64) =>
                            format!("numeric operation requires unsigned scalar operands: {named} is a reference to a number; read the number with `*{}`", match &side.kind { a::Expression::Name(n) => n.as_str(), _ => "value" }),
                        other => match self.layouts.type_name(other) {
                            Ok(shown) => format!("numeric operation requires unsigned scalar operands: {named} is a `{shown}`"),
                            Err(_) => "numeric operation requires unsigned scalar operands".into(),
                        },
                    };
                    return Err(Diagnostic::new("W0401", side.span.clone(), message));
                }
                let left = self.expect(left, input)?;
                let skipped = self.flow.clone();
                let right = self.expect(right, input)?;
                if matches!(op, Binary::And | Binary::Or) {
                    self.flow = Flow::join(&skipped, &[(&skipped, false), (&self.flow, false)]);
                }
                (result, Value::Binary(op, Box::new(left), Box::new(right)))
            }
            a::Expression::Call(callee, args) => return self.call_expression(expr, callee, args, expected),
            a::Expression::Array(items) => {
                if items.is_empty() || items.len() > 256 {
                    return Err(error("fixed array length must be 1..256"));
                }
                let expected_element = expected
                    .and_then(|t| self.layouts.get(t))
                    .and_then(|layout| {
                        if let Kind::Array(element, len) = layout.kind {
                            if len as usize == items.len() {
                                Some(element)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .or_else(|| items.iter().find_map(|item| self.hint(item).or_else(|| self.infer_hint(item, None, 0, &mut 0).ok().flatten())));
                let expected_element = if expected.is_none() { expected_element.map(|t| self.stored_text(t)) } else { expected_element };
                let first = self.expr_expected(&items[0], expected_element)?;
                let element = first.ty;
                let mut values = vec![first];
                for item in &items[1..] {
                    values.push(self.expect(item, element)?);
                }
                (
                    self.layouts.array(element, items.len() as u64)?,
                    Value::Aggregate(values),
                )
            }
            a::Expression::Repeat(value, count) => {
                let expected_element =
                    expected
                        .and_then(|t| self.layouts.get(t))
                        .and_then(|layout| {
                            if let Kind::Array(element, len) = layout.kind {
                                if len as u64 == *count {
                                    Some(element)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        });
                let value = self.expr_expected(value, expected_element)?;
                let ty = self.layouts.array(value.ty, *count)?;
                (ty, Value::Repeat(Box::new(value), *count as u32))
            }
            a::Expression::Field(value, name) => {
                let value = self.expr(value)?;
                if !self.layouts.properties(value.ty).copy {
                    return Err(error("owned record projection requires an available bound place"));
                }
                let layout = self
                    .layouts
                    .get(value.ty)
                    .ok_or_else(|| error("field access requires a record"))?;
                if !matches!(layout.kind, Kind::Record(_)) {
                    return Err(error("field access requires a record"));
                }
                let field = layout
                    .fields
                    .iter()
                    .enumerate().find(|(_,f)| f.name == *name)
                    .ok_or_else(|| unknown_field(&layout.fields, name))?;
                (field.1.ty, Value::Project(Box::new(value), field.0 as u32))
            }
            a::Expression::Index(value, index) => {
                let value = self.expr(value)?;
                let Some(layout) = self.layouts.get(value.ty) else {
                    return Err(error("indexing requires a fixed array"));
                };
                let Kind::Array(element, len) = layout.kind else {
                    return Err(error("indexing requires a fixed array"));
                };
                let index = self.expect(index, Ty::U64)?;
                (element, Value::Index(Box::new(value), Box::new(index), len))
            }
            a::Expression::Unary("&", target) if Self::place_syntax(target) => {
                // A shared reference to a place is a value: it lends the place's root, or, when
                // the place is reached through a reference, whatever that reference borrows.
                let pointee = self.hint(target).and_then(Pointee::from_ty).ok_or_else(|| error("a reference needs a place whose type is known"))?;
                let ty = Ty::Borrow { mutable: false, pointee };
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let address = self.argument(expr, ty);
                if address.is_err() { self.loans = saved.clone(); }
                let address = address?;
                self.keep(saved, mark, ty);
                (ty, address.kind)
            }
            a::Expression::Unary("&mut", target) if Self::place_syntax(target) => {
                // A unique reference to a place, kept as a value.
                let pointee = self.hint(target).and_then(Pointee::from_ty).ok_or_else(|| error("a reference needs a place whose type is known"))?;
                let ty = Ty::Borrow { mutable: true, pointee };
                let (saved, mark) = (self.loans.clone(), self.carried.len());
                let address = self.argument(expr, ty);
                if address.is_err() { self.loans = saved.clone(); }
                let address = address?;
                if let Value::Address(place) = &address.kind { self.lasting(place, false)?; }
                self.keep(saved, mark, ty);
                (ty, address.kind)
            }
            a::Expression::Unary("*", value) if matches!(value.kind, a::Expression::Field(..)) && matches!(self.hint(value), Some(Ty::Borrow { .. })) =>
                return Err(error("a reference held in a record is used by name: bind it with `let` first, as in `let items = view.items`")),
            _ => {
                return Err(Diagnostic::new(
                    "W0401",
                    expr.span.clone(),
                    if matches!(expr.kind, a::Expression::Try(_)) { "`?` ends a whole `let` value or a whole statement, not part of a larger expression" }
                    else if matches!(&expr.kind, a::Expression::Unary("&" | "&mut", inner) if !Self::place_syntax(inner)) { "unsupported expression: a reference lends a variable, a field or an element; bind the value with `var` first and lend that" }
                    // `*v.cell(0)`, `*map.get(&k)`: a reference a call gives is read once it has a name.
                    else if matches!(&expr.kind, a::Expression::Unary("*", inner) if !matches!(inner.kind, a::Expression::Name(_))) { "`*` of what a call gives is read as a whole `let` value, `let x = *v.cell(0)`; inside a larger expression, bind it first" }
                    else { "unsupported expression: general loans, moves, memory and unsafe primitives require a future checker" },
                ));
            }
        };
        self.finish_expression(t, kind)
    }

    pub(super) fn finish_expression(&self, t: Ty, kind: Value) -> Result<Expr> {
        if !self.contracted && matches!(&kind,Value::Storage{op,..} if op.proof_admitted()) {
            self.needs_contract.set(true);
        }
        // In a function proved about a Buffer, a returned value may hold one: the proof sees
        // where it was made. Nothing else in a function with a contract holds one.
        // In a proof over numbers, a parameter that holds `&` references is read whole, by its
        // name, to give to a call: the proof knows nothing of it or of what it refers to.
        if self.contracted && self.layouts.views(t)
            && !(self.scalar_proof && matches!(&kind, Value::ReadPlace(place) if place.projections.is_empty() && !place.pointer
                && place.ty == t && place.root_ty == t && self.view_parameters.contains(&place.root))) {
            return Err(error("a function with a contract cannot use a record that holds a reference yet"));
        }
        // Nor is a reference read out of such a parameter: the proof would follow what it points
        // at, which the rule above says it knows nothing of. Its numbers are read as any record's.
        if self.contracted && self.layouts.lends(t) && !self.view_parameters.is_empty()
            && let Value::ReadPlace(place) | Value::Address(place) | Value::Move { place, .. } | Value::Extract { place, .. } | Value::SliceOf(place, _) | Value::Stored(place, _) = &kind
            && self.view_parameters.contains(&place.root) && !place.projections.is_empty() {
            return Err(error("a function with a contract cannot use a record that holds a reference yet"));
        }
        // A scalar proof follows a `&mut` held in a local as it follows a `&mut` parameter, and
        // what it was lent from is unknown from the lending on (scalar_vc's `lent`).
        if self.modelled() && self.layouts.lends_unique(t) && !(self.memory_proof && self.returning) {
            return Err(error("a function with a contract cannot hold a `&mut` reference as a value yet"));
        }
        Ok(Expr { ty: t, kind })
    }
}
