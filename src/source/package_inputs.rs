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
    if path.components().any(|part| matches!(part, Component::ParentDir | Component::Prefix(_))) {
        return Err("packaged source paths must not contain parent or platform-prefix components".into());
    }
    Ok(path)
}

/// Coordinates inside the package's virtual filesystem. They are never joined
/// to a host directory or extracted. Keeping ancestry preserves nearest-manifest
/// absence and sibling lookup even when dependencies have different roots.
fn key(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {},
            Component::Normal(part) => parts.push(part.to_str().ok_or("non-UTF-8 package path")?),
            _ => return Err("unsafe package path component".into()),
        }
    }
    let key = parts.join("/");
    validate_path(&key)?;
    Ok(key)
}

#[derive(Default)]
struct CaptureFiles(RefCell<BTreeMap<String, Option<String>>>);
impl SourceFiles for CaptureFiles {
    fn read(&self, path: &Path) -> Result<Option<String>, String> {
        let name = key(path)?;
        if let Some(contents) = self.0.borrow().get(&name) { return Ok(contents.clone()); }
        let contents = AmbientFiles.read(path)?;
        self.0.borrow_mut().insert(name, contents.clone());
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
    let entry = absolute(Path::new(entry))?;
    let dependencies = dependencies.iter().map(|(alias, path)| Ok((alias.clone(), absolute(path)?)))
        .collect::<Result<HashMap<_, _>, String>>()?;
    let manifest = absolute(Path::new(manifest))?;
    let files = CaptureFiles::default();
    // This is the ordinary first-loaded resolution algorithm. No source is read
    // a second time after checking; repeated reads use the captured first value.
    let _ = load(&entry, &dependencies, &files)?;
    files.read(&manifest)?.ok_or("packaging manifest is missing")?;
    let mut dependencies = dependencies.into_iter().map(|(alias, path)| {
        Ok(SourceDependency { alias, path: key(&path)? })
    }).collect::<Result<Vec<_>, String>>()?;
    dependencies.sort_by(|a, b| a.alias.cmp(&b.alias));
    let files = files.0.into_inner().into_iter().map(|(path, contents)| SourceFile { path, contents }).collect();
    let inputs = SourceInputs::captured(key(&entry)?, dependencies, files, key(&manifest)?);
    let (checked, entry) = recheck_packaged_source(&inputs)?;
    Ok((checked, entry, inputs))
}

pub(crate) fn recheck_packaged_source(inputs: &SourceInputs) -> Result<(pipeline::CheckedModule, String), String> {
    let files = ReplayFiles(inputs);
    let dependencies = inputs.dependencies.iter().map(|dependency| {
        (dependency.alias.clone(), PathBuf::from(&dependency.path))
    }).collect();
    let (sources, entry, users, owners) = load(Path::new(&inputs.entry), &dependencies, &files)?;
    let checked = pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(sources, &entry, &users, owners)
        .map_err(|error| error.to_string())?;
    Ok((checked, entry))
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
