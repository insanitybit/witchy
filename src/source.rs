//! Native source discovery, loading, linking, and expansion.

mod package_inputs;
mod precompiled_library;
pub(crate) use package_inputs::{
    capture_packaged_source, capture_project_source, capture_project_source_with_precompiled,
    project_declares_precompiled_dependencies, restore_packaged_artifact,
};
use package_inputs::{AmbientFiles, SourceFiles};


use witchy_syntax::{ast, format, parser};
#[cfg(test)]
use witchy_syntax::linker;
use witchy_interp::{comptime, pipeline};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedDependency {
    pub path: std::path::PathBuf,
    pub owner: ModuleLoadIdentity,
}

/// A separately compiled dependency already verified against its signed Coven
/// manifest. It has no source path: its only linker presence is the sealed
/// declaration environment recovered from the authenticated sidecar.
#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedPrecompiledDependency {
    pub library: precompiled_library::PrecompiledLibrary,
}

pub(crate) struct PrecompiledCheckedModule {
    pub checked: pipeline::CheckedModule,
    pub entry: String,
    pub external_libraries: Vec<witchy_runtime::runtime::AuthenticatedExternalLibrary>,
}

#[derive(Debug)]
pub(crate) enum TestFileError {
    Load(String),
    Pipeline(pipeline::PipelineError),
}

impl TestFileError {
    pub(crate) fn is_discovery_failure(&self) -> bool {
        matches!(
            self,
            Self::Load(_)
                | Self::Pipeline(pipeline::PipelineError::Link(_))
        )
    }
}

impl std::fmt::Display for TestFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(error) => f.write_str(error),
            Self::Pipeline(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

/// The current directory's project entry source file (`src/<module>.witchy`,
/// where `<module>` is the manifest's rune name with `/`-prefixes stripped and
/// `-` mapped to `_`), if we're inside a project. Lets file-oriented commands
/// (`witchy caps`) default to the project entry. Reads the `name = "..."` line
/// from `witchy.toml` directly so no package-manager code is needed.
pub(crate) fn project_entry_file() -> Option<String> {
    let dir = std::path::Path::new(".");
    let toml = std::fs::read_to_string(dir.join("witchy.toml")).ok()?;
    let name = toml.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("name")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')).map(|s| s.to_string())
    })?;
    let module = name.rsplit('/').next().unwrap_or("").replace('-', "_");
    let path = dir.join("src").join(format!("{module}.witchy"));
    path.exists().then(|| path.display().to_string())
}

/// Source for a bundled standard-library module, shipped with the compiler so
/// `import list` works without a local file. Bundled module names are reserved.
pub(crate) fn bundled_module(name: &str) -> Option<&'static str> {
    witchy_syntax::linker::bundled_source(name)
}

fn source_bundle_needs_legacy_expansion(sources: &[(String, Vec<u8>)]) -> bool {
    sources.iter().any(|(_, bytes)| {
        let parsed = std::str::from_utf8(bytes).ok().and_then(|source| parser::parse_module(source).ok());
        parsed.as_ref().is_none_or(module_needs_legacy_expansion)
    })
}

/// Parse and link a source file. Non-std imports resolve from sibling
/// `<name>.witchy` files; reserved std names resolve from the bundled source.
/// Returns the linked module and entry stem.
#[cfg(test)]
pub(crate) fn link_file(path: &str) -> Result<(ast::Module, String), String> {
    link_file_with_deps_mode(
        path,
        &std::collections::HashMap::new(),
        linker::LinkMode::Production,
    )
}

/// Authenticate raw source ownership in THIS process, then derive the only
/// supported production build recipe. No caller-supplied AST or digest enters.
pub(crate) fn link_build_file(path: &str) -> Result<witchy_types::pipeline::CheckedBuildRecipe, String> {
    let entry_path = std::path::Path::new(path);
    let stem = entry_path.file_stem().and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?;
    let owner = workspace_module_owner(entry_path, stem)?;
    let (sources, entry, user_modules, owners) = load_file_modules_authenticated(
        path, &HashMap::new(), owner, &HashMap::new(),
    )?;
    let selected = sources.iter().find(|(name, _)| name == &entry)
        .ok_or_else(|| format!("loaded build source lacks entry `{entry}`"))?;
    let source = std::str::from_utf8(&selected.1)
        .map_err(|error| format!("{entry}: source is not UTF-8: {error}"))?;
    let module = parser::parse_module(source).map_err(|error| format!("{entry}: {error}"))?;
    if module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))) {
        return witchy::canonical_comptime::check_build_recipe_source_bundle(
            sources, &entry, user_modules, owners, witchy::canonical_comptime::source_limits(),
        );
    }
    if let Some(producer) = single_imported_block_producer(&sources, &entry, &user_modules)? {
        return witchy::canonical_comptime::check_build_recipe_source_bundle_with_imported_blocks(
            sources, &entry, &producer, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        );
    }
    let expand = if source_bundle_needs_legacy_expansion(&sources) {
        comptime::EXPANDER
    } else {
        comptime::EXPANSION_FREE_USER_EXPANDER
    };
    witchy_types::pipeline::check_build_recipe_from_source_bundle(
        sources, &entry, expand, &user_modules, owners,
    ).map_err(|error| error.to_string())
}

pub(crate) fn link_test_file(
    path: &str,
) -> Result<(witchy_types::pipeline::CheckedTestSuite, String), TestFileError> {
    let entry_path = std::path::Path::new(path);
    let stem = entry_path.file_stem().and_then(|stem| stem.to_str())
        .ok_or_else(|| TestFileError::Load(format!("invalid file name: {path}")))?;
    let owner = workspace_module_owner(entry_path, stem).map_err(TestFileError::Load)?;
    let (sources, entry_stem, user_modules, owners) = load_file_modules_authenticated(
        path, &HashMap::new(), owner, &HashMap::new(),
    ).map_err(TestFileError::Load)?;
    let selected = sources.iter().find(|(name, _)| name == &entry_stem)
        .ok_or_else(|| TestFileError::Load(format!("loaded test source lacks entry `{entry_stem}`")))?;
    let source = std::str::from_utf8(&selected.1)
        .map_err(|error| TestFileError::Load(format!("{entry_stem}: source is not UTF-8: {error}")))?;
    let module = parser::parse_module(source)
        .map_err(|error| TestFileError::Load(format!("{entry_stem}: {error}")))?;
    if module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))) {
        let checked = witchy::canonical_comptime::check_test_suite_source_bundle(
            sources, &entry_stem, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        ).map_err(|message| TestFileError::Pipeline(pipeline::PipelineError::Type(
            witchy_types::typeck::TypeError { message },
        )))?;
        return Ok((checked, entry_stem));
    }
    if let Some(producer) = single_imported_block_producer(&sources, &entry_stem, &user_modules)
        .map_err(TestFileError::Load)? {
        let checked = witchy::canonical_comptime::check_test_suite_source_bundle_with_imported_blocks(
            sources, &entry_stem, &producer, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        ).map_err(|message| TestFileError::Pipeline(pipeline::PipelineError::Type(
            witchy_types::typeck::TypeError { message },
        )))?;
        return Ok((checked, entry_stem));
    }
    let checked = pipeline::link_checked_test_suite_from_source_bundle(
        sources, &entry_stem, &user_modules, owners,
    ).map_err(TestFileError::Pipeline)?;
    Ok((checked, entry_stem))
}

/// Like `link_file`, but resolves named imports from an explicit dependency map
/// (`import X` → `deps["X"]`) before the sibling-`<name>.witchy` / bundled-std
/// fallback. This is the hook the witchy CLI front-end uses to hand the compiler
/// resolved coven-dependency sources via `witchy compile <entry> --dep name=path`
/// (rfcs/0004-self-hosted-cli.md §4).
#[cfg(test)]
pub(crate) fn link_file_with_deps(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<(ast::Module, String), String> {
    link_file_with_deps_mode(path, deps, linker::LinkMode::Production)
}

pub(crate) fn link_file_checked(path: &str) -> Result<(pipeline::CheckedModule, String), String> {
    link_file_checked_with_deps(path, &std::collections::HashMap::new())
}

/// Build the compiler-owned static-evaluation recipe from the exact source
/// bundle the loader authenticated.  The recipe rewrites Glamour's sealed
/// static constructors before resolver/type/fact sealing; it is deliberately
/// separate from ordinary `link_file_checked` until the admitted value ABI is
/// ready to consume it.
pub(crate) fn link_static_evaluation_file(
    path: &str,
) -> Result<(pipeline::CheckedStaticEvaluationRecipe, String), String> {
    let entry_path = std::path::Path::new(path);
    let entry_stem = entry_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?;
    let entry_owner = workspace_module_owner(entry_path, entry_stem)?;
    let (sources, entry, user_modules, owners) = load_file_modules_authenticated(
        path,
        &HashMap::new(),
        entry_owner,
        &HashMap::new(),
    )?;
    let recipe = pipeline::link_checked_static_evaluation_from_source_bundle(
        sources,
        &entry,
        &user_modules,
        owners,
    )
    .map_err(|error| error.to_string())?;
    Ok((recipe, entry))
}

pub(crate) fn link_file_checked_with_deps(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<(pipeline::CheckedModule, String), String> {
    let entry_path = std::path::Path::new(path);
    let entry_stem = entry_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?;
    let entry_owner = workspace_module_owner(entry_path, entry_stem)?;
    let authenticated = deps
        .iter()
        .map(|(alias, dependency)| {
            Ok((
                alias.clone(),
                AuthenticatedDependency {
                    path: dependency.clone(),
                    owner: workspace_module_owner(dependency, alias)?,
                },
            ))
        })
        .collect::<Result<std::collections::HashMap<_, _>, String>>()?;
    link_file_checked_authenticated_with_deps(path, &authenticated, entry_owner)
}

pub(crate) fn link_file_checked_authenticated_with_deps(
    path: &str,
    deps: &std::collections::HashMap<String, AuthenticatedDependency>,
    entry_owner: ModuleLoadIdentity,
) -> Result<(pipeline::CheckedModule, String), String> {
    let paths = deps
        .iter()
        .map(|(name, dep)| (name.clone(), dep.path.clone()))
        .collect();
    let dep_owners = deps
        .iter()
        .map(|(name, dep)| (name.clone(), dep.owner.clone()))
        .collect();
    let (sources, entry_stem, user_modules, owners) =
        load_file_modules_authenticated(path, &paths, entry_owner, &dep_owners)?;
    let checked = check_loaded_source_bundle(sources, &entry_stem, path, user_modules, owners)?;
    Ok((checked, entry_stem))
}

fn single_imported_block_producer(
    sources: &[(String, Vec<u8>)], entry: &str, user_modules: &UserModuleSet,
) -> Result<Option<String>, String> {
    let mut producer = None;
    for (name, bytes) in sources {
        if name == entry || !user_modules.contains(name) { continue; }
        let source = std::str::from_utf8(bytes)
            .map_err(|error| format!("{name}: source is not UTF-8: {error}"))?;
        let module = parser::parse_module(source).map_err(|error| format!("{name}: {error}"))?;
        if module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))) {
            if producer.replace(name.clone()).is_some() {
                return Ok(None); // multi-producer graphs need ordered sessions
            }
        }
    }
    Ok(producer)
}

/// Original blocks in the entry or its sole imported producer use the
/// admitted compiler session. Other source graphs retain the existing path.
fn check_loaded_source_bundle(
    sources: Vec<(String, Vec<u8>)>, entry: &str, diagnostic_path: &str,
    user_modules: UserModuleSet, owners: AuthenticatedModuleOwners,
) -> Result<pipeline::CheckedModule, String> {
    let selected = sources.iter().find(|(name, _)| name == entry)
        .ok_or_else(|| format!("loaded source lacks entry `{entry}`"))?;
    let source = std::str::from_utf8(&selected.1)
        .map_err(|error| format!("{entry}: source is not UTF-8: {error}"))?;
    let module = parser::parse_module(source).map_err(|error| format!("{entry}: {error}"))?;
    if module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))) {
        return witchy::canonical_comptime::check_source_bundle(
            sources, entry, user_modules, owners, witchy::canonical_comptime::source_limits(),
        );
    }
    if let Some(producer) = single_imported_block_producer(&sources, entry, &user_modules)? {
        return witchy::canonical_comptime::check_source_bundle_with_imported_blocks(
            sources, entry, &producer, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        );
    }
    let needs_legacy_expansion = sources.iter().any(|(name, bytes)| {
        if name == entry { return module_needs_legacy_expansion(&module); }
        let parsed = std::str::from_utf8(bytes).ok().and_then(|source| parser::parse_module(source).ok());
        parsed.as_ref().is_none_or(module_needs_legacy_expansion)
    });
    let expand = if needs_legacy_expansion {
        comptime::EXPANDER
    } else {
        comptime::EXPANSION_FREE_USER_EXPANDER
    };
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources, entry, expand, &user_modules, owners,
    ).map_err(|error| match error {
        pipeline::PipelineError::Ownership(error) => format!("{diagnostic_path}: {error}"),
        pipeline::PipelineError::Source(error) => format!("{diagnostic_path}: {error}"),
        pipeline::PipelineError::Link(error) => error.to_string(),
        pipeline::PipelineError::Type(error) => format!("{diagnostic_path}: {error}"),
    })
}

/// Source inspection uses the entry module generated by the same checked
/// source bundle. Return the original entry text for documentation comments.
/// Selected original blocks execute through the admitted compiler session;
/// the temporary legacy path remains for other expansion.
pub(crate) fn checked_expanded_entry_source(path: &str) -> Result<(String, ast::Module), String> {
    let entry_path = std::path::Path::new(path);
    let stem = entry_path.file_stem().and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?;
    let owner = workspace_module_owner(entry_path, stem)?;
    let (sources, entry, user_modules, owners) = load_file_modules_authenticated(
        path, &HashMap::new(), owner, &HashMap::new(),
    )?;
    let selected = sources.iter().find(|(name, _)| name == &entry)
        .ok_or_else(|| format!("loaded source lacks entry `{entry}`"))?;
    let source = std::str::from_utf8(&selected.1)
        .map_err(|error| format!("{entry}: source is not UTF-8: {error}"))?;
    let original = source.to_owned();
    let mut module = parser::parse_module(source).map_err(|error| format!("{entry}: {error}"))?;
    if module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))) {
        let (_, expanded) = witchy::canonical_comptime::check_and_expand_source_bundle(
            sources, &entry, user_modules, owners, witchy::canonical_comptime::source_limits(),
        )?;
        return Ok((original, expanded));
    }
    if let Some(producer) = single_imported_block_producer(&sources, &entry, &user_modules)? {
        let (_, expanded) = witchy::canonical_comptime::check_and_expand_source_bundle_with_imported_blocks(
            sources, &entry, &producer, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        )?;
        return Ok((original, expanded));
    }
    let _checked = check_loaded_source_bundle(sources, &entry, path, user_modules, owners)?;
    Ok((original, module))
}

/// Production loader-to-pipeline seam for verified precompiled libraries.
/// Neither raw Wasm nor source declarations are accepted here: callers hand
/// over only `PrecompiledLibrary::verify` products from the signed package
/// resolver, and this function carries the paired runtime bytes onward.
pub(crate) fn link_file_checked_authenticated_with_precompiled_deps(
    path: &str,
    deps: &std::collections::HashMap<String, AuthenticatedDependency>,
    precompiled: &std::collections::HashMap<String, AuthenticatedPrecompiledDependency>,
    entry_owner: ModuleLoadIdentity,
) -> Result<PrecompiledCheckedModule, String> {
    let paths = deps.iter().map(|(name, dep)| (name.clone(), dep.path.clone())).collect();
    let dep_owners = deps.iter().map(|(name, dep)| (name.clone(), dep.owner.clone())).collect();
    let precompiled_owners = precompiled.iter().map(|(name, dep)| (name.clone(), dep.library.owner().clone())).collect();
    let (sources, entry, user_modules, owners) = load_file_modules_authenticated_with_precompiled(
        path, &paths, entry_owner, &dep_owners, &precompiled_owners,
    )?;
    checked_precompiled_source_bundle(sources, entry, user_modules, owners, precompiled)
}

pub(crate) fn checked_precompiled_source_bundle(
    sources: Vec<(String, Vec<u8>)>,
    entry: String,
    user_modules: UserModuleSet,
    owners: AuthenticatedModuleOwners,
    precompiled: &std::collections::HashMap<String, AuthenticatedPrecompiledDependency>,
) -> Result<PrecompiledCheckedModule, String> {
    let mut declarations = Vec::with_capacity(precompiled.len());
    let mut external_libraries = Vec::with_capacity(precompiled.len());
    for (alias, dependency) in precompiled {
        if !user_modules.contains(alias) {
            // Match source dependencies: a resolver may carry a larger locked
            // closure, but only an imported module becomes a source/link input
            // or runtime library. This also keeps an unused artifact from
            // changing the checked-source witness or launch requirements.
            continue;
        }
        declarations.push(dependency.library.declaration_library(alias.clone())?);
        external_libraries.push(dependency.library.runtime_library(alias.clone())?);
    }
    let environment = witchy_types::precompiled_declarations::PrecompiledDeclarationEnvironment::new(declarations)?;
    let checked = if precompiled.is_empty() {
        check_loaded_source_bundle(sources, &entry, &entry, user_modules, owners)?
    } else {
        witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_precompiled_declarations(
            sources, &entry, comptime::EXPANDER, &user_modules, owners, environment,
        ).map_err(|error| error.to_string())?
    };
    Ok(PrecompiledCheckedModule { checked, entry, external_libraries })
}

#[cfg(test)]
fn link_file_with_deps_mode(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
    mode: linker::LinkMode,
) -> Result<(ast::Module, String), String> {
    let (modules, entry_stem, user_modules) = load_file_modules(path, deps)?;
    let linked = pipeline::link_with_user_modules_with_mode(modules, &entry_stem, &user_modules, mode)
        .map_err(|e| e.to_string())?;
    Ok((linked, entry_stem))
}

type SourceModules = Vec<(String, ast::Module)>;
type UserModuleSet = std::collections::HashSet<String>;
type LoadedFileModules = (SourceModules, String, UserModuleSet);

fn load_file_modules(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<LoadedFileModules, String> {
    use std::collections::{HashSet, VecDeque};
    use std::path::{Path, PathBuf};

    let entry_path = Path::new(path);
    let dir: &Path = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entry_stem = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?
        .to_string();

    let mut modules: SourceModules = Vec::new();
    let mut loaded: HashSet<String> = HashSet::new();
    let mut user_modules: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, PathBuf)> = VecDeque::new();
    queue.push_back((entry_stem.clone(), entry_path.to_path_buf()));

    while let Some((name, p)) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue; // already loaded (cycle-safe)
        }
        // Read a local source when present; the linker rejects it if its module
        // name is reserved by a non-identical bundled std module.
        let src = match std::fs::read_to_string(&p) {
            Ok(s) => {
                // Repository checks read canonical std sources from disk. Source
                // identity, not the filesystem path, determines provenance: an
                // exact embedded module keeps std ownership, while any local
                // modification remains user code even under a std filename.
                let user_source = bundled_module(&name) != Some(s.as_str());
                if user_source {
                    user_modules.insert(name.clone());
                }
                s
            }
            Err(e) => match bundled_module(&name) {
                Some(s) => s.to_string(),
                None => {
                    // A misspelled `import` of a std module gets a suggestion.
                    let hint = if name != entry_stem {
                        witchy_syntax::linker::closest_std_module(&name)
                            .map(|m| format!(" — did you mean `import {m}`?"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    return Err(format!("cannot read `{}`: {e}{hint}", p.display()));
                }
            },
        };
        let module = parse_module_cached(&p.canonicalize().unwrap_or_else(|_| p.clone()), &src)
            .map_err(|e| format!("{name}: {e}"))?;
        for imp in &module.imports {
            if !loaded.contains(imp) {
                let dep_path = deps
                    .get(imp)
                    .cloned()
                    .unwrap_or_else(|| dir.join(format!("{imp}.witchy")));
                queue.push_back((imp.clone(), dep_path));
            }
        }
        modules.push((name, module));
    }

    Ok((modules, entry_stem, user_modules))
}

fn load_file_modules_authenticated(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
    entry_owner: ModuleLoadIdentity,
    dep_owners: &std::collections::HashMap<String, ModuleLoadIdentity>,
) -> Result<(Vec<(String, Vec<u8>)>, String, UserModuleSet, AuthenticatedModuleOwners), String> {
    load_file_modules_authenticated_with_precompiled(
        path, deps, entry_owner, dep_owners, &HashMap::new(),
    )
}

fn load_file_modules_authenticated_with_precompiled(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
    entry_owner: ModuleLoadIdentity,
    dep_owners: &std::collections::HashMap<String, ModuleLoadIdentity>,
    precompiled_owners: &std::collections::HashMap<String, ModuleLoadIdentity>,
) -> Result<(Vec<(String, Vec<u8>)>, String, UserModuleSet, AuthenticatedModuleOwners), String> {
    load_file_modules_with_files(path, deps, entry_owner, dep_owners, precompiled_owners, &AmbientFiles)
}

fn load_file_modules_with_files(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
    entry_owner: ModuleLoadIdentity,
    dep_owners: &std::collections::HashMap<String, ModuleLoadIdentity>,
    precompiled_owners: &std::collections::HashMap<String, ModuleLoadIdentity>,
    files: &dyn SourceFiles,
) -> Result<(Vec<(String, Vec<u8>)>, String, UserModuleSet, AuthenticatedModuleOwners), String> {
    use std::collections::{HashSet, VecDeque};
    use std::path::{Path, PathBuf};

    let entry_path = Path::new(path);
    let entry_dir = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entry_stem = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?
        .to_string();

    for name in deps.keys() {
        if !dep_owners.contains_key(name) {
            return Err(format!(
                "authenticated dependency `{name}` is missing loader ownership"
            ));
        }
    }
    if let Some(name) = precompiled_owners.keys().find(|name| deps.contains_key(*name)) {
        return Err(format!("dependency `{name}` cannot be both source and precompiled"));
    }
    if let Some(name) = precompiled_owners.keys().find(|name| {
        witchy_syntax::linker::STD_MODULES.contains(&name.as_str())
            || witchy_syntax::linker::PLAYGROUND_MODULES.contains(&name.as_str())
    }) {
        return Err(format!("precompiled dependency `{name}` conflicts with a reserved toolchain module"));
    }
    if let Some(name) = dep_owners.keys().find(|name| !deps.contains_key(*name)) {
        return Err(format!(
            "loader ownership was supplied for unknown dependency `{name}`"
        ));
    }

    let mut modules = Vec::new();
    let mut loaded = HashSet::new();
    let mut user_modules = HashSet::new();
    let mut assignments = Vec::new();
    let mut queue: VecDeque<(String, PathBuf, ModuleLoadIdentity)> = VecDeque::new();
    queue.push_back((entry_stem.clone(), entry_path.to_path_buf(), entry_owner));

    while let Some((name, module_path, proposed_owner)) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        let (source, owner) = match files.read(&module_path)? {
            Some(source) => {
                files.authenticate_module(&module_path, &proposed_owner, &source)?;
                if bundled_module(&name) == Some(source.as_str()) {
                    (source, bundled_module_owner(&name)?)
                } else {
                    if files.closed_package() && bundled_module(&name).is_some() {
                        return Err(format!("packaged source cannot replace bundled module `{name}`"));
                    }
                    user_modules.insert(name.clone());
                    (source, proposed_owner)
                }
            }
            None => match bundled_module(&name) {
                Some(source) => (source.to_string(), bundled_module_owner(&name)?),
                None => {
                    let hint = if name != entry_stem {
                        witchy_syntax::linker::closest_std_module(&name)
                            .map(|module| format!(" — did you mean `import {module}`?"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    return Err(format!(
                        "cannot read `{}`: source is missing{hint}",
                        module_path.display()
                    ));
                }
            },
        };
        assignments.push((name.clone(), owner.clone()));
        let module = parse_module_cached(&module_path, &source)
            .map_err(|error| format!("{name}: {error}"))?;
        for import in &module.imports {
            if loaded.contains(import) {
                continue;
            }
            if let Some(dep_path) = deps.get(import) {
                let dep_owner = dep_owners.get(import).cloned().ok_or_else(|| {
                    format!("authenticated dependency `{import}` is missing loader ownership")
                })?;
                queue.push_back((import.clone(), dep_path.clone(), dep_owner));
                continue;
            }
            if let Some(owner) = precompiled_owners.get(import) {
                // The sealed declaration module is injected by the checked
                // pipeline, not read from a sibling path or SourceInputs.
                // Unlike source dependencies it never enters `queue`, so it
                // must join `loaded` here. This records exactly one loader
                // assignment even if several source modules import it.
                if loaded.insert(import.clone()) {
                    user_modules.insert(import.clone());
                    assignments.push((import.clone(), owner.clone()));
                }
                continue;
            }
            if bundled_module(import).is_some() {
                queue.push_back((
                    import.clone(),
                    entry_dir.join(format!("{import}.witchy")),
                    bundled_module_owner(import)?,
                ));
                continue;
            }
            let sibling = module_path
                .parent()
                .unwrap_or(entry_dir)
                .join(format!("{import}.witchy"));
            queue.push_back((
                import.clone(),
                sibling,
                sibling_module_owner(&owner, import)?,
            ));
        }
        modules.push((name, source.into_bytes()));
    }

    for module in witchy_syntax::linker::STD_MODULES {
        assignments.push((module.to_string(), toolchain_module_owner(module)?));
    }
    for module in witchy_syntax::linker::PLAYGROUND_MODULES {
        assignments.push((module.to_string(), playground_module_owner(module)?));
    }
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .map_err(|error| error.to_string())?;
    Ok((modules, entry_stem, user_modules, owners))
}

fn toolchain_module_owner(module: &str) -> Result<ModuleLoadIdentity, String> {
    let package = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/std",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    ModuleLoadIdentity::new(package, ["std", module]).map_err(|error| error.to_string())
}

fn playground_module_owner(module: &str) -> Result<ModuleLoadIdentity, String> {
    let package = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/glamour",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    ModuleLoadIdentity::new(package, ["src", module]).map_err(|error| error.to_string())
}

fn bundled_module_owner(module: &str) -> Result<ModuleLoadIdentity, String> {
    if witchy_syntax::linker::PLAYGROUND_MODULES.contains(&module) {
        playground_module_owner(module)
    } else {
        toolchain_module_owner(module)
    }
}

fn workspace_module_owner(
    source_path: &std::path::Path,
    fallback_name: &str,
) -> Result<ModuleLoadIdentity, String> {
    workspace_module_owner_with_files(source_path, fallback_name, &AmbientFiles)
}

fn workspace_module_owner_with_files(
    source_path: &std::path::Path, fallback_name: &str, files: &dyn SourceFiles,
) -> Result<ModuleLoadIdentity, String> {
    let mut manifest = None;
    for directory in source_path.parent().into_iter().flat_map(std::path::Path::ancestors) {
        let path = directory.join("witchy.toml");
        if let Some(source) = files.read(&path)? {
            manifest = Some((directory, path, source));
            break;
        }
    }
    let (package_name, package_version, module_path) = if let Some((root, manifest, source)) = manifest {
        let mut in_rune = false;
        let mut name = None;
        let mut version = None;
        for line in source.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_rune = line == "[rune]";
                continue;
            }
            if !in_rune {
                continue;
            }
            let value = |key: &str| {
                line.strip_prefix(key)
                    .and_then(|rest| rest.trim_start().strip_prefix('='))
                    .map(str::trim)
                    .and_then(|value| value.strip_prefix('"'))
                    .and_then(|value| value.strip_suffix('"'))
                    .map(str::to_string)
            };
            name = name.or_else(|| value("name"));
            version = version.or_else(|| value("version"));
        }
        let name = name.ok_or_else(|| {
            format!("`{}` has no [rune] name", manifest.display())
        })?;
        let version = version.unwrap_or_else(|| "0.0.0".to_string());
        let relative = source_path.strip_prefix(root).unwrap_or(source_path);
        let mut module_path = relative
            .parent()
            .into_iter()
            .flat_map(std::path::Path::components)
            .filter_map(|component| match component {
                std::path::Component::Normal(component) => component.to_str().map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>();
        module_path.push(
            relative
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(fallback_name)
                .to_string(),
        );
        (name, version, module_path)
    } else {
        (
            format!("local/{fallback_name}"),
            env!("CARGO_PKG_VERSION").to_string(),
            vec!["src".to_string(), fallback_name.to_string()],
        )
    };
    let package = PackageCoordinate::new(
        PackageSource::Workspace,
        package_name,
        package_version,
    )
    .map_err(|error| error.to_string())?;
    ModuleLoadIdentity::new(package, module_path).map_err(|error| error.to_string())
}

fn sibling_module_owner(
    owner: &ModuleLoadIdentity,
    module: &str,
) -> Result<ModuleLoadIdentity, String> {
    let mut path = owner.module_path().to_vec();
    path.pop();
    path.push(module.to_string());
    ModuleLoadIdentity::new(owner.package().clone(), path).map_err(|error| error.to_string())
}

pub(crate) fn expand_file_source(path: &str) -> Result<String, String> {
    let entry_path = std::path::Path::new(path);
    let entry_stem = entry_path.file_stem().and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?;
    let owner = workspace_module_owner(entry_path, entry_stem)?;
    let (sources, entry_stem, user_modules, owners) = load_file_modules_authenticated(
        path, &HashMap::new(), owner, &HashMap::new(),
    )?;
    let mut modules = sources.iter().map(|(name, source)| {
        let source = std::str::from_utf8(source)
            .map_err(|error| format!("{name}: source is not UTF-8: {error}"))?;
        let module = parser::parse_module(source).map_err(|error| format!("{name}: {error}"))?;
        Ok((name.clone(), module))
    }).collect::<Result<SourceModules, String>>()?;
    if modules.iter().find(|(name, _)| name == &entry_stem)
        .is_some_and(|(_, module)| module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_)))) {
        let expanded = witchy::canonical_comptime::expand_source_bundle(
            sources, &entry_stem, user_modules, owners, witchy::canonical_comptime::source_limits(),
        )?;
        return Ok(format::module(&expanded, &[]));
    }
    if let Some(producer) = single_imported_block_producer(&sources, &entry_stem, &user_modules)? {
        let (_, expanded) = witchy::canonical_comptime::check_and_expand_source_bundle_with_imported_blocks(
            sources, &entry_stem, &producer, user_modules, owners,
            witchy::canonical_comptime::source_limits(),
        )?;
        return Ok(format::module(&expanded, &[]));
    }
    // The compatibility path also schedules derive-generated comptime work.
    // Plain source can be
    // formatted directly without invoking the source tree walker.
    let needs_legacy_expansion = modules.iter().any(|(_, module)| module_needs_legacy_expansion(module));
    if !needs_legacy_expansion {
        let (_, module) = modules.iter().find(|(name, _)| name == &entry_stem)
            .ok_or_else(|| format!("internal error: entry module `{entry_stem}` was not loaded"))?;
        return Ok(format::module(module, &[]));
    }
    let names: Vec<String> = modules.iter().map(|(name, _)| name.clone()).collect();
    for (i, name) in names.iter().enumerate() {
        let siblings: Vec<(String, ast::Module)> = modules
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, module)| module.clone())
            .collect();
        comptime::expand_compile_time(name, &mut modules[i].1, &siblings)
            .map_err(|e| format!("{path}: {e}"))?;
    }
    let Some((_, module)) = modules.into_iter().find(|(name, _)| name == &entry_stem) else {
        return Err(format!("internal error: entry module `{entry_stem}` was not loaded"));
    };
    Ok(format::module(&module, &[]))
}

fn module_needs_legacy_expansion(module: &ast::Module) -> bool {
    !module.compiler_item_syntax.is_empty() || !module.compiler_expr_syntax.is_empty()
        || !module.compiler_type_syntax.is_empty() || !module.compiler_pattern_syntax.is_empty()
        || !module.compiler_stmt_syntax.is_empty() || !module.compiler_block_syntax.is_empty()
        || module.items.iter().any(|item| matches!(item, ast::Item::Comptime(_))
        || matches!(item, ast::Item::Function(function) if function.comptime_only)
        || matches!(item, ast::Item::Type(definition) if !definition.derives.is_empty()))
        || witchy_syntax::ast::visit::visit_module_exprs(module, &mut |expr| {
            if matches!(expr, ast::Expr::TaggedLit { .. }) { Err(()) } else { Ok(()) }
        }).is_err()
}

#[derive(Clone)]
struct CachedParsedSource {
    signature: u64,
    module: ast::Module,
}

fn module_cache() -> &'static Mutex<HashMap<PathBuf, CachedParsedSource>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedParsedSource>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn signature_for_source(source: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

fn parse_module_cached(path: &PathBuf, source: &str) -> Result<ast::Module, String> {
    let signature = signature_for_source(source);
    let mut cache = module_cache().lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(entry) = cache.get(path) {
        if entry.signature == signature {
            return Ok(entry.module.clone());
        }
    }
    let module = parser::parse_module(source).map_err(|e| e.to_string())?;
    cache.insert(
        path.to_path_buf(),
        CachedParsedSource {
            signature,
            module: module.clone(),
        },
    );
    Ok(module)
}

/// Whether a linked module contains a runnable `main`. Library-only files are
/// valid `witchy check` inputs, but there is no program artifact to compile.
pub(crate) fn linked_has_main(linked: &ast::Module) -> bool {
    linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
}

#[cfg(test)]
#[path = "../tests/support/precompiled_registry.rs"]
mod precompiled_registry_fixture;

#[cfg(test)]
mod tests {
    use super::{
        capture_project_source_with_precompiled, expand_file_source, link_file_checked_authenticated_with_deps,
        link_file_checked_authenticated_with_precompiled_deps,
        AuthenticatedDependency, AuthenticatedPrecompiledDependency, TestFileError,
    };
    use witchy_types::runtime_type::{
        DeclarationKind, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };

    use super::precompiled_registry_fixture::{
        PrecompiledRegistryFixture, scalar_map_wasm, sha256_hex, signed_scalar_sidecar,
        unique_dir,
    };

    fn external_owner_named(name: &str) -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(
                PackageSource::Registry("https://coven.example".into()),
                name,
                "1.0.0",
            )
            .unwrap(),
            ["src", "math"],
        )
        .unwrap()
    }

    fn external_owner() -> ModuleLoadIdentity { external_owner_named("acme/math") }

    #[test]
    fn checked_source_schedules_derive_before_expansion_free_dispatch() {
        let dir = unique_dir("checked_derive_dispatch");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.witchy");
        std::fs::write(&entry, "import reflect\nsealed type Foo derive(Reflect):\n    Foo(Int)\nfn main(console: Console):\n    console.print(reflect.debug(Foo(3)))\n").unwrap();
        let result = super::link_file_checked(entry.to_str().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
        result.expect("derive expansion remains available in checked source");
    }

    fn load_fixture(fixture: &PrecompiledRegistryFixture) -> Result<super::PrecompiledCheckedModule, String> {
        capture_project_source_with_precompiled(
            fixture.root.join("src/main.witchy").to_str().unwrap(),
            &std::collections::HashMap::new(),
            Some(fixture.root.join("witchy.toml").to_str().unwrap()),
            &fixture.entry_claim(),
            &std::collections::HashMap::new(),
        )
    }

    #[test]
    fn locked_precompiled_project_resolves_and_launches_the_authenticated_external_library() {
        let fixture = PrecompiledRegistryFixture::new();
        let loaded = load_fixture(&fixture).expect("locked precompiled dependency resolves");
        assert_eq!(loaded.external_libraries.len(), 1);
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&loaded.checked)
            .expect_lowered("locked precompiled dependency lowers");
        assert_eq!(artifact.external_imports().len(), 1);
        let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted_with_external_libraries(
            &artifact,
            witchy_runtime::runtime::Capabilities {
                print: true, print_int: true, quiet: true, ..Default::default()
            },
            16,
            &loaded.external_libraries,
        ).expect("launch receives the exact library authenticated by the resolver");
        vm.run().unwrap();
        assert_eq!(vm.output(), ["15"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }

    #[test]
    fn locked_precompiled_project_refuses_tamper_missing_oversize_and_source_overlap() {
        let cases = ["manifest", "sidecar", "missing", "oversize", "v2", "wrong-kind", "version", "missing-version", "bad-version", "zero-size", "zero-sidecar-size", "non-registry", "lock-hash", "lock-kind", "lock-module", "lock-digest", "overlap"];
        for case in cases {
            let fixture = PrecompiledRegistryFixture::new();
            match case {
                "manifest" => std::fs::write(
                    fixture.root.join("vendor/external_math/coven-artifact.json"),
                    "{\"version\":3,\"artifacts\":{},\"libraries\":{}}",
                ).unwrap(),
                "sidecar" => std::fs::write(
                    fixture.root.join("vendor/external_math/artifacts/src/math.opt"), b"tampered",
                ).unwrap(),
                "missing" => std::fs::remove_file(
                    fixture.root.join("vendor/external_math/artifacts/src/math.wasm"),
                ).unwrap(),
                "oversize" => {
                    let (wasm, wasm_size, sidecar, _) = fixture.artifact_fields();
                    fixture.replace_signed_artifact_manifest(
                        serde_json::json!({"version": 3, "artifacts": {}, "libraries": {
                            "src/math": {"kind": "precompiled", "wasm_sha256": wasm.clone(), "wasm_size": wasm_size,
                                "opt_callable_export_sha256": sidecar.clone(), "opt_callable_export_size": 1024 * 1024 + 1},
                        }}),
                        format!("v3\nlibrary|src/math|precompiled|{wasm}|{wasm_size}|{sidecar}|{}", 1024 * 1024 + 1),
                    );
                }
                "v2" => {
                    let (wasm, _, sidecar, _) = fixture.artifact_fields();
                    fixture.replace_signed_artifact_manifest(
                        serde_json::json!({"version": 2, "artifacts": {}, "libraries": {
                            "src/math": {"wasm_sha256": wasm.clone(), "opt_callable_export_sha256": sidecar.clone()},
                        }}),
                        format!("v2\nlibrary|src/math|{wasm}|{sidecar}"),
                    );
                }
                "wrong-kind" => {
                    let (wasm, wasm_size, sidecar, sidecar_size) = fixture.artifact_fields();
                    fixture.replace_signed_artifact_manifest(
                        serde_json::json!({"version": 3, "artifacts": {}, "libraries": {
                            "src/math": {"kind": "application", "wasm_sha256": wasm.clone(), "wasm_size": wasm_size,
                                "opt_callable_export_sha256": sidecar.clone(), "opt_callable_export_size": sidecar_size},
                        }}),
                        format!("v3\nlibrary|src/math|application|{wasm}|{wasm_size}|{sidecar}|{sidecar_size}"),
                    );
                }
                "version" => std::fs::write(
                    fixture.root.join("witchy.toml"),
                    "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\nexternal_math={version=\"^2.0.0\",kind=\"precompiled\",module=\"src/math\"}\n",
                ).unwrap(),
                "missing-version" => std::fs::write(
                    fixture.root.join("witchy.toml"),
                    "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\nexternal_math={kind=\"precompiled\",module=\"src/math\"}\n",
                ).unwrap(),
                "bad-version" => std::fs::write(
                    fixture.root.join("witchy.toml"),
                    "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\nexternal_math={version=\"not-a-requirement\",kind=\"precompiled\",module=\"src/math\"}\n",
                ).unwrap(),
                "non-registry" => std::fs::write(
                    fixture.root.join("witchy.toml"),
                    "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\nexternal_math={version=\"1.0.0\",kind=\"precompiled\",module=\"src/math\",source=\"elsewhere\"}\n",
                ).unwrap(),
                "lock-hash" => fixture.add_spurious_lock_hash(),
                "zero-size" => {
                    let (wasm, _, sidecar, sidecar_size) = fixture.artifact_fields();
                    fixture.replace_signed_artifact_manifest(
                        serde_json::json!({"version": 3, "artifacts": {}, "libraries": {
                            "src/math": {"kind": "precompiled", "wasm_sha256": wasm.clone(), "wasm_size": 0,
                                "opt_callable_export_sha256": sidecar.clone(), "opt_callable_export_size": sidecar_size},
                        }}),
                        format!("v3\nlibrary|src/math|precompiled|{wasm}|0|{sidecar}|{sidecar_size}"),
                    );
                }
                "zero-sidecar-size" => {
                    let (wasm, wasm_size, sidecar, _) = fixture.artifact_fields();
                    fixture.replace_signed_artifact_manifest(
                        serde_json::json!({"version": 3, "artifacts": {}, "libraries": {
                            "src/math": {"kind": "precompiled", "wasm_sha256": wasm.clone(), "wasm_size": wasm_size,
                                "opt_callable_export_sha256": sidecar.clone(), "opt_callable_export_size": 0},
                        }}),
                        format!("v3\nlibrary|src/math|precompiled|{wasm}|{wasm_size}|{sidecar}|0"),
                    );
                }
                "lock-kind" => fixture.replace_lock_selection("source", "src/math", &fixture.signed_artifact_digest()),
                "lock-module" => fixture.replace_lock_selection("precompiled", "src/other", &fixture.signed_artifact_digest()),
                "lock-digest" => fixture.replace_lock_selection("precompiled", "src/math", "sha256:deadbeef"),
                "overlap" => {
                    let source = fixture.root.join("external_math.witchy");
                    std::fs::write(&source, "pub fn map(value: Int) -> Int:\n    value\n").unwrap();
                    let error = match capture_project_source_with_precompiled(
                        fixture.root.join("src/main.witchy").to_str().unwrap(),
                        &std::collections::HashMap::from([("external_math".into(), source)]),
                        Some(fixture.root.join("witchy.toml").to_str().unwrap()),
                        &fixture.entry_claim(),
                        &std::collections::HashMap::new(),
                    ) {
                        Ok(_) => panic!("one alias cannot select source and precompiled input"),
                        Err(error) => error,
                    };
                    assert!(error.contains("both source and precompiled"), "{error}");
                    continue;
                }
                _ => unreachable!(),
            }
            let error = match load_fixture(&fixture) {
                Ok(_) => panic!("tampered registry precompiled input must fail closed"),
                Err(error) => error,
            };
            let expected = match case {
                "manifest" => "precompiled artifact manifest differs from locked signed digest",
                "sidecar" => "precompiled registry export-signature sidecar differs from signed size",
                "missing" => "cannot inspect precompiled registry input",
                "oversize" => "artifact size `opt_callable_export_size=1048577` is outside its fixed signed limit",
                "v2" => "requires a v3 signed artifact manifest",
                "wrong-kind" => "library kind `application` is not supported",
                "version" => "lock version does not satisfy its manifest requirement",
                "missing-version" => "requires a non-empty registry version requirement",
                "bad-version" => "precompiled dependency has malformed version requirement",
                "non-registry" => "precompiled dependency `external_math` must select the Coven registry",
                "lock-hash" => "precompiled registry lock must not carry a source hash",
                "zero-size" => "artifact size `wasm_size=0` is outside its fixed signed limit",
                "zero-sidecar-size" => "artifact size `opt_callable_export_size=0` is outside its fixed signed limit",
                "lock-kind" => "registry lock kind does not select a precompiled dependency",
                "lock-module" => "registry lock module differs from its precompiled dependency declaration",
                "lock-digest" => "registry lock artifact digest differs from the signed record",
                _ => unreachable!(),
            };
            assert!(error.contains(expected), "{case}: expected `{expected}`, got `{error}`");
        }
    }

    #[cfg(unix)]
    #[test]
    fn locked_precompiled_project_rejects_vendor_artifact_symlinks() {
        let fixture = PrecompiledRegistryFixture::new();
        let wasm = fixture.root.join("vendor/external_math/artifacts/src/math.wasm");
        let replacement = fixture.root.join("replacement.wasm");
        std::fs::rename(&wasm, &replacement).unwrap();
        std::os::unix::fs::symlink(&replacement, &wasm).unwrap();
        let error = match load_fixture(&fixture) {
            Ok(_) => panic!("precompiled vendor artifacts must not cross symlinks"),
            Err(error) => error,
        };
        assert!(error.contains("must not cross a symlink"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn locked_precompiled_project_rejects_unsafe_alias_and_vendor_ancestor() {
        let fixture = PrecompiledRegistryFixture::new();
        std::fs::write(
            fixture.root.join("witchy.toml"),
            "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\n\"../external_math\"={version=\"1.0.0\",kind=\"precompiled\",module=\"src/math\"}\n",
        ).unwrap();
        let error = match load_fixture(&fixture) {
            Ok(_) => panic!("a path-shaped precompiled alias must fail before vendor access"),
            Err(error) => error,
        };
        assert!(error.contains("must be one safe path component"), "{error}");

        let fixture = PrecompiledRegistryFixture::new();
        let vendor = fixture.root.join("vendor");
        let replacement = fixture.root.join("vendor-real");
        std::fs::rename(&vendor, &replacement).unwrap();
        std::os::unix::fs::symlink(&replacement, &vendor).unwrap();
        let error = match load_fixture(&fixture) {
            Ok(_) => panic!("a precompiled vendor ancestor must not be a symlink"),
            Err(error) => error,
        };
        assert!(error.contains("must be a real directory"), "{error}");
    }

    fn wrongly_typed_scalar_map_wasm() -> Vec<u8> {
        use wasm_encoder::{
            CodeSection, ExportKind, ExportSection, Function, FunctionSection,
            Instruction, Module, TypeSection, ValType,
        };
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32], [ValType::I32]);
        module.section(&types);
        let mut functions = FunctionSection::new();
        functions.function(0);
        module.section(&functions);
        let mut exports = ExportSection::new();
        exports.export("map", ExportKind::Func, 0);
        module.section(&exports);
        let mut code = CodeSection::new();
        let mut body = Function::new(Vec::new());
        body.instruction(&Instruction::LocalGet(0));
        body.instruction(&Instruction::End);
        code.function(&body);
        module.section(&code);
        module.finish()
    }

    fn precompiled_scalar_fixture(wasm: Vec<u8>) -> super::PrecompiledCheckedModule {
        let dir = unique_dir("precompiled_scalar_negative");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("app.witchy");
        std::fs::write(
            &entry,
            "import external_math\n\nfn main(console: Console):\n    console.print(\"${external_math.map(5)}\")\n",
        )
        .unwrap();
        let owner = external_owner();
        let sidecar = signed_scalar_sidecar(owner.clone());
        let library = super::precompiled_library::PrecompiledLibrary::verify(
            owner,
            wasm.clone(),
            &sidecar,
            &sha256_hex(&wasm),
            &sha256_hex(&sidecar),
        )
        .unwrap();
        let app_owner = ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "example/app", "1.0.0").unwrap(),
            ["src", "app"],
        )
        .unwrap();
        let precompiled = std::collections::HashMap::from([(
            "external_math".to_owned(),
            AuthenticatedPrecompiledDependency { library },
        )]);
        let loaded = link_file_checked_authenticated_with_precompiled_deps(
            entry.to_str().unwrap(),
            &std::collections::HashMap::new(),
            &precompiled,
            app_owner,
        )
        .expect("signed scalar precompiled fixture must link");
        let _ = std::fs::remove_dir_all(&dir);
        loaded
    }

    #[test]
    fn authenticated_precompiled_scalar_calls_link_and_execute_direct_and_as_function_value() {
        for (mode, label) in [("", "normal-to-opt"), ("mode opt\n", "opt-to-opt")] {
            let dir = unique_dir(&format!("precompiled_scalar_call_{label}"));
            std::fs::create_dir_all(&dir).unwrap();
            let entry = dir.join("app.witchy");
            let app = format!("{mode}import external_math\n\n\
                              fn twice(f: fn(Int) -> Int, value: Int) -> Int:\n    f(f(value))\n\n\
                              fn main(console: Console):\n    console.print(\"${{external_math.map(5)}}\")\n    console.print(\"${{twice(external_math.map, 7)}}\")\n");
            std::fs::write(&entry, &app).unwrap();

            let wasm = scalar_map_wasm();
            let owner = external_owner();
            let sidecar = signed_scalar_sidecar(owner.clone());
            let library = super::precompiled_library::PrecompiledLibrary::verify(
                owner,
                wasm.clone(),
                &sidecar,
                &sha256_hex(&wasm),
                &sha256_hex(&sidecar),
            )
            .expect("fixture has a manifest-bound scalar export");
            let app_owner = ModuleLoadIdentity::new(
                PackageCoordinate::new(PackageSource::Workspace, "example/app", "1.0.0").unwrap(),
                ["src", "app"],
            )
            .unwrap();
            let precompiled = std::collections::HashMap::from([(
                "external_math".to_owned(),
                AuthenticatedPrecompiledDependency { library },
            )]);
            let loaded = link_file_checked_authenticated_with_precompiled_deps(
                entry.to_str().unwrap(),
                &std::collections::HashMap::new(),
                &precompiled,
                app_owner,
            )
            .unwrap_or_else(|error| panic!("{label} authenticated scalar import must link: {error}"));
            let artifact = witchy_lower::codegen::compile_checked_module_artifact(&loaded.checked)
                .expect_lowered("authenticated scalar import lowers to an external call");
            assert_eq!(artifact.external_imports().len(), 1, "{label}");
            let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
            let mut vm = runtime.spawn_admitted_with_external_libraries(
                &artifact,
                witchy_runtime::runtime::Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    ..Default::default()
                },
                16,
                &loaded.external_libraries,
            )
            .expect("runtime links the exact authenticated external instance");
            vm.run().unwrap();
            assert_eq!(vm.output(), ["15", "27"], "{label}");

            // Independent source semantics for the same exported behavior.
            // The precompiled sidecar deliberately carries only a checked
            // signature, not executable source, so this is a separate
            // authenticated-source oracle rather than interpreting a
            // synthetic declaration stub.
            let oracle = witchy_interp::interpreter::run_program(
                &[
                    ("external_math", "mode opt\npub fn map(value: Int) -> Int:\n    value + 10\n"),
                    ("app", app.as_str()),
                ],
                "app",
            )
            .expect("independent source oracle");
            assert_eq!(vm.output(), oracle, "{label}");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn authenticated_precompiled_runtime_rejects_missing_extra_and_wrongly_typed_exports() {
        let loaded = precompiled_scalar_fixture(scalar_map_wasm());
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&loaded.checked)
            .expect_lowered("authenticated scalar fixture lowers");
        let caps = witchy_runtime::runtime::Capabilities {
            print: true,
            print_int: true,
            quiet: true,
            ..Default::default()
        };
        let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
        let missing = match runtime.spawn_admitted_with_external_libraries(&artifact, caps.clone(), 16, &[]) {
            Ok(_) => panic!("an admitted external import needs its exact library"),
            Err(error) => error,
        };
        assert!(missing.to_string().contains("exactly match"), "{missing:#}");

        let extra_owner = external_owner_named("acme/extra");
        let extra_wasm = scalar_map_wasm();
        let extra_sidecar = signed_scalar_sidecar(extra_owner.clone());
        let extra = super::precompiled_library::PrecompiledLibrary::verify(
            extra_owner,
            extra_wasm.clone(),
            &extra_sidecar,
            &sha256_hex(&extra_wasm),
            &sha256_hex(&extra_sidecar),
        )
        .unwrap()
        .runtime_library("extra_math".into())
        .unwrap();
        let mut supplied = loaded.external_libraries.clone();
        supplied.push(extra);
        let extra = match runtime.spawn_admitted_with_external_libraries(&artifact, caps, 16, &supplied) {
            Ok(_) => panic!("an unused authenticated library is not ambient authority"),
            Err(error) => error,
        };
        assert!(extra.to_string().contains("exactly match"), "{extra:#}");

        let wrongly_typed = precompiled_scalar_fixture(wrongly_typed_scalar_map_wasm());
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&wrongly_typed.checked)
            .expect_lowered("the signed source declaration still lowers before runtime ABI binding");
        let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
        let error = match runtime.spawn_admitted_with_external_libraries(
                &artifact,
                witchy_runtime::runtime::Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    ..Default::default()
                },
                16,
                &wrongly_typed.external_libraries,
            ) {
            Ok(_) => panic!("physical Wasm export must match the sealed scalar signature"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("incompatible"), "{error:#}");
    }

    #[test]
    fn source_semantic_failures_are_not_discovery_skips() {
        let source_error = TestFileError::Pipeline(
            witchy_interp::pipeline::PipelineError::Source(
                witchy_syntax::source_check::SourceCheckError::new("unknown function"),
            ),
        );
        assert!(!source_error.is_discovery_failure());

        let link_error = TestFileError::Pipeline(
            witchy_interp::pipeline::PipelineError::Link(
                witchy_syntax::linker::LinkError {
                    location: None,
                    message: "missing imported module".into(),
                },
            ),
        );
        assert!(link_error.is_discovery_failure());
    }

    #[test]
    fn expand_file_prints_generated_items_without_comptime_blocks() {
        let dir = unique_dir("expand_comptime");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.witchy");
        std::fs::write(
            &file,
            "comptime:\n    \
             emit_item(quote item:\n        \
                 pub fn generated() -> Int:\n            \
                     42\n    \
             )\n\n\
             fn main(console: Console):\n    \
             console.print(\"${generated()}\")\n",
        )
        .unwrap();

        let expanded = expand_file_source(file.to_str().unwrap()).expect("expand source");
        assert!(!expanded.contains("comptime:"), "{expanded}");
        assert!(expanded.contains("pub fn generated() -> Int:\n    42\n"), "{expanded}");
        assert!(expanded.contains("fn main(console: Console):"), "{expanded}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_file_checks_ordered_original_blocks_before_display() {
        let dir = unique_dir("expand_admitted_comptime");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.witchy");
        std::fs::write(&file, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            40\n    )\ncomptime:\n    emit_item(quote item:\n        fn result() -> Int:\n            answer() + 2\n    )\nfn main(console: Console):\n    console.print(\"${result()}\")\n").unwrap();
        let expanded = expand_file_source(file.to_str().unwrap()).expect("admitted expansion");
        assert!(!expanded.contains("comptime:"), "{expanded}");
        assert!(expanded.contains("fn answer() -> Int:"), "{expanded}");
        assert!(expanded.contains("fn result() -> Int:"), "{expanded}");

        std::fs::write(&file, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            \"wrong\"\n    )\nfn main() -> Int:\n    0\n").unwrap();
        let error = expand_file_source(file.to_str().unwrap()).expect_err("invalid output must refuse");
        assert!(error.contains("Int") && error.contains("String"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn authenticated_file_loader_retains_package_coordinates() {
        let dir = unique_dir("authenticated_loader");
        let app_src = dir.join("app/src");
        let dep_src = dir.join("vendor/model/src");
        std::fs::create_dir_all(&app_src).unwrap();
        std::fs::create_dir_all(&dep_src).unwrap();
        let entry = app_src.join("app.witchy");
        let dependency = dep_src.join("model.witchy");
        std::fs::write(
            &entry,
            "import model\n\ntype Local:\n    Local\n\nfn accept(value: model.User) -> model.User:\n    value\n",
        )
        .unwrap();
        std::fs::write(&dependency, "type User:\n    User\n").unwrap();

        let app_package = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/app",
            "0.1.0",
        )
        .unwrap();
        let dep_package = PackageCoordinate::new(
            PackageSource::Registry("coven-root-key".into()),
            "acme/model",
            "1.2.3",
        )
        .unwrap();
        let app_owner =
            ModuleLoadIdentity::new(app_package.clone(), ["src", "app"]).unwrap();
        let dep_owner =
            ModuleLoadIdentity::new(dep_package.clone(), ["src", "model"]).unwrap();
        let deps = std::collections::HashMap::from([(
            "model".to_string(),
            AuthenticatedDependency { path: dependency, owner: dep_owner },
        )]);

        let (checked, _) = link_file_checked_authenticated_with_deps(
            entry.to_str().unwrap(),
            &deps,
            app_owner,
        )
        .expect("authenticated checked link");
        let catalog = checked
            .runtime_declaration_catalog()
            .expect("runtime declaration catalog");
        let local = catalog
            .resolve("app.Local", DeclarationKind::Type)
            .expect("application declaration");
        let user = catalog
            .resolve("model.User", DeclarationKind::Type)
            .expect("dependency declaration");
        assert_eq!(local.package(), &app_package);
        assert_eq!(local.module(), &["src".to_string(), "app".to_string()]);
        assert_eq!(user.package(), &dep_package);
        assert_eq!(user.module(), &["src".to_string(), "model".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rfc0081_expand_preserves_existential_types_and_calls() {
        let dir = unique_dir("expand_rfc0081");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.witchy");
        std::fs::write(
            &file,
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Label:\n    Label(String)\n\n\
             impl Render for Label:\n    fn render(let self) -> String:\n        match self:\n            Label(value) -> value\n\n\
             fn main(console: Console):\n    let value: dyn Render = Label(\"ready\")\n    console.print(value.render())\n",
        )
        .unwrap();

        let expanded = expand_file_source(file.to_str().unwrap()).expect("expand source");
        assert!(expanded.contains("let value: dyn Render = Label(\"ready\")"), "{expanded}");
        assert!(expanded.contains("console.print(value.render())"), "{expanded}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_file_uses_sibling_modules_for_imported_tags() {
        let dir = unique_dir("expand_imported_tag");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tags.witchy"),
            "import meta\n\npub comptime fn bump(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    \
             meta.expr_binary(\"+\", meta.expr_tag_hole_typed(holes.at(0)), meta.expr_int(1))\n",
        )
        .unwrap();
        let file = dir.join("app.witchy");
        std::fs::write(
            &file,
            "import tags\n\n\
             fn main(console: Console):\n    \
             let n = bump\"value ${41}\"\n    \
             console.print(\"${n}\")\n",
        )
        .unwrap();

        let expanded = expand_file_source(file.to_str().unwrap()).expect("expand source");
        assert!(!expanded.contains("bump\""), "{expanded}");
        assert!(expanded.contains("41 + 1"), "{expanded}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
