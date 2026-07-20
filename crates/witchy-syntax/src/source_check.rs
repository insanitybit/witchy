//! Non-destructive checks over source-only syntax.
//!
//! Destructive source lowerings consume successive opaque typestates so their
//! call sites cannot erase source syntax before the rules that depend on those
//! nodes have run, or skip a stage in the lowering sequence.

use crate::ast::Module;

/// An owned module whose source-only generator and async rules have been
/// checked without rewriting its AST.
#[derive(Debug)]
pub struct SourceCheckedModule {
    module: Module,
}

/// A source-checked module after generator lowering, but before async lowering.
#[derive(Debug)]
pub struct GeneratorsLoweredModule {
    module: Module,
}

/// A source-checked module after generator and async lowering, but before
/// record lowering completes the destructive source-lowering sequence.
#[derive(Debug)]
pub struct AsyncLoweredModule {
    module: Module,
}

/// A source-checked module after the destructive source-lowering sequence has
/// completed. Only this terminal stage exposes the runtime AST to downstream
/// crates.
#[derive(Debug)]
pub struct SourceLoweredModule {
    module: Module,
}

impl SourceCheckedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl GeneratorsLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl AsyncLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl SourceLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    /// Finish the source-lowering sequence and recover the runtime AST.
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
        let source_check = include_str!("source_check.rs");
        let generators = include_str!("generators.rs");
        let async_lower = include_str!("async_lower.rs");
        let records = include_str!("records.rs");
        assert!(generators.contains("pub fn lower(mut checked: SourceCheckedModule)"));
        assert!(generators.contains("Result<GeneratorsLoweredModule, String>"));
        assert!(async_lower.contains("pub fn lower(checked: GeneratorsLoweredModule)"));
        assert!(async_lower.contains("mut checked: GeneratorsLoweredModule"));
        assert!(async_lower.contains("Result<AsyncLoweredModule, String>"));
        assert!(records.contains("pub fn lower(module: AsyncLoweredModule)"));
        assert!(records.contains("pub fn lower_lenient(module: AsyncLoweredModule)"));
        assert!(source_check.contains("pub(crate) fn into_module(self) -> Module"));
        assert_eq!(
            source_check
                .matches("\n    pub fn into_module(self) -> Module")
                .count(),
            1
        );
    }
}
