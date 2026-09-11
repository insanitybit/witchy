//! Call-local places: capture source coordinates once, then rebuild the current
//! root with the returned value. No saved parent value can overwrite a sibling
//! result or a disjoint update performed while evaluating a later argument.
use super::*;

pub(super) struct PreparedWriteBack {
    pub(super) root: String,
    projections: Vec<PreparedProjection>,
}

enum PreparedProjection {
    Field { source: Type, index: usize },
    Index { source: Type, index: ValueId },
}

pub(super) fn source_place_root(expression: &Expr) -> Option<&str> {
    match expression {
        Expr::Var(name) => Some(name),
        Expr::Field { base, .. } | Expr::Index { base, .. } => source_place_root(base),
        Expr::Call { name, args }
            if name == witchy_syntax::intrinsics::LIST_AT && args.len() == 2 =>
        {
            source_place_root(&args[0])
        }
        _ => None,
    }
}

impl FunctionLowerer<'_> {
    pub(super) fn prepare_write_back(&mut self, expression: &Expr) -> Result<PreparedWriteBack, CanonicalLoweringError> {
        match expression {
            Expr::Var(root) => {
                if !self.bindings.contains_key(root) {
                    return Err(CanonicalLoweringError::MissingBinding(root.clone()));
                }
                Ok(PreparedWriteBack { root: root.clone(), projections: Vec::new() })
            }
            Expr::Field { base, field } => {
                let mut place = self.prepare_write_back(base)?;
                let source = self.expression_type(base)?;
                let shape = scalar_aggregate_shape(&source, self.definitions)
                    .filter(|shape| !shape.tagged)
                    .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
                let index = match source.unqualified() {
                    Type::Tuple(fields) => field.parse::<usize>().ok().filter(|index| *index < fields.len()),
                    Type::Named(name, _) => self.record_definition(name)
                        .and_then(|definition| definition.variants.first())
                        .and_then(|variant| variant.field_names.iter().position(|name| name == field)),
                    _ => None,
                }.filter(|index| *index < shape.fields.len())
                    .ok_or_else(|| CanonicalLoweringError::MissingBinding(field.clone()))?;
                place.projections.push(PreparedProjection::Field { source, index });
                Ok(place)
            }
            Expr::Index { base, index } => self.prepare_index_write_back(base, index),
            Expr::Call { name, args }
                if name == witchy_syntax::intrinsics::LIST_AT && args.len() == 2 =>
            {
                self.prepare_index_write_back(&args[0], &args[1])
            }
            _ => Err(CanonicalLoweringError::UnsupportedExpression("var write-back requires a typed record/list place")),
        }
    }

    fn prepare_index_write_back(&mut self, base: &Expr, index: &Expr) -> Result<PreparedWriteBack, CanonicalLoweringError> {
        let mut place = self.prepare_write_back(base)?;
        let source = self.expression_type(base)?;
        if !matches!(source.unqualified(), Type::Named(name, arguments) if name == "List" && arguments.len() == 1) {
            return Err(CanonicalLoweringError::UnsupportedType(source));
        }
        // Capture all coordinates before reading the place, as the source
        // interpreter does. Index expressions may themselves perform calls.
        let index = self.lower_expr(index)?;
        if self.values[&index] != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression("list place index type"));
        }
        place.projections.push(PreparedProjection::Index { source, index });
        Ok(place)
    }

    pub(super) fn read_prepared_write_back(&mut self, place: &PreparedWriteBack) -> Result<ValueId, CanonicalLoweringError> {
        let mut value = *self.bindings.get(&place.root)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
        for projection in &place.projections {
            value = self.read_write_back_projection(value, projection)?;
        }
        self.ensure_string_extent(value)?;
        self.ensure_list_extent(value)?;
        Ok(value)
    }

    fn read_write_back_field(&mut self, value: ValueId, source: &Type, index: usize, ty: ValueType) -> Result<ValueId, CanonicalLoweringError> {
        if matches!(self.values[&value], ValueType::InlineAggregate(_)) {
            let destination = self.fresh(ty)?;
            self.push_operation(Operation::ExtractInlineField {
                destination, aggregate: value,
                index: u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            });
            Ok(destination)
        } else {
            self.load_source_aggregate_field(value, source, 0, index, index, ty)
        }
    }

    fn read_write_back_projection(&mut self, value: ValueId, projection: &PreparedProjection) -> Result<ValueId, CanonicalLoweringError> {
        match projection {
            PreparedProjection::Field { source, index } => {
                let shape = scalar_aggregate_shape(source, self.definitions)
                    .filter(|shape| !shape.tagged)
                    .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
                let ty = self.field_value_type(&shape.fields[*index])?;
                self.read_write_back_field(value, source, *index, ty)
            }
            PreparedProjection::Index { source, index } => {
                let Type::Named(_, arguments) = source.unqualified() else {
                    return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
                };
                let ty = self.field_value_type(&arguments[0])?;
                let index_size = self.fresh(ValueType::Size)?;
                self.push_operation(Operation::NarrowSize { destination: index_size, value: *index });
                self.ensure_list_extent(value)?;
                self.load_list_element(value, index_size, ty)
            }
        }
    }

    pub(super) fn commit_prepared_write_back(&mut self, place: &PreparedWriteBack, replacement: ValueId) -> Result<(), CanonicalLoweringError> {
        let current = *self.bindings.get(&place.root)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
        let value = self.rebuild_write_back(current, &place.projections, replacement)?;
        self.ensure_string_extent(value)?;
        self.ensure_list_extent(value)?;
        self.bindings.insert(place.root.clone(), value);
        Ok(())
    }

    fn rebuild_write_back(&mut self, current: ValueId, projections: &[PreparedProjection], replacement: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let Some((projection, rest)) = projections.split_first() else {
            return Ok(replacement);
        };
        let replacement = if rest.is_empty() {
            replacement
        } else {
            let child = self.read_write_back_projection(current, projection)?;
            self.rebuild_write_back(child, rest, replacement)?
        };
        match projection {
            PreparedProjection::Index { source, index } => {
                self.replace_list_value(current, *index, replacement, source.clone())
            }
            PreparedProjection::Field { source, index } => {
                let shape = scalar_aggregate_shape(source, self.definitions)
                    .filter(|shape| !shape.tagged)
                    .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
                let mut fields = Vec::with_capacity(shape.fields.len());
                for (slot, source_type) in shape.fields.iter().enumerate() {
                    let ty = self.field_value_type(source_type)?;
                    fields.push(if slot == *index {
                        if self.values[&replacement] != ty {
                            return Err(CanonicalLoweringError::UnsupportedExpression("projected write-back result type"));
                        }
                        replacement
                    } else {
                        self.read_write_back_field(current, source, slot, ty)?
                    });
                }
                if let ValueType::InlineAggregate(layout) = self.values[&current] {
                    let destination = self.fresh(ValueType::InlineAggregate(layout))?;
                    self.push_operation(Operation::MakeInlineAggregate { destination, layout, fields });
                    Ok(destination)
                } else {
                    self.lower_heap_record_for_source(source, &shape.name, &shape.fields, fields)
                }
            }
        }
    }
}
