//! Authenticated native regex spans, reconstructed by ordinary std wrappers.
use super::*;

pub(super) fn is_needed(module: &witchy_syntax::ast::Module) -> bool {
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut needed = false;
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression
                && name == witchy_syntax::intrinsics::REGEX_MATCH_SPANS { needed = true; }
            Ok(())
        }).expect("regex import visitor cannot fail");
    }
    needed
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_regex_spans(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let [pattern, text] = args else {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: witchy_syntax::intrinsics::REGEX_MATCH_SPANS.to_owned(), expected: 2, actual: args.len(),
            });
        };
        let import = self.host_imports.regex_spans
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated regex spans import"))?;
        let layout = self.string_layout_id()?;
        // Source evaluation order remains observable even though regex is pure.
        let pattern = self.lower_string_argument(pattern)?;
        let text = self.lower_string_argument(text)?;
        let inputs = vec![self.string_slice_argument(pattern, layout)?, self.string_slice_argument(text, layout)?];
        // The ordinary helper owns allocation, the second answer's capacity
        // guard, UpdateCount, and RC accounting. No tuple layouts enter the ABI.
        self.lower_bounded_host_result(import, inputs)
    }
}
