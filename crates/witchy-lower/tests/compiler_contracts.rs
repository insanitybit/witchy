//! Small checked-source and lowering contracts kept separate from the large
//! raw-codegen harness so changing one does not relink the other.

fn no_expand(
    _name: &str,
    _module: &mut witchy_syntax::ast::Module,
    _siblings: &[(String, witchy_syntax::ast::Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    Ok(witchy_syntax::origin::OriginTable::default())
}

fn try_checked(
    source: &str,
) -> Result<witchy_types::pipeline::CheckedModule, witchy_types::pipeline::PipelineError> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        no_expand,
    )
}

#[path = "compiler_contracts/reference_carrier_repros.rs"]
mod reference_carrier_repros;
