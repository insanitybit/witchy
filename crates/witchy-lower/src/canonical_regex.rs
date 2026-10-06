//! Authenticated native regex operations over checked string slices: spans
//! (reconstructed by ordinary std wrappers), a match count, and literal
//! global replacement.
use super::*;

/// The native regex operations a module's reachable functions call.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Needed {
    pub(super) spans: bool,
    pub(super) count: bool,
    pub(super) replace_all: bool,
}

impl Needed {
    pub(super) fn any(self) -> bool {
        self.spans || self.count || self.replace_all
    }
}

pub(super) fn needed(module: &witchy_syntax::ast::Module) -> Needed {
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut needed = Needed::default();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression {
                match name.as_str() {
                    witchy_syntax::intrinsics::REGEX_MATCH_SPANS => needed.spans = true,
                    witchy_syntax::intrinsics::REGEX_COUNT => needed.count = true,
                    witchy_syntax::intrinsics::REGEX_REPLACE_ALL => needed.replace_all = true,
                    _ => {}
                }
            }
            Ok(())
        }).expect("regex import visitor cannot fail");
    }
    needed
}

impl FunctionLowerer<'_> {
    /// The string arguments of a regex operation as read slices, evaluated in
    /// source order: regex is pure, but evaluating its operands is not.
    fn regex_inputs(&mut self, callee: &str, args: &[Expr], arity: usize) -> Result<Vec<HostCallArgument>, CanonicalLoweringError> {
        if args.len() != arity {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: callee.to_owned(), expected: arity, actual: args.len(),
            });
        }
        let layout = self.string_layout_id()?;
        let mut inputs = Vec::with_capacity(args.len());
        for argument in args {
            let value = self.lower_string_argument(argument)?;
            // A later argument may rebind or mutate a `var` an earlier one
            // read: each operand keeps its own snapshot for both calls.
            let snapshot = self.clone_reference_token(value)?;
            inputs.push(self.string_slice_argument(snapshot, layout)?);
        }
        Ok(inputs)
    }

    pub(super) fn lower_regex_spans(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let import = self.host_imports.regex_spans
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated regex spans import"))?;
        let inputs = self.regex_inputs(witchy_syntax::intrinsics::REGEX_MATCH_SPANS, args, 2)?;
        // The ordinary helper owns allocation, the second answer's capacity
        // guard, UpdateCount, and RC accounting. No tuple layouts enter the ABI.
        self.lower_bounded_host_result(import, inputs)
    }

    pub(super) fn lower_regex_count(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let import = self.host_imports.regex_count
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated regex count import"))?;
        let inputs = self.regex_inputs(witchy_syntax::intrinsics::REGEX_COUNT, args, 2)?;
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![count], import, arguments: inputs });
        let destination = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination, value: count });
        Ok(destination)
    }

    pub(super) fn lower_regex_replace_all(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let import = self.host_imports.regex_replace_all
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated regex replace import"))?;
        let inputs = self.regex_inputs(witchy_syntax::intrinsics::REGEX_REPLACE_ALL, args, 3)?;
        self.lower_bounded_host_result(import, inputs)
    }
}
