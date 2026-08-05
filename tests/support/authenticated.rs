use witchy_types::pipeline::{CheckedModule, PipelineError};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

pub(crate) fn checked_result(source: &str) -> Result<CheckedModule, PipelineError> {
    let module = witchy_syntax::parser::parse_module(source).expect("parse authenticated fixture");
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
    witchy_interp::pipeline::link_checked_authenticated(
        vec![("main".to_string(), module)],
        "main",
        owners,
    )
}
