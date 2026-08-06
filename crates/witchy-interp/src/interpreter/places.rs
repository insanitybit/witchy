//! Memory-place resolution and write-back: capturing an assignable place from
//! an expression, reading and storing through it, the desugared-place fast
//! path, and committing deferred write-backs. Split out of interpreter.rs as an
//! impl continuation.

use super::*;

impl Interpreter {
    pub(super) fn capture_place(&mut self, expr: &Expr, env: &mut Env) -> Result<CapturedPlace, Flow> {
        fn capture(
            interpreter: &mut Interpreter,
            expr: &Expr,
            env: &mut Env,
            projections: &mut Vec<PlaceProjection>,
        ) -> Result<String, Flow> {
            match expr {
                Expr::Var(root) => Ok(root.clone()),
                Expr::Field { base, field } => {
                    let root = capture(interpreter, base, env, projections)?;
                    projections.push(PlaceProjection::Field(field.clone()));
                    Ok(root)
                }
                Expr::Index { base, index } => {
                    let root = capture(interpreter, base, env, projections)?;
                    let index = interpreter.eval(index, env)?;
                    projections.push(PlaceProjection::Index(index));
                    Ok(root)
                }
                Expr::Call { name, args }
                    if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                        && args.len() == 2 =>
                {
                    let root = capture(interpreter, &args[0], env, projections)?;
                    let index = interpreter.eval(&args[1], env)?;
                    projections.push(PlaceProjection::Index(index));
                    Ok(root)
                }
                _ => err("a `var` argument must be a mutable place"),
            }
        }

        let mut projections = Vec::new();
        let root = capture(self, expr, env, &mut projections)?;
        Ok(CapturedPlace { root, projections })
    }

    pub(super) fn place_field_index(&self, value: &Value, field: &str) -> Result<usize, Flow> {
        if let Ok(index) = field.parse::<usize>() {
            return match value {
                Value::Tuple(items) if index < items.len() => Ok(index),
                Value::Tuple(items) => err(format!(
                    "tuple has no element `.{index}` (it has {})",
                    items.len()
                )),
                other => err(format!("element access `.{index}` on a non-tuple value `{other}`")),
            };
        }
        let Value::Ctor { name, fields } = value else {
            return err(format!("field access `.{field}` on a non-record value `{value}`"));
        };
        self.record_fields
            .get(&**name)
            .and_then(|names| names.iter().position(|candidate| candidate == field))
            .filter(|index| *index < fields.len())
            .ok_or_else(|| Flow::from(RuntimeError { message: format!("`{name}` has no field `{field}`") }))
    }

    pub(super) fn read_place_value(&self, place: &CapturedPlace, env: &Env) -> Result<Value, Flow> {
        let mut value = env
            .get(&place.root)
            .cloned()
            .ok_or_else(|| Flow::from(RuntimeError {
                message: format!(
                    "`var` argument root `{}` must be a local variable",
                    place.root
                ),
            }))?;
        for projection in &place.projections {
            value = match projection {
                PlaceProjection::Field(field) => {
                    let index = self.place_field_index(&value, field)?;
                    match &value {
                        Value::Tuple(items) => items[index].clone(),
                        Value::Ctor { fields, .. } => fields[index].clone(),
                        _ => unreachable!("place_field_index checked the aggregate"),
                    }
                }
                PlaceProjection::Index(index) => match (&value, index) {
                    (Value::List(items), Value::Int(index))
                    | (Value::Tuple(items), Value::Int(index))
                        if *index >= 0 && (*index as usize) < items.len() =>
                    {
                        items[*index as usize].clone()
                    }
                    (Value::Dict(entries), key) => entries
                        .iter()
                        .find(|(candidate, _)| candidate == key)
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| Flow::from(RuntimeError {
                            message: "dictionary key is absent".into(),
                        }))?,
                    (Value::List(items), Value::Int(index))
                    | (Value::Tuple(items), Value::Int(index)) => {
                        return err(format!(
                            "index {index} is out of bounds for length {}",
                            items.len()
                        ));
                    }
                    (_, other) => return err(format!("invalid place index `{other}`")),
                },
            };
        }
        Ok(value)
    }

    pub(super) fn store_place_value(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
    ) -> Result<(), Flow> {
        self.store_place_value_inner(current, projections, replacement, false)
    }

    pub(super) fn store_assignment_place_value(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
    ) -> Result<(), Flow> {
        self.store_place_value_inner(current, projections, replacement, true)
    }

    pub(super) fn store_place_value_inner(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
        insert_missing_dict_leaf: bool,
    ) -> Result<(), Flow> {
        let Some((projection, rest)) = projections.split_first() else {
            *current = replacement;
            return Ok(());
        };
        match projection {
            PlaceProjection::Field(field) => {
                let index = self.place_field_index(current, field)?;
                match current {
                    Value::Tuple(items) => self.store_place_value_inner(
                        &mut Rc::make_mut(items)[index],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    ),
                    Value::Ctor { fields, .. } => self.store_place_value_inner(
                        &mut Rc::make_mut(fields)[index],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    ),
                    _ => unreachable!("place_field_index checked the aggregate"),
                }
            }
            PlaceProjection::Index(index) => match (current, index) {
                (Value::List(items), Value::Int(index))
                    if *index >= 0 && (*index as usize) < items.len() =>
                {
                    self.store_place_value_inner(
                        &mut Rc::make_mut(items)[*index as usize],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    )
                }
                (Value::Tuple(items), Value::Int(index))
                    if *index >= 0 && (*index as usize) < items.len() =>
                {
                    self.store_place_value_inner(
                        &mut Rc::make_mut(items)[*index as usize],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    )
                }
                (Value::Dict(entries), key) => {
                    let position = self.dict_key_position(entries, key)?;
                    let entries = Rc::make_mut(entries);
                    if let Some(index) = position {
                        self.store_place_value_inner(
                            &mut entries[index].1,
                            rest,
                            replacement,
                            insert_missing_dict_leaf,
                        )
                    } else if insert_missing_dict_leaf && rest.is_empty() {
                        entries.push((key.clone(), replacement));
                        Ok(())
                    } else {
                        err("dictionary key is absent")
                    }
                }
                (Value::List(items), Value::Int(index)) => err(
                    DiagTemplate::ListIndexOob.render(
                        *index,
                        items.len() as i64,
                        "",
                    ),
                ),
                (Value::Tuple(items), Value::Int(index)) => err(format!(
                    "index {index} is out of bounds for length {}",
                    items.len()
                )),
                (_, other) => err(format!("invalid place index `{other}`")),
            },
        }
    }

    pub(super) fn try_desugared_place_assign(
        &mut self,
        name: &str,
        expression: &Expr,
        env: &mut Env,
    ) -> Result<bool, Flow> {
        let Some(plan) = desugared_assignment_plan(name, expression) else {
            return Ok(false);
        };
        let mut projections = Vec::with_capacity(plan.projections.len());
        for projection in plan.projections {
            projections.push(match projection {
                AssignmentProjection::Field(field) => {
                    PlaceProjection::Field(field.to_string())
                }
                AssignmentProjection::Index { expression, .. } => {
                    PlaceProjection::Index(self.eval(expression, env)?)
                }
            });
        }
        let replacement = self.eval(plan.replacement, env)?;
        let mut current = env.get(name).cloned().ok_or_else(|| {
            Flow::from(RuntimeError {
                message: format!("cannot assign to unbound variable `{name}`"),
            })
        })?;
        self.store_assignment_place_value(
            &mut current,
            &projections,
            replacement,
        )?;
        match env.assign(name, current) {
            Assign::Done => Ok(true),
            Assign::Immutable => err(format!(
                "cannot assign to `{name}`: it is immutable (declared with `let`)"
            )),
            Assign::Unbound => {
                err(format!("cannot assign to unbound variable `{name}`"))
            }
        }
    }

    pub(super) fn commit_writebacks(
        &mut self,
        writebacks: Vec<(CapturedPlace, Value)>,
        env: &mut Env,
    ) -> Result<(), Flow> {
        let mut roots: Vec<(String, Value)> = Vec::new();
        for (place, value) in writebacks {
            let root = if let Some((_, root)) = roots.iter_mut().find(|(name, _)| *name == place.root) {
                root
            } else {
                let current = env.get(&place.root).cloned().ok_or_else(|| {
                    Flow::from(RuntimeError {
                        message: format!(
                            "`var` argument root `{}` must be a local variable",
                            place.root
                        ),
                    })
                })?;
                roots.push((place.root.clone(), current));
                &mut roots.last_mut().expect("just pushed root").1
            };
            self.store_place_value(root, &place.projections, value)?;
        }
        for (name, value) in roots {
            match env.assign(&name, value) {
                Assign::Done => {}
                Assign::Immutable => {
                    return err(format!("`var` argument root `{name}` must be a mutable `var`"));
                }
                Assign::Unbound => {
                    return err(format!("`var` argument root `{name}` must be a local variable"));
                }
            }
        }
        Ok(())
    }
}
