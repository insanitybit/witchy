//! Source-bound terminal diagnostics over authenticated read-only byte slices.
use super::*;

pub(super) const IMPORT: &str = "__witchy_abort_at";
pub(super) const BOUNDS_IMPORT: &str = "__witchy_list_bounds_at";
pub(super) const BYTES_BOUNDS_IMPORT: &str = "__witchy_bytes_bounds_at";
pub(super) const PARSE_INT_IMPORT: &str = "__witchy_parse_int_at";

pub(super) fn is_list_access(expression: &Expr) -> bool {
    // A range subscript is projection syntax consumed by authenticated
    // rooted-slice formation. It never reaches `lower_index` and therefore
    // must not register an unreferenced bounds-diagnostic payload.
    matches!(expression, Expr::Index { index, .. } if !matches!(index.as_ref(), Expr::Range { .. }))
        || matches!(expression,
        Expr::Call { name, .. } if matches!(name.as_str(), "list.at" | "list.__at" | "list.__set_at"))
}

#[derive(Clone)]
pub(super) struct Site {
    pub function: String,
    pub line: u32,
    pub bytes_access: bool,
    pub parse_int_access: bool,
}

pub(super) fn sites(
    module: &witchy_syntax::ast::Module, table: &TypeTable, console: bool,
) -> Result<HashMap<usize, Site>, CanonicalLoweringError> {
    // Ordering a NaN aborts, so only Float orderings carry a diagnostic.
    fn float_ordering(expression: &Expr, table: &TypeTable) -> bool {
        matches!(expression, Expr::Binary { op: BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq, lhs, .. }
            if matches!(table.type_of(lhs).and_then(witchy_types::typeck::ty_to_ast),
                Some(Type::Named(name, arguments)) if name == "Float" && arguments.is_empty()))
    }
    fn block(
        body: &witchy_syntax::ast::Block, function: &str, table: &TypeTable, console: bool,
        sites: &mut HashMap<usize, Site>, visited: &mut HashSet<usize>,
    ) -> Result<(), CanonicalLoweringError> {
        if !visited.insert(body as *const _ as usize) { return Ok(()); }
        for (index, statement) in body.stmts.iter().enumerate() {
            let expression = match statement {
                Stmt::Let { value, .. } | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. } | Stmt::Expr(value)
                | Stmt::Yield(value) | Stmt::Return(Some(value)) => value,
                _ => continue,
            };
            witchy_syntax::ast::visit::visit_expr(expression, &mut |expression| {
                // Visit nested statement owners before their expressions are
                // reached by the generic visitor, retaining the innermost line.
                match expression {
                    Expr::Block(body) | Expr::Lambda { body, .. }
                    | Expr::While { body, .. } | Expr::WhileLet { body, .. }
                    | Expr::For { body, .. } => block(body, function, table, console, sites, visited)?,
                    Expr::If { then_block, else_block, .. } => {
                        block(then_block, function, table, console, sites, visited)?;
                        if let Some(body) = else_block { block(body, function, table, console, sites, visited)?; }
                    }
                    _ => {}
                }
                if is_list_access(expression)
                    || matches!(expression, Expr::Binary { op: BinOp::Div | BinOp::Mod, .. })
                    || float_ordering(expression, table)
                    || matches!(expression, Expr::Call { name, .. } if name == "fail" || name == "__bytes_at" || name == "bytes.at" || name == "string.to_int" || name == "string.__to_int" || name == "math.to_int" || name == "math.__to_int" || registry_hosts::is_require(name) || (console && is_console_print(name))) {
                    let key = expression as *const Expr as usize;
                    if !sites.contains_key(&key) {
                        let line = body.lines.get(index).copied().ok_or(
                            CanonicalLoweringError::UnsupportedExpression("diagnostic source location"),
                        )?;
                        let bytes_access = matches!(expression, Expr::Call { name, .. } if name == "__bytes_at" || name == "bytes.at");
                        let parse_int_access = matches!(expression, Expr::Call { name, .. } if name == "string.to_int" || name == "string.__to_int");
                        sites.insert(key, Site { function: function.to_owned(), line, bytes_access, parse_int_access });
                    }
                }
                Ok::<_, CanonicalLoweringError>(())
            })?;
        }
        Ok(())
    }
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut sites = HashMap::new();
    let mut visited = HashSet::new();
    for item in &module.items {
        if let Item::Function(function) = item && scope.contains(&function.name) {
            let source_callable = witchy_syntax::suspension::source_callable_name(function);
            block(&function.body, &source_callable, table, console, &mut sites, &mut visited)?;
        }
    }
    Ok(sites)
}

pub(super) fn catalog(
    catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, layout: witchy_wir::layout::LayoutId,
    bytes_needed: bool, parse_int_needed: bool,
) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
    let parameters = vec![ValueType::Ref(layout), ValueType::Ref(layout), ValueType::Size];
    signatures.push(FunctionSignature {
        id, parameters: parameters.clone(), results: Vec::new(),
        contract: CallableContract {
            parameters: parameters.into_iter().map(|value_type| ParameterContract {
                value_type, ownership: ParameterOwnership::Value,
            }).collect(),
            results: Vec::new(),
            parameter_effects: BTreeMap::from([(0, vec![MemoryEffect::Read]), (1, vec![MemoryEffect::Read])]),
        },
    });
    let catalog = catalog.with_abort_import(id, layout)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
    let parameters = vec![ValueType::Int, ValueType::Size, ValueType::Ref(layout), ValueType::Size];
    signatures.push(FunctionSignature {
        id, parameters: parameters.clone(), results: Vec::new(),
        contract: CallableContract {
            parameters: parameters.into_iter().map(|value_type| ParameterContract {
                value_type, ownership: ParameterOwnership::Value,
            }).collect(), results: Vec::new(),
            parameter_effects: BTreeMap::from([(2, vec![MemoryEffect::Read])]),
        },
    });
    let catalog = catalog.with_list_bounds_import(id, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    let mut catalog = catalog;
    if bytes_needed {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameters = vec![ValueType::Int, ValueType::Size, ValueType::Ref(layout), ValueType::Size];
        signatures.push(FunctionSignature {
            id, parameters: parameters.clone(), results: Vec::new(),
            contract: CallableContract {
                parameters: parameters.into_iter().map(|value_type| ParameterContract { value_type, ownership: ParameterOwnership::Value }).collect(),
                results: Vec::new(), parameter_effects: BTreeMap::from([(2, vec![MemoryEffect::Read])]),
            },
        });
        catalog = catalog.with_bytes_bounds_import(id, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    }
    if parse_int_needed {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameters = vec![ValueType::Ref(layout), ValueType::Ref(layout), ValueType::Size];
        signatures.push(FunctionSignature {
            id, parameters: parameters.clone(), results: Vec::new(),
            contract: CallableContract {
                parameters: parameters.into_iter().map(|value_type| ParameterContract { value_type, ownership: ParameterOwnership::Value }).collect(),
                results: Vec::new(), parameter_effects: BTreeMap::from([(0, vec![MemoryEffect::Read]), (1, vec![MemoryEffect::Read])]),
            },
        });
        catalog = catalog.with_parse_int_import(id, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
    }
    Ok(catalog)
}

impl FunctionLowerer<'_> {
    pub(super) fn diagnostic_site(&self, expression: &Expr) -> Result<Site, CanonicalLoweringError> {
        self.host_imports.abort_sites.get(&(expression as *const Expr as usize)).cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("diagnostic source witness"))
    }

    /// Publish a source-bound arithmetic diagnostic only on the failing path.
    /// The successful continuation retains its pre-branch ownership facts.
    pub(super) fn abort_on_condition(
        &mut self, condition: ValueId, site: &Site, template: witchy_syntax::diag::DiagTemplate,
    ) -> Result<(), CanonicalLoweringError> {
        let fail = self.new_block(&[])?;
        let proceed = self.new_block(&[])?;
        let facts = self.snapshot_facts();
        self.finish_current(Terminator::BranchIf {
            condition, then_target: fail, then_arguments: Vec::new(),
            else_target: proceed, else_arguments: Vec::new(),
        })?;
        self.current = fail;
        let message = self.lower_string_literal(&template.render(0, 0, ""))?;
        self.abort_value_at(site, message)?;
        self.current = proceed;
        self.restore_snapshot(facts);
        Ok(())
    }

    /// Diagnostic only: the ordinary narrowing and dominating Bounds guard
    /// remain on success. This branch establishes no memory authority.
    pub(super) fn diagnose_list_index(&mut self, object: ValueId, index: ValueId, site: &Site) -> Result<(), CanonicalLoweringError> {
        let length = if let Some(family) = self.owner_list_family(object).cloned() {
            self.owner_list_length(object, family.signature)?
        } else {
            self.ensure_list_extent(object)?;
            self.tracked_lanes(object).ok_or(CanonicalLoweringError::UnsupportedExpression("bounds list extent"))?.0
        };
        let import = self.host_imports.bounds.ok_or_else(|| CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("bounds import missing".into()))?;
        self.diagnose_index(index, length, site, import)
    }

    pub(super) fn diagnose_bytes_index(&mut self, object: ValueId, index: ValueId, site: &Site) -> Result<(), CanonicalLoweringError> {
        let length = self.string_length_of(object)?;
        let import = self.host_imports.bytes_bounds.ok_or_else(|| CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("bytes bounds import missing".into()))?;
        self.diagnose_index(index, length, site, import)
    }

    fn diagnose_index(&mut self, index: ValueId, length: ValueId, site: &Site, import: witchy_wir::canonical_safewir::ImportId) -> Result<(), CanonicalLoweringError> {
        let wide_length = self.widen_size_to_int(length)?;
        // Full-width unsigned comparison rejects negatives and >u32::MAX
        // without truncating the original signed diagnostic value.
        let valid = self.push_compare(ComparisonOperator::LessUnsigned, index, wide_length)?;
        let proceed = self.new_block(&[])?;
        let fail = self.new_block(&[])?;
        let facts = self.snapshot_facts();
        self.finish_current(Terminator::BranchIf {
            condition: valid, then_target: proceed, then_arguments: Vec::new(),
            else_target: fail, else_arguments: Vec::new(),
        })?;
        self.current = fail;
        let function = self.lower_string_literal(&site.function)?;
        let start = self.push_const_size(0)?;
        let function_length = self.string_lengths[&function];
        let line = self.push_const_size(u64::from(site.line))?;
        self.push_operation(Operation::HostCall {
            destinations: Vec::new(), import, arguments: vec![
                HostCallArgument::Value(index), HostCallArgument::Value(length),
                HostCallArgument::Buffer {
                    place: witchy_wir::canonical_safewir::Place::projected(function,
                        Projection::Slice { start, length: function_length }, self.values[&function], AccessMode::Read),
                    extent: HostBufferExtent::Slice,
                }, HostCallArgument::Value(line),
            ],
        });
        self.finish_current(Terminator::Trap)?;
        self.current = proceed;
        self.restore_snapshot(facts);
        Ok(())
    }

    pub(super) fn parse_int_failure_block(&mut self, source: ValueId, site: &Site) -> Result<BlockId, CanonicalLoweringError> {
        let continuation = self.current;
        let facts = self.snapshot_facts();
        let fail = self.new_block(&[])?;
        self.current = fail;
        let function = self.lower_string_literal(&site.function)?;
        let start = self.push_const_size(0)?;
        let source_length = self.string_length_of(source)?;
        let function_length = self.string_lengths[&function];
        let line = self.push_const_size(u64::from(site.line))?;
        let import = self.host_imports.parse_int.ok_or_else(|| CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("parse Int import missing".into()))?;
        let buffer = |object, length| HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(object,
                Projection::Slice { start, length }, self.values[&object], AccessMode::Read),
            extent: HostBufferExtent::Slice,
        };
        self.push_operation(Operation::HostCall {
            destinations: Vec::new(), import,
            arguments: vec![buffer(source, source_length), buffer(function, function_length), HostCallArgument::Value(line)],
        });
        self.finish_current(Terminator::Trap)?;
        self.current = continuation;
        self.restore_snapshot(facts);
        Ok(fail)
    }

    pub(super) fn parse_int_unless(&mut self, condition: ValueId, source: ValueId, site: &Site) -> Result<(), CanonicalLoweringError> {
        // Each trap arm has a distinct block: structured Wasm lowering emits
        // terminal arms inline and cannot emit one shared terminal twice.
        let fail = self.parse_int_failure_block(source, site)?;
        let proceed = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition, then_target: proceed, then_arguments: Vec::new(),
            else_target: fail, else_arguments: Vec::new(),
        })?;
        self.current = proceed;
        Ok(())
    }

    pub(super) fn lower_abort(&mut self, expression: &Expr, arguments: &[Expr]) -> Result<(), CanonicalLoweringError> {
        let [message] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("fail arguments"));
        };
        let site = self.host_imports.abort_sites.get(&(expression as *const Expr as usize)).cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("fail source witness"))?;
        // Evaluate once, before publishing this call's own immutable source site.
        let message = self.lower_expr(message)?;
        self.abort_value_at(&site, message)
    }

    pub(super) fn abort_value_at(&mut self, site: &Site, message: ValueId) -> Result<(), CanonicalLoweringError> {
        let function = self.lower_string_literal(&site.function)?;
        let line = self.push_const_size(u64::from(site.line))?;
        let start = self.push_const_size(0)?;
        let layout = string_layout(&Type::Named("String".into(), Vec::new()), self.layout_bundle, self.layouts)?;
        let mut arguments = Vec::new();
        for object in [message, function] {
            if self.values.get(&object) != Some(&ValueType::Ref(layout)) {
                return Err(CanonicalLoweringError::UnsupportedExpression("fail string"));
            }
            let length = *self.string_lengths.get(&object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("fail string length"),
            )?;
            arguments.push(HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(object,
                    Projection::Slice { start, length }, ValueType::Ref(layout), AccessMode::Read),
                extent: HostBufferExtent::Slice,
            });
        }
        arguments.push(HostCallArgument::Value(line));
        let import = self.host_imports.abort.ok_or_else(|| CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("fail import missing".into()))?;
        self.push_operation(Operation::HostCall { destinations: Vec::new(), import, arguments });
        // No return envelope or cleanup continuation exists after a host abort.
        self.finish_current(Terminator::Trap)
    }
}
