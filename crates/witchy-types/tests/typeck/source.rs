use std::collections::{HashSet, VecDeque};

use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

/// Check one entry source against exactly its reachable bundled std modules.
/// This is the target-neutral half of the root crate's browser-oriented
/// `resolve_std_only_checked`; checker integration tests do not need the
/// interpreter, lowerer, native runtime, Wasmtime, or AWS-LC.
pub(super) fn checked_module(
    source: &str,
) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let entry = witchy_syntax::parser::parse_module(source).map_err(|error| error.to_string())?;
    let mut sources = vec![("main".to_string(), source.as_bytes().to_vec())];
    let mut loaded = HashSet::from(["main".to_string()]);
    let mut queue = VecDeque::from([entry]);
    while let Some(module) = queue.pop_front() {
        for name in module.imports {
            if !loaded.insert(name.clone()) {
                continue;
            }
            let bundled = witchy_syntax::linker::bundled_source(&name)
                .ok_or_else(|| format!("unknown bundled module `{name}`"))?;
            let parsed = witchy_syntax::parser::parse_module(bundled)
                .map_err(|error| error.to_string())?;
            queue.push_back(parsed);
            sources.push((name, bundled.as_bytes().to_vec()));
        }
    }
    let application = PackageCoordinate::new(
        PackageSource::Workspace,
        "witchy/source",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/std",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(application, ["source", "main"])
            .map_err(|error| error.to_string())?,
    )];
    assignments.extend(
        witchy_syntax::linker::STD_MODULES
            .iter()
            .map(|name| {
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *name])
                    .map(|identity| ((*name).to_string(), identity))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let playground = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/glamour",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    assignments.extend(
        witchy_syntax::linker::PLAYGROUND_MODULES
            .iter()
            .map(|name| {
                ModuleLoadIdentity::new(playground.clone(), ["src", *name])
                    .map(|identity| ((*name).to_string(), identity))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .map_err(|error| error.to_string())?;
    let user_modules = HashSet::from(["main".to_string()]);
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources,
        "main",
        |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        &user_modules,
        owners,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn checked(source: &str) -> Result<(), String> {
    checked_module(source).map(drop)
}
