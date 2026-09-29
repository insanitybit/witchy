use witchy_types::pipeline::{CheckedModule, PipelineError};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

/// Link exact source bytes through the runtime-neutral checked pipeline.
pub(crate) fn checked_source_result(source: &str) -> Result<CheckedModule, PipelineError> {
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/authenticated-integration-test",
        "0.1.0",
    )
    .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .expect("toolchain coordinate");
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).expect("main module owner"),
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
        (
            (*module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *module])
                .expect("std module owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated module owners");
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        witchy_syntax::linker::ComptimeExpander::with_sibling_request(
            witchy_types::pipeline::require_expansion_free, |_| false,
        ),
        &std::collections::HashSet::from(["main".to_string()]),
        owners,
    )
}
