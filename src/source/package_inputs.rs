//! Capture and replay the ordinary loader, without serialized ownership proofs.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Component, Path};
use witchy::trusted_publisher::source_inputs::{SourceDependency, SourceFile, SourceInputs, validate_path};

pub(super) trait SourceFiles {
    fn read(&self, path: &Path) -> Result<Option<String>, String>;
    fn closed_package(&self) -> bool { false }
}

pub(super) struct AmbientFiles;
impl SourceFiles for AmbientFiles {
    fn read(&self, path: &Path) -> Result<Option<String>, String> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read `{}`: {error}", path.display())),
        }
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    let path = std::path::absolute(path).map_err(|error| error.to_string())?;
    if path.components().any(|part| matches!(part, Component::Prefix(_))) {
        return Err("packaged source paths must not contain platform-prefix components".into());
    }
    Ok(path)
}

/// Coordinates only, NEVER paths for host filesystem access. Capture uses the
/// original spelling (including symlink/parent traversal). Colliding observations
/// refuse, and a replay must reproduce the original graph and ownership exactly.
fn key(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {},
            Component::Normal(part) => parts.push(part.to_str().ok_or("non-UTF-8 package path")?),
            Component::ParentDir => { parts.pop().ok_or("package path traverses above its virtual root")?; },
            _ => return Err("unsafe package path component".into()),
        }
    }
    let key = parts.join("/");
    validate_path(&key)?;
    Ok(key)
}

#[derive(Default)]
struct CaptureFiles(RefCell<BTreeMap<PathBuf, Option<String>>>);
impl SourceFiles for CaptureFiles {
    fn read(&self, path: &Path) -> Result<Option<String>, String> {
        // Cache by the actual spelling, not the virtual coordinate: `link/../x`
        // and `x` can name different files. Never substitute one read for another.
        if let Some(contents) = self.0.borrow().get(path) { return Ok(contents.clone()); }
        let contents = AmbientFiles.read(path)?;
        self.0.borrow_mut().insert(path.to_path_buf(), contents.clone());
        Ok(contents)
    }
    fn closed_package(&self) -> bool { true }
}

struct ReplayFiles<'a>(&'a SourceInputs);
impl SourceFiles for ReplayFiles<'_> {
    fn read(&self, path: &Path) -> Result<Option<String>, String> {
        let name = key(path)?;
        self.0.files.iter().find(|file| file.path == name)
            .map(|file| file.contents.clone())
            .ok_or_else(|| format!("closed package omitted loader lookup `{name}`"))
    }
    fn closed_package(&self) -> bool { true }
}

fn load(
    path: &Path, dependencies: &HashMap<String, PathBuf>, files: &dyn SourceFiles,
) -> Result<(Vec<(String, Vec<u8>)>, String, UserModuleSet, AuthenticatedModuleOwners), String> {
    let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or("invalid package entry")?;
    let owner = workspace_module_owner_with_files(path, stem, files)?;
    let owners = dependencies.iter().map(|(alias, path)| {
        Ok((alias.clone(), workspace_module_owner_with_files(path, alias, files)?))
    }).collect::<Result<HashMap<_, _>, String>>()?;
    load_file_modules_with_files(path.to_str().ok_or("non-UTF-8 package entry")?, dependencies, owner, &owners, files)
}

pub(crate) fn capture_packaged_source(
    entry: &str, dependencies: &HashMap<String, PathBuf>, manifest: &str,
) -> Result<(pipeline::CheckedModule, String, SourceInputs), String> {
    let inputs = capture_source_inputs(entry, dependencies, manifest)?;
    let (checked, entry) = recheck_packaged_source(&inputs)?;
    Ok((checked, entry, inputs))
}

/// Public owner flags are assertions about loader-selected inputs, never
/// assignments passed to the checker. Registry replay needs resolver evidence
/// that this workspace-only protocol deliberately does not manufacture.
pub(crate) fn capture_project_source(
    entry: &str, dependencies: &HashMap<String, PathBuf>, manifest: Option<&str>,
    entry_claim: &ModuleLoadIdentity, claims: &HashMap<String, ModuleLoadIdentity>,
) -> Result<(pipeline::CheckedModule, String, SourceInputs), String> {
    for claim in std::iter::once(entry_claim).chain(claims.values()) {
        if claim.package().source() != &PackageSource::Workspace {
            return Err("source replay does not yet support custom/registry owner assignments; resolver provenance must be retained, not relabeled".into());
        }
    }
    if claims.len() != dependencies.len() || dependencies.keys().any(|alias| !claims.contains_key(alias)) {
        return Err("project source requires exactly one owner assertion per selected dependency".into());
    }
    let manifest = match manifest {
        Some(path) => PathBuf::from(path),
        None => Path::new(entry).parent().into_iter().flat_map(Path::ancestors)
            .map(|directory| directory.join("witchy.toml"))
            .find(|path| path.is_file()).ok_or("project source requires a workspace manifest")?,
    };
    let inputs = capture_source_inputs(entry, dependencies,
        manifest.to_str().ok_or("non-UTF-8 project manifest")?)?;
    let files = ReplayFiles(&inputs);
    let assert_owner = |path: &str, alias: &str, claim: &ModuleLoadIdentity| -> Result<(), String> {
        let actual = workspace_module_owner_with_files(Path::new(path), alias, &files)?;
        // The existing project resolver spells a root module `foo`, whereas
        // the native loader records its package-relative path as `src.foo`.
        // Accept that spelling only as an assertion; always retain `actual`.
        let module_matches = claim.module_path() == actual.module_path()
            || (actual.module_path().len() == 2 && actual.module_path()[0] == "src"
                && claim.module_path() == &actual.module_path()[1..]);
        if claim.package() != actual.package() || !module_matches {
            return Err(format!("project owner assertion for `{alias}` differs from captured source/manifest ownership"));
        }
        Ok(())
    };
    let entry_name = Path::new(&inputs.entry).file_stem().and_then(|name| name.to_str())
        .ok_or("project source entry has no UTF-8 module name")?;
    assert_owner(&inputs.entry, entry_name, entry_claim)?;
    for dependency in &inputs.dependencies {
        assert_owner(&dependency.path, &dependency.alias, &claims[&dependency.alias])?;
    }
    // Do not turn a registry dependency into workspace ownership merely because
    // its source happens to be on disk or its caller labels it `workspace`.
    for file in &inputs.files {
        if !file.path.ends_with("/witchy.toml") { continue; }
        let Some(contents) = &file.contents else { continue; };
        let manifest: toml::Value = toml::from_str(contents).map_err(|error| format!("project manifest: {error}"))?;
        if let Some(dependencies) = manifest.get("dependencies") {
            let dependencies = dependencies.as_table().ok_or("project dependencies must be a table")?;
            for (alias, dependency) in dependencies {
                if dependency.get("path").and_then(toml::Value::as_str).is_none() {
                    return Err(format!("project registry dependency `{alias}` requires authenticated resolver replay; workspace relabeling is forbidden"));
                }
            }
        }
    }
    let (checked, entry) = recheck_packaged_source(&inputs)?;
    Ok((checked, entry, inputs))
}

fn capture_source_inputs(
    entry: &str, dependencies: &HashMap<String, PathBuf>, manifest: &str,
) -> Result<SourceInputs, String> {
    let entry = absolute(Path::new(entry))?;
    let dependencies = dependencies.iter().map(|(alias, path)| Ok((alias.clone(), absolute(path)?)))
        .collect::<Result<HashMap<_, _>, String>>()?;
    let manifest = absolute(Path::new(manifest))?;
    let files = CaptureFiles::default();
    // This is the ordinary first-loaded resolution algorithm. No source is read
    // a second time after checking; repeated reads use the captured first value.
    let original = load(&entry, &dependencies, &files)?;
    files.read(&manifest)?.ok_or("packaging manifest is missing")?;
    let mut dependencies = dependencies.into_iter().map(|(alias, path)| {
        Ok(SourceDependency { alias, path: key(&path)? })
    }).collect::<Result<Vec<_>, String>>()?;
    dependencies.sort_by(|a, b| a.alias.cmp(&b.alias));
    let mut virtual_files = BTreeMap::new();
    for (path, contents) in files.0.into_inner() {
        let name = key(&path)?;
        if let Some(previous) = virtual_files.get(&name) {
            if previous != &contents {
                return Err(format!("closed package path alias has conflicting observations at `{name}`; cannot preserve parent/symlink resolution"));
            }
        } else {
            virtual_files.insert(name, contents);
        }
    }
    let files = virtual_files.into_iter().map(|(path, contents)| SourceFile { path, contents }).collect();
    let inputs = SourceInputs::captured(key(&entry)?, dependencies, files, key(&manifest)?);
    let replay = replay_loaded_inputs(&inputs).map_err(|error| {
        format!("closed package cannot preserve source/manifest resolution: {error}")
    })?;
    if original != replay {
        return Err("closed package cannot preserve the selected source graph and loader ownership after path normalization".into());
    }
    Ok(inputs)
}

pub(crate) fn recheck_packaged_source(inputs: &SourceInputs) -> Result<(pipeline::CheckedModule, String), String> {
    let (sources, entry, users, owners) = replay_loaded_inputs(inputs)?;
    let checked = pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(sources, &entry, &users, owners)
        .map_err(|error| error.to_string())?;
    Ok((checked, entry))
}

fn replay_loaded_inputs(inputs: &SourceInputs)
    -> Result<(Vec<(String, Vec<u8>)>, String, UserModuleSet, AuthenticatedModuleOwners), String>
{
    let files = ReplayFiles(inputs);
    let dependencies = inputs.dependencies.iter().map(|dependency| {
        (dependency.alias.clone(), PathBuf::from(&dependency.path))
    }).collect();
    load(Path::new(&inputs.entry), &dependencies, &files)
}

pub(crate) fn restore_packaged_artifact(
    application: &witchy::trusted_publisher::OwnedEmbeddedApplication,
) -> Result<witchy_wir::AdmittedCanonicalArtifact, String> {
    // load/probe checked the envelope, compiler prefix and all compatibility
    // fields before this can initialize optimization state or parse source.
    let inputs = &application.source_inputs;
    inputs.configure_options()?;
    let (checked, entry) = recheck_packaged_source(inputs)?;
    witchy::enforce_performance_modes(checked.module(), &entry)?;
    let manifest = ReplayFiles(inputs).read(Path::new(&inputs.target_manifest))?
        .ok_or("closed target manifest missing")?;
    let bindings = witchy::trusted_publisher::build_binding_plan(checked.module(), &manifest)?;
    if bindings != application.bindings { return Err("packaged binding plan differs from freshly checked source and manifest".into()); }
    let artifact = match witchy_lower::codegen::compile_checked_module_artifact(&checked) {
        witchy_lower::codegen::LoweringOutcome::Lowered(artifact) => artifact,
        witchy_lower::codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        witchy_lower::codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    if artifact.wasm() != application.wasm { return Err("packaged Wasm differs from freshly admitted source".into()); }
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn parent_relative_package_matches_source_through_symlinks_and_refuses_alias_conflicts() {
        use witchy::trusted_publisher::{self, OwnedEmbeddedApplication};
        for symlink in [false, true] {
            struct Folder(PathBuf);
            impl Drop for Folder { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
            let folder = Folder(std::env::temp_dir().join(format!("witchy-parent-package-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())));
            let app = folder.0.join("app");
            let helper = folder.0.join(if symlink { "elsewhere/helper" } else { "helper" });
            for path in [app.join("src"), helper.join("src"), folder.0.join("elsewhere/anchor")] {
                std::fs::create_dir_all(path).unwrap();
            }
            let entry = app.join("src/main.witchy");
            let manifest = app.join("witchy.toml");
            let manifest_text = "[rune]\nname = \"parent-app\"\nversion = \"1\"\n[dependencies]\n[targets.trusted-publisher]\n";
            std::fs::write(&manifest, manifest_text).unwrap();
            std::fs::write(helper.join("witchy.toml"), "[rune]\nname = \"selected-helper\"\nversion = \"1\"\n").unwrap();
            std::fs::write(helper.join("src/helper.witchy"), "pub fn message() -> String:\n    \"selected through parent\"\n").unwrap();
            std::fs::write(&entry, "import helper\nfn main(console: Console):\n    let text = helper.message()\n    let report = fn(): console.print(text)\n    report()\n").unwrap();
            let dependency = if symlink {
                std::os::unix::fs::symlink(folder.0.join("elsewhere/anchor"), app.join("hop")).unwrap();
                // Lexical collapse would select this DIFFERENT source/manifest.
                std::fs::create_dir_all(app.join("helper/src")).unwrap();
                std::fs::write(app.join("helper/src/helper.witchy"), "pub fn message() -> String:\n    \"wrong lexical source\"\n").unwrap();
                std::fs::write(app.join("helper/witchy.toml"), "[rune]\nname = \"wrong-helper\"\nversion = \"9\"\n").unwrap();
                app.join("hop/../helper/src/helper.witchy")
            } else { app.join("../helper/src/helper.witchy") };
            let dependencies = HashMap::from([("helper".to_owned(), dependency.clone())]);
            let (ordinary, _) = super::super::link_file_checked_with_deps(entry.to_str().unwrap(), &dependencies).unwrap();
            let expected = vec!["selected through parent".to_owned()];
            assert_eq!(witchy_interp::interpreter::run_checked_module(&ordinary, ".", Vec::new()).unwrap(), expected);
            let (checked, _, inputs) = capture_packaged_source(entry.to_str().unwrap(), &dependencies, manifest.to_str().unwrap()).unwrap();
            for file in &inputs.files { validate_path(&file.path).unwrap(); }
            let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked).expect_lowered("parent-relative artifact");
            assert!(!artifact.owner_bridges().is_empty());
            let bindings = trusted_publisher::build_binding_plan(checked.module(), manifest_text).unwrap();
            let image = trusted_publisher::package(b"parent-package-unit-launcher", artifact.wasm(), &bindings, &inputs).unwrap();
            if symlink {
                // Both spellings are valid source inputs but collide in the
                // virtual namespace with different bytes/owners. Refuse rather
                // than letting the first cached observation impersonate either.
                let conflicting = HashMap::from([
                    ("helper".to_owned(), dependency),
                    ("other".to_owned(), app.join("helper/src/helper.witchy")),
                ]);
                let error = capture_packaged_source(entry.to_str().unwrap(), &conflicting, manifest.to_str().unwrap()).unwrap_err();
                assert!(error.contains("conflicting observations"), "{error}");
            }
            std::fs::remove_dir_all(&folder.0).unwrap();
            let embedded = trusted_publisher::probe(&image).unwrap().unwrap();
            let application = OwnedEmbeddedApplication {
                wasm: embedded.wasm.to_vec(), bindings: embedded.bindings.to_vec(),
                source_inputs: serde_json::from_slice(embedded.source_inputs).unwrap(),
            };
            let restored = restore_packaged_artifact(&application).unwrap();
            let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
            let caps = witchy_runtime::runtime::Capabilities { print: true, print_int: true, quiet: true,
                ..witchy_runtime::runtime::Capabilities::none() };
            let mut vm = runtime.spawn_admitted(&restored, caps, 1024).unwrap();
            vm.run().unwrap();
            assert_eq!(vm.output(), expected);
            assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        }
    }

    #[test]
    fn closed_loader_replays_first_loaded_explicit_dependencies_and_manifest_absence() {
        struct Folder(PathBuf);
        impl Drop for Folder { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let folder = Folder(std::env::temp_dir().join(format!("witchy-package-loader-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())));
        for path in ["src", "a", "b"] { std::fs::create_dir_all(folder.0.join(path)).unwrap(); }
        let manifest = folder.0.join("witchy.toml");
        std::fs::write(&manifest, "[rune]\nname = \"closed-loader\"\nversion = \"1\"\n").unwrap();
        let entry = folder.0.join("src/main.witchy");
        std::fs::write(&entry, "import a\nimport b\nfn main() -> Int:\n    a.value() + b.value()\n").unwrap();
        for name in ["a", "b"] {
            std::fs::write(folder.0.join(format!("{name}/{name}.witchy")),
                "import helper\npub fn value() -> Int:\n    helper.value()\n").unwrap();
        }
        std::fs::write(folder.0.join("a/helper.witchy"), "pub fn value() -> Int:\n    11\n").unwrap();
        std::fs::write(folder.0.join("b/helper.witchy"), "pub fn value() -> Int:\n    22\n").unwrap();
        // Explicit --dep wins over the entry's conflicting sibling.
        std::fs::write(folder.0.join("src/a.witchy"), "invalid source never selected").unwrap();
        let dependencies = ["a", "b"].into_iter().map(|name| (name.to_owned(), folder.0.join(format!("{name}/{name}.witchy")))).collect();
        let (checked, stem, inputs) = capture_packaged_source(entry.to_str().unwrap(), &dependencies, manifest.to_str().unwrap()).unwrap();
        assert_eq!(stem, "main");
        assert!(inputs.files.iter().any(|file| file.path.ends_with("a/helper.witchy")));
        assert!(!inputs.files.iter().any(|file| file.path.ends_with("b/helper.witchy") || file.path.ends_with("src/a.witchy")));
        assert!(inputs.files.iter().any(|file| file.path.ends_with("src/witchy.toml") && file.contents.is_none()));
        std::fs::remove_dir_all(&folder.0).unwrap();
        let (replayed, _) = recheck_packaged_source(&inputs).unwrap();
        assert_eq!(checked.canonical_source().unwrap().source_witness().source_bundle_digest(),
            replayed.canonical_source().unwrap().source_witness().source_bundle_digest());
    }
}
