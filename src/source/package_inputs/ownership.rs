//! Loader ownership for a captured source graph. A module belongs to its
//! workspace or to an explicit path dependency's manifest. Registry packages
//! are the package manager's: it resolves and verifies them and hands the
//! compiler explicit inputs, so a `vendor/` tree or a non-path manifest
//! dependency never reaches the loader as source.
use super::*;
use std::collections::BTreeSet;

pub(super) struct Files<'a> {
    raw: &'a dyn SourceFiles,
}

impl<'a> Files<'a> {
    pub(super) fn new(raw: &'a dyn SourceFiles) -> Self { Self { raw } }

    fn selected_path_dependency(&self, path: &Path, alias: &str, entry: &Path) -> Result<bool, String> {
        let mut consumer = None;
        for root in entry.parent().into_iter().flat_map(Path::ancestors) {
            if self.raw.read(&root.join("witchy.toml"))?.is_some() {
                consumer = Some(root.to_path_buf());
                break;
            }
        }
        let Some(consumer) = consumer else { return Ok(false); };
        let selected = key(path)?;
        let mut pending = vec![consumer];
        let mut seen = BTreeSet::new();
        let mut declared = false;
        // collect_deps resolves explicit paths (including their path closure).
        // Consult those captured declarations; a directory's spelling is not
        // provenance.
        while let Some(root) = pending.pop() {
            if !seen.insert(directory_key(&root)?) { continue; }
            let Some(contents) = self.raw.read(&root.join("witchy.toml"))? else { continue; };
            let manifest: toml::Value = toml::from_str(&contents).map_err(|error| error.to_string())?;
            let Some(dependencies) = manifest.get("dependencies") else { continue; };
            for (name, dependency) in dependencies.as_table().ok_or("project dependencies must be a table")? {
                let Some(relative) = dependency.get("path") else { continue; };
                let relative = relative.as_str().ok_or("project dependency path must be a string")?;
                let dependency_root = root.join(relative);
                if name == alias {
                    declared = true;
                    let coordinate = directory_key(&dependency_root)?;
                    // Compare closed loader coordinates only. Actual bytes and
                    // workspace owners still come from the existing reader;
                    // this does not canonicalize a host filesystem access.
                    if selected == coordinate || selected.starts_with(&format!("{coordinate}/")) {
                        return Ok(true);
                    }
                }
                pending.push(dependency_root);
            }
        }
        if declared { return Err("selected dependency differs from its explicit manifest path".into()); }
        Ok(false)
    }

    pub(super) fn module_owner(&mut self, path: &Path, fallback: &str, entry: &Path) -> Result<ModuleLoadIdentity, String> {
        if !self.selected_path_dependency(path, fallback, entry)?
            && path.parent().into_iter().flat_map(Path::ancestors).any(|directory| {
                directory.parent().and_then(Path::file_name).and_then(|name| name.to_str()) == Some("vendor")
            })
        {
            // A vendored registry package would otherwise load under the
            // consumer's own workspace identity.
            return Err(format!("`{fallback}` is a vendored registry package; the package manager resolves those"));
        }
        generated::module_owner(path, fallback, entry, self.raw)
    }
}

impl SourceFiles for Files<'_> {
    fn read(&self, path: &Path) -> Result<Option<String>, String> { self.raw.read(path) }
    fn closed_package(&self) -> bool { true }
    fn authenticate_module(&self, path: &Path, owner: &ModuleLoadIdentity, _source: &str) -> Result<(), String> {
        if matches!(owner.package().source(), PackageSource::Registry(_)) {
            return Err("registry source modules are resolved by the package manager".into());
        }
        // A non-path manifest dependency is a registry selection. Refuse it
        // rather than loading a same-named file under the workspace's identity.
        for root in path.parent().into_iter().flat_map(Path::ancestors) {
            let Some(contents) = self.raw.read(&root.join("witchy.toml"))? else { continue; };
            let manifest: toml::Value = toml::from_str(&contents).map_err(|error| error.to_string())?;
            if let Some(dependencies) = manifest.get("dependencies") {
                let dependencies = dependencies.as_table().ok_or("project dependencies must be a table")?;
                if let Some((alias, _)) = dependencies.iter()
                    .find(|(_, dependency)| dependency.get("path").and_then(toml::Value::as_str).is_none())
                {
                    return Err(format!("project dependency `{alias}` is not a path dependency; the package manager resolves registry dependencies"));
                }
            }
            break;
        }
        Ok(())
    }
}
