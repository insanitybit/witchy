//! Source-bound terminal diagnostics over authenticated read-only byte slices.
use super::*;

pub(super) const IMPORT: &str = "__witchy_abort_at";

#[derive(Clone)]
pub(super) struct Site {
    pub function: String,
    pub line: u32,
}

pub(super) fn sites(module: &witchy_syntax::ast::Module) -> Result<HashMap<usize, Site>, CanonicalLoweringError> {
    fn block(
        body: &witchy_syntax::ast::Block, function: &str,
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
                    | Expr::For { body, .. } => block(body, function, sites, visited)?,
                    Expr::If { then_block, else_block, .. } => {
                        block(then_block, function, sites, visited)?;
                        if let Some(body) = else_block { block(body, function, sites, visited)?; }
                    }
                    _ => {}
                }
                if matches!(expression, Expr::Call { name, .. } if name == "fail") {
                    let key = expression as *const Expr as usize;
                    if !sites.contains_key(&key) {
                        let line = body.lines.get(index).copied().ok_or(
                            CanonicalLoweringError::UnsupportedExpression("fail source location"),
                        )?;
                        sites.insert(key, Site { function: function.to_owned(), line });
                    }
                }
                Ok::<_, CanonicalLoweringError>(())
            })?;
        }
        Ok(())
    }
    let scope = witchy_types::pipeline::canonical_scope(module);
    let mut sites = HashMap::new();
    let mut visited = HashSet::new();
    for item in &module.items {
        if let Item::Function(function) = item && scope.contains(&function.name) {
            block(&function.body, &function.name, &mut sites, &mut visited)?;
        }
    }
    Ok(sites)
}

pub(super) fn catalog(
    catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, layout: witchy_wir::layout::LayoutId,
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
    catalog.with_abort_import(id, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_abort(&mut self, expression: &Expr, arguments: &[Expr]) -> Result<(), CanonicalLoweringError> {
        let [message] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("fail arguments"));
        };
        let site = self.host_imports.abort_sites.get(&(expression as *const Expr as usize)).cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("fail source witness"))?;
        // Evaluate once, before publishing this call's own immutable source site.
        let message = self.lower_expr(message)?;
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
