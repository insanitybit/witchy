//! Native source discovery, loading, linking, and expansion.

use witchy_syntax::{ast, format, linker, parser};
use witchy_interp::{comptime, pipeline};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedDependency {
    pub path: std::path::PathBuf,
    pub owner: ModuleLoadIdentity,
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

/// Parse and link a source file. Non-std imports resolve from sibling
/// `<name>.witchy` files; reserved std names resolve from the bundled source.
/// Returns the linked module and entry stem.
#[cfg(test)]
pub(crate) fn link_file(path: &str) -> Result<(ast::Module, String), String> {
    link_file_with_mode(path, linker::LinkMode::Production)
}

#[cfg(test)]
pub(crate) fn link_file_with_mode(path: &str, mode: linker::LinkMode) -> Result<(ast::Module, String), String> {
    link_file_with_deps_mode(path, &std::collections::HashMap::new(), mode)
}

pub(crate) fn link_test_file(path: &str) -> Result<(pipeline::CheckedModule, String), String> {
    let (modules, entry_stem, user_modules) =
        load_file_modules(path, &std::collections::HashMap::new())?;
    let checked = pipeline::link_checked_test_with_user_modules(
        modules,
        &entry_stem,
        &user_modules,
    )
    .map_err(|error| error.to_string())?;
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
    let (modules, entry_stem, user_modules, owners) =
        load_file_modules_authenticated(path, &paths, entry_owner, &dep_owners)?;
    let checked = pipeline::link_checked_authenticated_with_user_modules(
        modules,
        &entry_stem,
        &user_modules,
        owners,
    )
    .map_err(|error| match error {
        pipeline::PipelineError::Ownership(error) => format!("{path}: {error}"),
        pipeline::PipelineError::Link(error) => error.to_string(),
        pipeline::PipelineError::Source(error) => format!("{path}: {error}"),
        pipeline::PipelineError::Type(error) => format!("{path}: {error}"),
    })?;
    Ok((checked, entry_stem))
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
                if bundled_module(&name) != Some(s.as_str()) {
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
        let module = parser::parse_module(&src).map_err(|e| format!("{name}: {e}"))?;
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
) -> Result<(SourceModules, String, UserModuleSet, AuthenticatedModuleOwners), String> {
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
        let (source, owner) = match std::fs::read_to_string(&module_path) {
            Ok(source) => {
                if bundled_module(&name) == Some(source.as_str()) {
                    (source, bundled_module_owner(&name)?)
                } else {
                    user_modules.insert(name.clone());
                    (source, proposed_owner)
                }
            }
            Err(error) => match bundled_module(&name) {
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
                        "cannot read `{}`: {error}{hint}",
                        module_path.display()
                    ));
                }
            },
        };
        assignments.push((name.clone(), owner.clone()));
        let module = parser::parse_module(&source).map_err(|error| format!("{name}: {error}"))?;
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
        modules.push((name, module));
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
    let manifest = source_path
        .parent()
        .into_iter()
        .flat_map(std::path::Path::ancestors)
        .find_map(|directory| {
            let path = directory.join("witchy.toml");
            path.is_file().then_some((directory, path))
        });
    let (package_name, package_version, module_path) = if let Some((root, manifest)) = manifest {
        let source = std::fs::read_to_string(&manifest)
            .map_err(|error| format!("cannot read `{}`: {error}", manifest.display()))?;
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
    let (mut modules, entry_stem, _) =
        load_file_modules(path, &std::collections::HashMap::new())?;
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
/// Whether a linked module contains a runnable `main`. Library-only files are
/// valid `witchy check` inputs, but there is no program artifact to compile.
pub(crate) fn linked_has_main(linked: &ast::Module) -> bool {
    linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
}

#[cfg(test)]
mod tests {
    use super::{
        expand_file_source, link_file_checked_authenticated_with_deps,
        AuthenticatedDependency,
    };
    use witchy_types::runtime_type::{
        DeclarationKind, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn expand_file_prints_generated_items_without_comptime_blocks() {
        let dir = unique_dir("expand_comptime");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.witchy");
        std::fs::write(
            &file,
            "from meta import item\n\n\
             comptime:\n    \
             emit_item(item(\"pub fn generated() -> Int:\\n    42\"))\n\n\
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
            "import meta\n\npub comptime fn bump(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    \
             meta.expr_raw(\"(\" + list.at(holes, 0) + \" + 1)\")\n",
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
