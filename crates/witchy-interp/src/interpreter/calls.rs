//! Call evaluation: resolving and invoking callables and closures, evaluating
//! argument lists, and the interpreter-special / existential / plain call
//! dispatch. Split out of interpreter.rs as an impl continuation.

use super::*;

/// An exclusive reference has the public `let` calling convention, but its
/// runtime envelope carries one writable caller place.  Keeping this separate
/// from `var` is important: `var` is a move-in/move-out value convention,
/// whereas `&mut` preserves reference identity and will eventually lower to a
/// direct place rather than this interpreter write-back representation.
fn is_exclusive_reference(param: &Param) -> bool {
    matches!(
        param.ty,
        Some(Type::Qualified(TypeQual::BorrowMut(_), _))
    )
}

fn parameter_writes_back(param: &Param) -> bool {
    param.convention == Convention::Var || is_exclusive_reference(param)
}

fn parameter_binds_mutable(param: &Param) -> bool {
    param.convention.binds_mutable() || is_exclusive_reference(param)
}

impl Interpreter {
    pub(super) fn run_callable(
        &mut self,
        mut callable: TailCallable,
        mut argvals: Vec<Value>,
    ) -> Result<CallableOutcome, Flow> {
        self.depth += 1;
        if self.depth > self.depth_limit {
            self.depth -= 1;
            return err("call stack too deep (possible infinite recursion)");
        }
        let prev_fn = self.cur_fn.clone();
        let prev_line = self.cur_line;
        let prev_tail_function = self.tail_function.take();
        let prev_tail_dynamic_chain = self.tail_dynamic_chain;
        let mut dynamic_chain = matches!(callable, TailCallable::Closure(_));
        let result = loop {
            let current_args = std::mem::take(&mut argvals);
            let (function, mut env, is_closure) = match callable {
                TailCallable::Function(function) => {
                    if function.params.len() != current_args.len() {
                        break err(format!(
                            "`{}` expects {} argument(s) but got {}",
                            function.name,
                            function.params.len(),
                            current_args.len()
                        ));
                    }
                    let mut env = Env::new();
                    let cached_names = self.param_names.get(&(Rc::as_ptr(&function) as usize)).cloned();
                    for (index, (param, value)) in
                        function.params.iter().zip(current_args).enumerate()
                    {
                        let name = match &cached_names {
                            Some(names) => names[index].clone(),
                            None => Rc::from(param.name.as_str()),
                        };
                        env.define(name, value, parameter_binds_mutable(param));
                    }
                    (function, env, false)
                }
                TailCallable::Closure(Value::Closure { function, env }) => {
                    if function.params.len() != current_args.len() {
                        break err(format!(
                            "function expects {} argument(s) but got {}",
                            function.params.len(),
                            current_args.len()
                        ));
                    }
                    let mut env = *env;
                    env.push();
                    let cached_names = self.param_names.get(&(Rc::as_ptr(&function) as usize)).cloned();
                    for (index, (param, value)) in
                        function.params.iter().zip(current_args).enumerate()
                    {
                        let name = match &cached_names {
                            Some(names) => names[index].clone(),
                            None => Rc::from(param.name.as_str()),
                        };
                        env.define(name, value, parameter_binds_mutable(param));
                    }
                    (function, env, true)
                }
                TailCallable::Closure(_) => break err("attempted to call a non-function value"),
            };
            self.cur_fn = function.name.clone();
            self.tail_function = Some(function.clone());
            dynamic_chain |= is_closure;
            self.tail_dynamic_chain = dynamic_chain;
            match self.eval_function_block(&function.body, &function, &mut env) {
                Err(Flow::TailCall { callable: next, args: next_args }) => {
                    callable = next;
                    argvals = next_args;
                }
                Ok(value) | Err(Flow::Return(value)) => {
                    break Ok(CallableOutcome { value, function, env });
                }
                Err(error @ Flow::Err(_)) => break Err(error),
                Err(Flow::Break | Flow::Continue) => {
                    break err("`break`/`continue` outside a loop");
                }
            }
        };
        self.tail_function = prev_tail_function;
        self.tail_dynamic_chain = prev_tail_dynamic_chain;
        self.depth -= 1;
        if result.is_ok() {
            self.cur_fn = prev_fn;
            self.cur_line = prev_line;
        }
        result
    }

    /// Apply a closure to already-evaluated arguments. The closure runs in its
    /// captured environment (plus a fresh scope for the parameters), and its body
    /// is a function boundary, so a `?` inside it returns from the closure.
    pub(super) fn run_closure(&mut self, clo: Value, argvals: Vec<Value>) -> Result<ClosureOutcome, Flow> {
        let outcome = self.run_callable(TailCallable::Closure(clo), argvals)?;
        let writebacks = outcome
            .function
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| parameter_writes_back(param))
            .map(|(index, param)| {
                let value = outcome
                    .env
                    .get(&param.name)
                    .cloned()
                    .expect("closure parameter is bound");
                (index, value)
            })
            .collect();
        Ok(ClosureOutcome { value: outcome.value, writebacks })
    }

    pub(super) fn apply_closure(&mut self, clo: Value, argvals: Vec<Value>) -> Result<Value, Flow> {
        let outcome = self.run_closure(clo, argvals)?;
        if !outcome.writebacks.is_empty() {
            return err("a `var` function value requires a mutable caller place");
        }
        Ok(outcome.value)
    }

    pub(super) fn apply_closure_call(
        &mut self,
        clo: Value,
        argvals: Vec<Value>,
        places: Vec<Option<CapturedPlace>>,
        env: &mut Env,
    ) -> Result<Value, Flow> {
        let outcome = self.run_closure(clo, argvals)?;
        let writebacks = outcome
            .writebacks
            .into_iter()
            .map(|(index, value)| {
                let place = places
                    .get(index)
                    .and_then(Clone::clone)
                    .ok_or_else(|| Flow::from(RuntimeError {
                        message: "a `var` function-value argument must be a mutable place".into(),
                    }))?;
                Ok((place, value))
            })
            .collect::<Result<Vec<_>, Flow>>()?;
        self.commit_writebacks(writebacks, env)?;
        Ok(outcome.value)
    }

    /// Evaluate a function call expression, honoring parameter conventions:
    /// `var` arguments must be mutable variables and are written back after
    /// the call returns (Hylo-style move-in / move-out).
    pub(super) fn eval_call_args(
        &mut self,
        args: &[Expr],
        params: &[Param],
        env: &mut Env,
    ) -> Result<(Vec<Value>, Vec<Option<CapturedPlace>>), Flow> {
        let mut values = Vec::with_capacity(args.len());
        // The overwhelmingly common call has no writable-place parameter; leave `places`
        // unallocated then (`Vec::new` doesn't allocate, and every consumer
        // reads it through `.get(i)`, where absent == None).
        let any_writeback = params
            .iter()
            .take(args.len())
            .any(parameter_writes_back);
        let mut places = if any_writeback { Vec::with_capacity(args.len()) } else { Vec::new() };
        for (index, arg) in args.iter().enumerate() {
            let param = params.get(index);
            if param.is_some_and(parameter_writes_back) {
                if param.is_some_and(is_exclusive_reference)
                    && !matches!(arg, Expr::Unary { op: UnOp::BorrowMut, .. })
                {
                    let value = self.eval(arg, env)?;
                    if !matches!(value, Value::Reference { mutable: true, .. }) {
                        return err("an `&mut` parameter requires an exclusive reference or `&mut place`");
                    }
                    // A first-class reference already identifies the caller's
                    // storage.  Do not turn it back into a copy/write-back pair.
                    values.push(value);
                    places.push(None);
                } else {
                    let place_expr = if param.is_some_and(is_exclusive_reference) {
                        match arg {
                            Expr::Unary { op: UnOp::BorrowMut, expr } => &**expr,
                            _ => unreachable!("the direct reference arm returned above"),
                        }
                    } else {
                        arg
                    };
                    let place = self.capture_place(place_expr, env)?;
                    let value = self.read_place_value(&place, env)?;
                    values.push(value);
                    places.push(Some(place));
                }
            } else {
                values.push(self.eval(arg, env)?);
                if any_writeback {
                    places.push(None);
                }
            }
        }
        Ok((values, places))
    }

    pub(super) fn call_interpreter_special(
        &mut self,
        name: &str,
        argvals: &[Value],
    ) -> Result<Option<(Value, Vec<Value>)>, Flow> {
        // NOTE: any name for which this OR `call_builtin` produces a result MUST be
        // covered by `is_interpreter_builtin` (below) — the fast path in `eval_call`
        // skips both when that predicate is false. `interpreter_builtin_names_are_covered`
        // (test) enforces it so a new dispatch arm can't silently regress the fast path.
        // Native `var` operations have two independent result channels: the
        // ordinary source value and each final `var` value. Keep that split here
        // instead of encoding write-back into a tuple that source code must unpack.
        if intrinsics::is_list_pop_extract(name) && argvals.len() == 1
        {
            let Value::List(items) = &argvals[0] else {
                return err("pop expects a list");
            };
            let mut out = (**items).clone();
            let old = match out.pop() {
                Some(value) => Value::ctor("Some", vec![value]),
                None => Value::ctor("None", Vec::new()),
            };
            return Ok(Some((old, vec![Value::list(out)])));
        }
        if intrinsics::is_dict_insert_extract(name) && argvals.len() == 3
        {
            let Value::Dict(entries) = &argvals[0] else {
                return err("insert expects a Dict, a key, and a value");
            };
            let mut out = (**entries).clone();
            let previous = match self.dict_key_position(&out, &argvals[1])? {
                Some(index) => {
                    let old = std::mem::replace(&mut out[index].1, argvals[2].clone());
                    Value::ctor("Some", vec![old])
                }
                None => {
                    out.push((argvals[1].clone(), argvals[2].clone()));
                    Value::ctor("None", Vec::new())
                }
            };
            return Ok(Some((previous, vec![Value::dict(out)])));
        }
        if intrinsics::is_dict_remove_extract(name) && argvals.len() == 2
        {
            let Value::Dict(entries) = &argvals[0] else {
                return err("remove expects a Dict and a key");
            };
            let mut out = (**entries).clone();
            let previous = match self.dict_key_position(&out, &argvals[1])? {
                Some(index) => Value::Ctor {
                    name: "Some".into(),
                    fields: Rc::new(vec![out.remove(index).1]),
                },
                None => Value::ctor("None", Vec::new()),
            };
            return Ok(Some((previous, vec![Value::dict(out)])));
        }
        // These two operations need the interpreter to apply a function value,
        // so they cannot live in the pure builtin table.
        if name == intrinsics::DICT_UPDATE && argvals.len() == 4 {
            let Value::Dict(entries) = &argvals[0] else {
                return err("update expects a Dict as its first argument");
            };
            let mut out = (**entries).clone();
            let key = &argvals[1];
            let position = self.dict_key_position(&out, key)?;
            let current = position
                .map(|index| out[index].1.clone())
                .unwrap_or_else(|| argvals[2].clone());
            let new_v = self.apply_closure(argvals[3].clone(), vec![current])?;
            match position {
                Some(index) => out[index].1 = new_v,
                None => out.push((argvals[1].clone(), new_v)),
            }
            return Ok(Some((Value::dict(out), Vec::new())));
        }
        if name == "vm.par_map" && argvals.len() == 2 {
            let Value::List(items) = &argvals[0] else {
                return err("par_map expects a list as its first argument");
            };
            let items = items.clone();
            let f = argvals[1].clone();
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter().cloned() {
                out.push(self.apply_closure(f.clone(), vec![item])?);
            }
            return Ok(Some((Value::list(out), Vec::new())));
        }
        Ok(None)
    }

    /// Dispatch a compiler-owned existential call through its authenticated
    /// closed-program witness entry. This deliberately reuses ordinary function
    /// execution and the normal write-back commit path: tail returns, explicit
    /// returns, and `?` therefore commit together, while traps commit nothing.
    pub(super) fn eval_existential_call(
        &mut self,
        receiver: &Expr,
        args: &[Expr],
        owner_trait: &str,
        method: &str,
        slot: u32,
        env: &mut Env,
    ) -> Result<Value, Flow> {
        let receiver_convention = self
            .witnesses
            .witnesses
            .iter()
            .filter_map(|witness| witness.slots.get(usize::try_from(slot).ok()?))
            .find(|entry| entry.owner_trait == owner_trait && entry.method == method)
            .map(|entry| entry.receiver)
            .ok_or_else(|| Flow::from(RuntimeError {
                message: format!(
                    "internal: no witness layout authenticates `{owner_trait}.{method}` slot {slot}",
                ),
            }))?;
        let receiver_place = if receiver_convention == Convention::Var {
            Some(self.capture_place(receiver, env)?)
        } else {
            None
        };
        let receiver_value = match &receiver_place {
            Some(place) => self.read_place_value(place, env)?,
            None => self.eval(receiver, env)?,
        };
        let Value::Existential { payload, witness } = receiver_value else {
            return err("internal: existential dispatch received a non-existential receiver");
        };
        let (adapter, receiver_convention) = {
            let witness_plan = self.witnesses.by_id(witness).ok_or_else(|| Flow::from(RuntimeError {
                message: format!("internal: unknown existential witness {witness}"),
            }))?;
            let entry = witness_plan
                .slots
                .get(usize::try_from(slot).map_err(|_| Flow::from(RuntimeError {
                    message: "internal: existential slot does not fit host indexing".to_string(),
                }))?)
                .ok_or_else(|| Flow::from(RuntimeError {
                    message: format!("internal: witness {witness} has no slot {slot}"),
                }))?;
            if entry.owner_trait != owner_trait || entry.method != method {
                return err(format!(
                    "internal: witness {witness} slot {slot} does not authenticate `{owner_trait}.{method}`"
                ));
            }
            (entry.adapter.clone(), entry.receiver)
        };
        let Some(function) = self.functions.get(&adapter).cloned() else {
            return err(format!(
                "internal: existential adapter `{}` is not registered",
                adapter
            ));
        };
        if function.params.len() != args.len() + 1 {
            return err(format!(
                "internal: existential adapter `{}` has a mismatched signature",
                adapter
            ));
        }
        if function.params.first().map(|param| param.convention) != Some(receiver_convention) {
            return err(format!(
                "internal: existential adapter `{}` changed receiver convention",
                adapter
            ));
        }

        let (mut values, explicit_places) = self.eval_call_args(args, &function.params[1..], env)?;
        values.insert(0, *payload);
        let outcome = self.run_callable(TailCallable::Function(function), values)?;
        let result = outcome.value;
        let mut writebacks = Vec::new();
        if let Some(place) = receiver_place {
            let updated_payload = outcome
                .env
                .get(&outcome.function.params[0].name)
                .cloned()
                .expect("terminal existential receiver is bound");
            writebacks.push((
                place,
                Value::Existential {
                    payload: Box::new(updated_payload),
                    witness,
                },
            ));
        }
        for (index, param) in outcome.function.params.iter().enumerate().skip(1) {
            if !parameter_writes_back(param) {
                continue;
            }
            let place = explicit_places
                .get(index - 1)
                .and_then(Clone::clone)
                .ok_or_else(|| Flow::from(RuntimeError {
                    message: format!(
                        "`var` argument to existential `{owner_trait}.{method}` must be a mutable place"
                    ),
                }))?;
            let value = outcome
                .env
                .get(&param.name)
                .cloned()
                .expect("terminal existential var parameter is bound");
            writebacks.push((place, value));
        }
        self.commit_writebacks(writebacks, env)?;
        Ok(result)
    }

    pub(super) fn eval_call(&mut self, name: &str, args: &[Expr], env: &mut Env) -> Result<Value, Flow> {
        // Record an assertion call SITE *before* evaluating arguments — nested
        // calls in the arguments move `cur_line`, so capturing it later (e.g. once
        // we're inside the callee) would report the wrong line.
        self.note_assert_crossing(name);
        let name = witchy_syntax::cap_ops::surface_name(name);
        let local_closure = matches!(env.get(name), Some(Value::Closure { .. }))
            .then(|| env.get(name).expect("closure just matched").clone());
        // ONE table lookup for the whole call (an Rc clone): it feeds the
        // parameter-convention slice here and is the callee at the end —
        // no per-call Vec<Convention> collect, no second lookup.
        let callee = self.functions.get(name).cloned();
        let closure_fn = local_closure.as_ref().and_then(|value| match value {
            Value::Closure { function, .. } => Some(function.clone()),
            _ => None,
        });
        let params: &[Param] = closure_fn
            .as_ref()
            .or(callee.as_ref())
            .map(|function| function.params.as_slice())
            .unwrap_or(&[]);
        let (argvals, places) = self.eval_call_args(args, params, env)?;
        // Fast path: skip both builtin-dispatch probes for a name that is not a
        // builtin (a plain user function / closure). Each probe otherwise re-scans
        // the intrinsic table (see is_interpreter_builtin) — ~33% of call-dense
        // interpreter self-time. Ordering within the builtin case is UNCHANGED:
        // special before the closure check, call_builtin after.
        let maybe_builtin = is_interpreter_builtin(name);
        if maybe_builtin {
            if let Some((value, var_values)) = self.call_interpreter_special(name, &argvals)? {
                let var_places: Vec<CapturedPlace> = params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        (parameter_writes_back(param))
                            .then(|| places.get(index).and_then(Clone::clone))
                            .flatten()
                    })
                    .collect();
                if var_places.len() != var_values.len() {
                    return err(format!(
                        "internal: native `{name}` returned {} `var` value(s), expected {}",
                        var_values.len(),
                        var_places.len()
                    ));
                }
                self.commit_writebacks(var_places.into_iter().zip(var_values).collect(), env)?;
                return Ok(value);
            }
        }
        // A local variable holding a function value (a closure): apply it.
        if let Some(clo) = local_closure {
            return self.apply_closure_call(clo, argvals, places, env);
        }
        if maybe_builtin {
            if let Some(v) = self.call_builtin(name, &argvals)? {
                let var_indices: Vec<usize> = params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        (parameter_writes_back(param)).then_some(index)
                    })
                    .collect();
                if let [index] = var_indices.as_slice() {
                    let place = places
                        .get(*index)
                        .and_then(Clone::clone)
                        .ok_or_else(|| Flow::from(RuntimeError {
                            message: format!("`var` argument to `{name}` must be a mutable place"),
                        }))?;
                    // Current native collection primitives return the updated receiver.
                    // The stdlib migration will split auxiliary results from this
                    // write-back channel without changing the place machinery.
                    self.commit_writebacks(vec![(place, v.clone())], env)?;
                }
                return Ok(v);
            }
        }
        let Some(func) = callee else {
            return err(format!("call to unknown function `{name}`"));
        };
        if func.params.len() != argvals.len() {
            return err(format!(
                "`{name}` expects {} argument(s) but got {}",
                func.params.len(),
                argvals.len()
            ));
        }
        let mut writeback_indices: Vec<(usize, CapturedPlace)> = Vec::new();
        for (i, param) in func.params.iter().enumerate() {
            if parameter_writes_back(param) {
                if let Some(place) = places.get(i).and_then(Clone::clone) {
                    writeback_indices.push((i, place));
                } else if !is_exclusive_reference(param) {
                    return err(format!("`var` argument to `{name}` must be a mutable place"));
                }
            }
        }
        // The callee's own `?` early-return stops at this callable boundary; it
        // becomes the call's value rather than propagating into the caller.
        let outcome = self.run_callable(TailCallable::Function(func), argvals)?;
        let result = outcome.value;
        let fenv = outcome.env;
        let writebacks = writeback_indices
            .into_iter()
            .map(|(index, place)| {
                let param = &outcome.function.params[index];
                let value =
                fenv.get(&param.name)
                    .cloned()
                    .expect("terminal var parameter is bound");
                (place, value)
            })
            .collect();
        self.commit_writebacks(writebacks, env)?;
        Ok(result)
    }
}
