//! Workspace-generated namespaces are reconstructed from captured source inputs.
//!
//! This establishes namespace ownership, not a receipt claiming that a build
//! executed or produced particular bytes. Output bytes are ordinary untrusted
//! workspace source and still undergo fresh checking and exact-byte admission.
use super::*;

pub(super) fn module_owner(
    path: &Path,
    fallback: &str,
    entry: &Path,
    files: &dyn SourceFiles,
) -> Result<ModuleLoadIdentity, String> {
    // Anchor to the entry's actual ancestry, never the caller's --manifest.
    let mut consumer = None;
    for root in entry.parent().into_iter().flat_map(Path::ancestors) {
        if let Some(manifest) = files.read(&root.join("witchy.toml"))? {
            consumer = Some((root, manifest));
            break;
        }
    }
    let Some((root, manifest)) = consumer else {
        return workspace_module_owner_with_files(path, fallback, files);
    };
    let generated = match path.strip_prefix(root).ok()
        .and_then(|relative| relative.strip_prefix("build-out").ok()) {
        Some(relative) => relative,
        None => return workspace_module_owner_with_files(path, fallback, files),
    };
    let parts = generated.components().map(|part| match part {
        Component::Normal(name) => name.to_str().map(str::to_owned)
            .ok_or_else(|| "generated source path is not UTF-8".to_owned()),
        _ => Err("generated source path contains traversal or ambiguous components".to_owned()),
    }).collect::<Result<Vec<_>, _>>()?;
    if parts.len() < 2 || path.extension().and_then(|extension| extension.to_str()) != Some("witchy") {
        return Err("generated source must select a .witchy module beneath one producer namespace".into());
    }
    let alias = &parts[0];
    let manifest: toml::Value = toml::from_str(&manifest)
        .map_err(|error| format!("generated source consumer manifest: {error}"))?;
    let dependency = manifest.get("dependencies").and_then(|value| value.get(alias))
        .ok_or_else(|| format!("generated source namespace `{alias}` has no consumer dependency"))?;
    let producer_path = dependency.get("path").and_then(toml::Value::as_str)
        .ok_or_else(|| format!("generated source namespace `{alias}` requires authenticated registry resolver replay"))?;
    if manifest.get("build").and_then(|value| value.get("grants"))
        .and_then(|value| value.get(alias)).and_then(toml::Value::as_table).is_none() {
        return Err(format!("generated source namespace `{alias}` has no explicit consumer build grant"));
    }
    // An output-controlled manifest cannot choose a different package identity.
    // Capture absence too, so closed replay makes exactly the same decision.
    for directory in path.parent().into_iter().flat_map(Path::ancestors) {
        if directory == root { break; }
        if files.read(&directory.join("witchy.toml"))?.is_some() {
            return Err(format!("generated source namespace `{alias}` contains a conflicting ownership manifest"));
        }
    }
    let producer = root.join(producer_path);
    files.read(&producer.join("witchy.toml"))?
        .ok_or_else(|| format!("generated source producer `{alias}` has no manifest"))?;
    if files.read(&producer.join("src/witchy.toml"))?.is_some() {
        return Err(format!("generated source producer `{alias}` has conflicting source ownership"));
    }
    let build = producer.join("src/build.witchy");
    files.read(&build)?
        .ok_or_else(|| format!("generated source producer `{alias}` has no build source"))?;
    // Reuse ordinary workspace identity derivation. Neither the dependency alias
    // nor a supplied --dep-owner determines the producer's coordinate.
    let producer_owner = workspace_module_owner_with_files(&build, "build", files)?;
    let mut module = vec!["src".to_owned()];
    module.extend(parts[1..parts.len() - 1].iter().cloned());
    module.push(path.file_stem().and_then(|name| name.to_str())
        .ok_or("generated source module has no UTF-8 name")?.to_owned());
    ModuleLoadIdentity::new(producer_owner.package().clone(), module)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Files(BTreeMap<String, String>);
    impl SourceFiles for Files {
        fn read(&self, path: &Path) -> Result<Option<String>, String> {
            Ok(self.0.get(&key(path)?).cloned())
        }
    }
    fn fixture() -> Files {
        Files(BTreeMap::from([
            ("workspace/app/witchy.toml".into(),
                "[rune]\nname=\"consumer\"\nversion=\"1\"\n[dependencies]\ncodegen={path=\"../producer\"}\n[build.grants.codegen]\n".into()),
            ("workspace/producer/witchy.toml".into(),
                "[rune]\nname=\"actual-producer\"\nversion=\"2\"\n".into()),
            ("workspace/producer/src/build.witchy".into(),
                "fn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", \"\")\n".into()),
        ]))
    }
    fn owner(files: &Files, output: &str) -> Result<ModuleLoadIdentity, String> {
        module_owner(Path::new(output), "generated", Path::new("/workspace/app/src/main.witchy"), files)
    }

    #[test]
    fn generated_workspace_ownership_uses_real_producer_and_module_path() {
        let files = fixture();
        let actual = owner(&files, "/workspace/app/build-out/codegen/generated.witchy").unwrap();
        let expected = workspace_module_owner_with_files(
            Path::new("/workspace/producer/src/generated.witchy"), "generated", &files).unwrap();
        assert_eq!(actual, expected);
        let sibling = owner(&files, "/workspace/app/build-out/codegen/sibling.witchy").unwrap();
        assert_eq!(actual.package(), sibling.package());
        assert_ne!(actual.module_path(), sibling.module_path());
        let ordinary = owner(&files, "/workspace/app/src/generated.witchy").unwrap();
        assert_ne!(ordinary.package(), actual.package());
    }

    #[test]
    fn generated_workspace_ownership_rejects_undeclared_ungranted_and_registry_roots() {
        for (manifest, reason) in [
            ("[rune]\nname=\"consumer\"\n[dependencies]\n[build.grants.codegen]\n", "no consumer dependency"),
            ("[rune]\nname=\"consumer\"\n[dependencies]\ncodegen={path=\"../producer\"}\n", "no explicit consumer build grant"),
            ("[rune]\nname=\"consumer\"\n[dependencies]\ncodegen={version=\"2\"}\n[build.grants.codegen]\n", "registry resolver replay"),
        ] {
            let mut files = fixture();
            files.0.insert("workspace/app/witchy.toml".into(), manifest.into());
            let error = owner(&files, "/workspace/app/build-out/codegen/generated.witchy").unwrap_err();
            assert!(error.contains(reason), "{error}");
        }
    }

    #[test]
    fn generated_workspace_ownership_rejects_output_manifest_and_missing_producer_inputs() {
        let mut files = fixture();
        files.0.insert("workspace/app/build-out/codegen/witchy.toml".into(),
            "[rune]\nname=\"forged-owner\"\nversion=\"2\"\n".into());
        assert!(owner(&files, "/workspace/app/build-out/codegen/generated.witchy")
            .unwrap_err().contains("conflicting ownership manifest"));
        for missing in ["workspace/producer/witchy.toml", "workspace/producer/src/build.witchy"] {
            let mut files = fixture();
            files.0.remove(missing);
            assert!(owner(&files, "/workspace/app/build-out/codegen/generated.witchy").is_err());
        }
        assert!(owner(&fixture(), "/workspace/app/build-out/codegen/../codegen/generated.witchy").is_err());
    }
}
