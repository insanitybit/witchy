//! Non-destructive checks over source-only syntax.
//!
//! Destructive source lowerings consume [`SourceCheckedModule`] so their call
//! sites cannot accidentally erase generator or async syntax before the rules
//! that depend on those nodes have run. The proof is preserved as each lowering
//! rewrites the owned module.

use crate::ast::Module;

/// An owned module whose source-only generator and async rules have been
/// checked without rewriting its AST.
#[derive(Debug)]
pub struct SourceCheckedModule {
    module: Module,
}

impl SourceCheckedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    /// Finish the source-lowering sequence and recover the transformed module.
    pub fn into_module(self) -> Module {
        self.module
    }
}

/// Check source-only semantics before any pass can erase their syntax.
pub fn check(module: Module) -> Result<SourceCheckedModule, String> {
    crate::generators::validate_source(&module)?;
    crate::async_lower::validate_source(&module)?;
    Ok(SourceCheckedModule { module })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Module {
        crate::parser::parse_module(source).expect("parse source-check fixture")
    }

    #[test]
    fn generator_region_error_precedes_destructive_lowering() {
        let module = parse(
            "gen fn bad() -> Iter(Int):\n  region:\n    yield 1\n    0\n",
        );
        let error = check(module).expect_err("source check must reject yield in region");
        assert!(error.contains("cannot `yield` inside `region:`"), "{error}");
    }

    #[test]
    fn async_tail_region_error_precedes_destructive_lowering() {
        let module = parse(
            "async fn bad() -> Int:\n  if true:\n    region:\n      1\n",
        );
        let error = check(module).expect_err("source check must reject async tail region");
        assert!(error.contains("`region:` in an async tail"), "{error}");
    }

    #[test]
    fn destructive_source_lowerers_require_the_proof_wrapper() {
        let generators = include_str!("generators.rs");
        let async_lower = include_str!("async_lower.rs");
        assert!(generators.contains("pub fn lower(mut checked: SourceCheckedModule)"));
        assert!(async_lower.contains("pub fn lower(checked: SourceCheckedModule)"));
        assert!(async_lower.contains("mut checked: SourceCheckedModule"));
    }
}
