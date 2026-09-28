//! A pinned vendor selection is derived from raw offline resolver inputs.
//! Source membership is checked for every loaded module, not merely the root
//! dependency: an unsigned sibling cannot inherit a signed package's identity.
use super::*;
use std::collections::BTreeSet;
use witchy_lower::codegen::{self, LoweringOutcome};
use witchy_runtime::runtime::{Capabilities, Runtime};

pub(super) struct Files<'a> {
    raw: &'a dyn SourceFiles,
    members: BTreeMap<PathBuf, (PackageCoordinate, String)>,
    selected: BTreeSet<(PathBuf, String)>,
    verifier: Option<pipeline::CheckedModule>,
}

impl<'a> Files<'a> {
    pub(super) fn new(raw: &'a dyn SourceFiles) -> Self {
        Self { raw, members: BTreeMap::new(), selected: BTreeSet::new(), verifier: None }
    }

    fn verifier(&mut self) -> Result<&pipeline::CheckedModule, String> {
        if self.verifier.is_none() {
            // Only compiler-bundled verifier sources enter comptime. In
            // particular, a vendored manifest cannot choose a verifier body.
            let compiler = PackageCoordinate::new(
                PackageSource::Toolchain, "witchy/compiler", env!("CARGO_PKG_VERSION"),
            ).map_err(|error| error.to_string())?;
            let mut assignments = vec![
                ("registry_recipe".to_owned(), ModuleLoadIdentity::new(compiler.clone(),
                    ["source", "package_inputs", "registry_recipe"]).map_err(|error| error.to_string())?),
                ("gm_core".to_owned(), ModuleLoadIdentity::new(compiler.clone(),
                    ["projects", "grimoire", "src", "gm_core"]).map_err(|error| error.to_string())?),
                ("coven_artifact".to_owned(), ModuleLoadIdentity::new(compiler,
                    ["projects", "coven", "src", "coven_artifact"]).map_err(|error| error.to_string())?),
            ];
            for name in witchy_syntax::linker::STD_MODULES {
                assignments.push(((*name).to_owned(), toolchain_module_owner(name)?));
            }
            for name in witchy_syntax::linker::PLAYGROUND_MODULES {
                assignments.push(((*name).to_owned(), playground_module_owner(name)?));
            }
            let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
                .map_err(|error| error.to_string())?;
            let user_modules = std::collections::HashSet::from([
                "registry_recipe".to_owned(), "gm_core".to_owned(), "coven_artifact".to_owned(),
            ]);
            self.verifier = Some(witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(vec![
                ("registry_recipe".into(), include_bytes!("registry_verify.witchy").to_vec()),
                ("gm_core".into(), include_bytes!("../../../projects/grimoire/src/gm_core.witchy").to_vec()),
                ("coven_artifact".into(), include_bytes!("../../../projects/coven/src/coven_artifact.witchy").to_vec()),
            ], "registry_recipe", comptime::EXPANSION_FREE_USER_EXPANDER,
                &user_modules, owners)
                .map_err(|error| error.to_string())?);
        }
        Ok(self.verifier.as_ref().expect("registry verifier initialized"))
    }

    fn evaluate_verifier(&mut self, mode: &str, arguments: Vec<String>) -> Result<Vec<String>, String> {
        let valid = match mode {
            "source" => arguments.len() >= 3 && (arguments.len() - 3) % 2 == 0,
            "precompiled" => arguments.len() == 6,
            _ => false,
        };
        if !valid {
            return Err("registry verifier received an invalid argument frame".into());
        }
        let artifact = match codegen::compile_checked_module_artifact(self.verifier()?) {
            LoweringOutcome::Lowered(artifact) => artifact,
            LoweringOutcome::Unsupported(reason) => return Err(format!("registry verifier is not admitted: {reason}")),
            LoweringOutcome::Rejected(error) => return Err(error.to_string()),
        };
        let mut runtime = Runtime::new().map_err(|error| error.to_string())?;
        let mut args = Vec::with_capacity(arguments.len() + 1);
        args.push(mode.to_owned());
        args.extend(arguments);
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, args,
            capture_output_limit_bytes: Some(1024 * 1024),
            ..Capabilities::none()
        }, 1024).map_err(|error| error.to_string())?;
        runtime.run_with_budget(&mut vm, std::time::Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
        if vm.canonical_heap_leaks().map_err(|error| error.to_string())? != Some(0) {
            return Err("registry verifier leaked heap objects".into());
        }
        let output = vm.output();
        let [output] = output.as_slice() else {
            return Err("registry verifier returned an invalid output frame".into());
        };
        serde_json::from_str(output).map_err(|error| format!("registry verifier returned invalid JSON: {error}"))
    }

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
        // collect_deps resolves explicit paths (including their path closure)
        // before registry locks. Consult those captured declarations before
        // interpreting a vendor directory; its spelling is not provenance.
        while let Some(root) = pending.pop() {
            if !seen.insert(key(&root)?) { continue; }
            let Some(contents) = self.raw.read(&root.join("witchy.toml"))? else { continue; };
            let manifest: toml::Value = toml::from_str(&contents).map_err(|error| error.to_string())?;
            let Some(dependencies) = manifest.get("dependencies") else { continue; };
            for (name, dependency) in dependencies.as_table().ok_or("project dependencies must be a table")? {
                let Some(relative) = dependency.get("path") else { continue; };
                let relative = relative.as_str().ok_or("project dependency path must be a string")?;
                let dependency_root = root.join(relative);
                if name == alias {
                    declared = true;
                    let coordinate = key(&dependency_root)?;
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
        if self.selected_path_dependency(path, fallback, entry)? {
            return generated::module_owner(path, fallback, entry, self.raw);
        }
        let vendor = path.parent().into_iter().flat_map(Path::ancestors).find(|directory| {
            directory.parent().and_then(Path::file_name).and_then(|name| name.to_str()) == Some("vendor")
        });
        let Some(vendor) = vendor else {
            return generated::module_owner(path, fallback, entry, self.raw);
        };
        let alias = vendor.file_name().and_then(|name| name.to_str()).ok_or("registry vendor alias is not UTF-8")?;
        if alias != fallback {
            return Err("registry selected alias differs from its vendor namespace".into());
        }
        let consumer = vendor.parent().and_then(Path::parent).ok_or("registry consumer missing")?;
        let relative = path.strip_prefix(vendor).map_err(|error| error.to_string())?;
        let relative_text = relative.to_str().ok_or("registry module path is not UTF-8")?;
        validate_path(relative_text)?;
        if !relative.starts_with("src") || path.extension().and_then(|extension| extension.to_str()) != Some("witchy") {
            return Err("registry module must belong to the signed src tree".into());
        }
        self.raw.read(&consumer.join("witchy.toml"))?.ok_or("registry consumer manifest missing")?;
        let lock = self.raw.read(&consumer.join("witchy.lock"))?.ok_or("registry consumer lock missing")?;
        let record = self.raw.read(&vendor.join("coven.json"))?.ok_or("registry signed coven.json missing")?;
        let manifest = self.raw.read(&vendor.join("witchy.toml"))?.ok_or("registry vendor manifest missing")?;
        let mut sources = vec![("witchy.toml".to_owned(), manifest.clone())];
        let mut paths = self.raw.registry_source_paths(vendor)?;
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate registry source path".into());
        }
        let mut members = Vec::new();
        for source_path in paths {
            let relative = source_path.strip_prefix(vendor).map_err(|_| "registry census escapes its vendor")?;
            let name = relative.to_str().ok_or("registry census path is not UTF-8")?;
            validate_path(name)?;
            if !relative.starts_with("src") || source_path.extension().and_then(|extension| extension.to_str()) != Some("witchy") {
                return Err("registry census contains a non-source entry".into());
            }
            let source = self.raw.read(&source_path)?.ok_or("registry census source missing")?;
            sources.push((name.to_owned(), source.clone()));
            members.push((source_path, source));
        }
        if !members.iter().any(|(member, _)| member == path) {
            return Err("registry selected module is absent from signed source census".into());
        }
        let mut arguments = vec![lock, alias.into(), record];
        arguments.extend(sources.iter().map(|(name, _)| name.clone()));
        arguments.extend(sources.into_iter().map(|(_, source)| source));
        let values = self.evaluate_verifier("source", arguments)
            .map_err(|error| format!("pinned registry `{alias}`: {error}"))?;
        let [status, root, name, version] = values.as_slice()
            else { return Err("registry verifier returned invalid result schema".into()); };
        if status != "ok" { return Err(format!("pinned registry `{alias}`: {root}")); }
        let package = PackageCoordinate::new(PackageSource::Registry(root.clone()), name.clone(), version.clone())
            .map_err(|error| error.to_string())?;
        let manifest: toml::Value = toml::from_str(&manifest).map_err(|error| error.to_string())?;
        let rune = manifest.get("rune").ok_or("registry manifest has no rune coordinate")?;
        if rune.get("name").and_then(toml::Value::as_str) != Some(name.as_str())
            || rune.get("version").and_then(toml::Value::as_str) != Some(version.as_str()) {
            return Err("registry manifest coordinate differs from signed record".into());
        }
        for (member, source) in members {
            if let Some(previous) = self.members.insert(member, (package.clone(), source.clone())) {
                if previous != (package.clone(), source) { return Err("conflicting verified registry membership".into()); }
            }
        }
        self.selected.insert((consumer.to_path_buf(), alias.into()));
        let mut module = relative.with_extension("").components().map(|part| match part {
            Component::Normal(name) => name.to_str().map(str::to_owned).ok_or("registry module component is not UTF-8"),
            _ => Err("registry module contains ambiguous components"),
        }).collect::<Result<Vec<_>, _>>()?;
        if module.is_empty() { return Err("registry module has no name".into()); }
        ModuleLoadIdentity::new(package, std::mem::take(&mut module)).map_err(|error| error.to_string())
    }

    /// Resolve only explicit `kind = "precompiled"` registry declarations.
    /// This deliberately has no source-path fallback: the signed manifest
    /// commits both artifact blobs and the resolver owns their fixed locations.
    pub(super) fn precompiled_dependencies(
        &mut self,
        manifest_path: &Path,
    ) -> Result<HashMap<String, AuthenticatedPrecompiledDependency>, String> {
        if manifest_path.file_name().and_then(|name| name.to_str()) != Some("witchy.toml") {
            return Err("precompiled dependency resolution requires the project's witchy.toml".into());
        }
        let consumer = manifest_path.parent().ok_or("project source manifest has no parent directory")?.to_path_buf();
        let consumer_manifest = self.raw.read(&consumer.join("witchy.toml"))?
            .ok_or("project source manifest disappeared during precompiled dependency resolution")?;
        let manifest: toml::Value = toml::from_str(&consumer_manifest).map_err(|error| error.to_string())?;
        let Some(dependencies) = manifest.get("dependencies") else { return Ok(HashMap::new()); };
        let dependencies = dependencies.as_table().ok_or("project dependencies must be a table")?;

        let mut selected = Vec::new();
        for (alias, dependency) in dependencies {
            let Some(table) = dependency.as_table() else { continue; };
            let Some(kind) = table.get("kind") else { continue; };
            let kind = kind.as_str().ok_or_else(|| format!("dependency `{alias}` kind must be a string"))?;
            if kind != "precompiled" {
                return Err(format!("dependency `{alias}` has unsupported kind `{kind}`"));
            }
            if table.contains_key("path") {
                return Err(format!("precompiled dependency `{alias}` must be registry-selected, not a path dependency"));
            }
            if let Some(source) = table.get("source") {
                if source.as_str() != Some("coven") {
                    return Err(format!("precompiled dependency `{alias}` must select the Coven registry"));
                }
            }
            let module = table.get("module").and_then(toml::Value::as_str)
                .ok_or_else(|| format!("precompiled dependency `{alias}` requires logical module `src/...`"))?;
            let requirement = table.get("version").and_then(toml::Value::as_str)
                .filter(|requirement| !requirement.trim().is_empty())
                .ok_or_else(|| format!("precompiled dependency `{alias}` requires a non-empty registry version requirement"))?;
            let components = logical_module_components(module)?;
            selected.push((alias.clone(), module.to_owned(), components, requirement.to_owned()));
        }

        let mut dependencies = HashMap::new();
        for (alias, module, components, requirement) in selected {
            registry_alias_component(&alias)?;
            if witchy_syntax::linker::STD_MODULES.contains(&alias.as_str())
                || witchy_syntax::linker::PLAYGROUND_MODULES.contains(&alias.as_str())
            {
                return Err(format!("precompiled dependency `{alias}` conflicts with a reserved toolchain module"));
            }
            let vendor = regular_vendor_directory(&consumer, &alias)?;
            regular_file(&consumer.join("witchy.lock"))?;
            regular_vendor_file(&vendor, Path::new("coven.json"))?;
            regular_vendor_file(&vendor, Path::new("coven-artifact.json"))?;
            let lock = self.raw.read(&consumer.join("witchy.lock"))?
                .ok_or("registry consumer lock missing")?;
            let record = self.raw.read(&vendor.join("coven.json"))?
                .ok_or("registry signed coven.json missing")?;
            let artifact_manifest = self.raw.read(&vendor.join("coven-artifact.json"))?
                .ok_or("precompiled registry artifact manifest missing")?;
            let values = self.evaluate_verifier("precompiled", vec![
                lock, alias.clone(), record, artifact_manifest, module.clone(), requirement,
            ]).map_err(|error| format!("pinned precompiled registry `{alias}`: {error}"))?;
            let [status, root, name, version, wasm_sha256, sidecar_sha256, wasm_size, sidecar_size] = values.as_slice()
            else { return Err("precompiled registry verifier returned invalid result schema".into()); };
            if status != "ok" { return Err(format!("pinned precompiled registry `{alias}`: {root}")); }
            let wasm_size = signed_size(wasm_size, 64 * 1024 * 1024, "Wasm")?;
            let sidecar_size = signed_size(sidecar_size, 1024 * 1024, "export-signature sidecar")?;
            let package = PackageCoordinate::new(PackageSource::Registry(root.clone()), name.clone(), version.clone())
                .map_err(|error| error.to_string())?;
            let owner = ModuleLoadIdentity::new(package, components.clone()).map_err(|error| error.to_string())?;
            let artifact_root = vendor.join("artifacts").join(&module);
            let wasm_path = artifact_root.with_extension("wasm");
            let sidecar_path = artifact_root.with_extension("opt");
            regular_vendor_file(&vendor, wasm_path.strip_prefix(&vendor).map_err(|_| "precompiled artifact escapes vendor root")?)?;
            regular_vendor_file(&vendor, sidecar_path.strip_prefix(&vendor).map_err(|_| "precompiled artifact escapes vendor root")?)?;
            let wasm = self.raw.read_bounded_bytes(&wasm_path, wasm_size)?
                .ok_or("precompiled registry Wasm artifact missing")?;
            if wasm.len() != wasm_size {
                return Err("precompiled registry Wasm artifact differs from signed size".into());
            }
            let sidecar = self.raw.read_bounded_bytes(&sidecar_path, sidecar_size)?
                .ok_or("precompiled registry export-signature sidecar missing")?;
            if sidecar.len() != sidecar_size {
                return Err("precompiled registry export-signature sidecar differs from signed size".into());
            }
            let library = precompiled_library::PrecompiledLibrary::verify(
                owner, wasm, &sidecar, wasm_sha256, sidecar_sha256,
            )?;
            if dependencies.insert(alias.clone(), AuthenticatedPrecompiledDependency { library }).is_some() {
                return Err(format!("duplicate precompiled dependency `{alias}`"));
            }
            self.selected.insert((consumer.clone(), alias));
        }
        Ok(dependencies)
    }
}

fn logical_module_components(module: &str) -> Result<Vec<String>, String> {
    if !module.starts_with("src/") || module.ends_with(".witchy") || module.contains('\\') {
        return Err("precompiled dependency module must be a logical `src/...` path without `.witchy`".into());
    }
    validate_path(module)?;
    let components: Vec<_> = module.split('/').map(str::to_owned).collect();
    if components.len() < 2 || components.iter().any(|part| part.is_empty() || part == "." || part == "..") {
        return Err("precompiled dependency module must be a logical `src/...` path".into());
    }
    Ok(components)
}

fn registry_alias_component(alias: &str) -> Result<(), String> {
    let mut components = Path::new(alias).components();
    if alias.is_empty() || alias.contains(['/', '\\'])
        || !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some()
    {
        return Err(format!("precompiled dependency alias `{alias}` must be one safe path component"));
    }
    Ok(())
}

fn signed_size(value: &str, limit: usize, artifact: &str) -> Result<usize, String> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("signed precompiled {artifact} size is not a canonical non-negative decimal"));
    }
    let size = value.parse::<usize>().map_err(|_| format!("signed precompiled {artifact} size is out of range"))?;
    if size == 0 {
        return Err(format!("signed precompiled {artifact} size must be positive"));
    }
    if size > limit {
        return Err(format!("signed precompiled {artifact} size exceeds its authenticated limit"));
    }
    Ok(size)
}

fn regular_file(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect precompiled registry input `{}`: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("precompiled registry input `{}` must be a regular non-symlink file", path.display()));
    }
    Ok(())
}

fn regular_vendor_directory(consumer: &Path, alias: &str) -> Result<PathBuf, String> {
    let root = consumer.join("vendor");
    let vendor = root.join(alias);
    for directory in [&root, &vendor] {
        let metadata = std::fs::symlink_metadata(directory)
            .map_err(|error| format!("cannot inspect precompiled registry vendor `{}`: {error}", directory.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!("precompiled registry vendor `{}` must be a real directory", directory.display()));
        }
    }
    Ok(vendor)
}

fn regular_vendor_file(vendor: &Path, relative: &Path) -> Result<(), String> {
    // Rust's portable file-open API has no no-follow primitive. These checks
    // reject existing symlink paths; the bounded byte reader still limits a
    // later replacement, but a hostile concurrent replacement remains a
    // platform-specific TOCTOU residual until a no-follow open is available.
    let mut current = vendor.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() { return Err("precompiled registry input has no file name".into()); }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(segment) = component else {
            return Err("precompiled registry input has an unsafe relative path".into());
        };
        current.push(segment);
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| format!("cannot inspect precompiled registry input `{}`: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("precompiled registry input `{}` must not cross a symlink", current.display()));
        }
        if index + 1 == components.len() {
            if !metadata.is_file() {
                return Err(format!("precompiled registry input `{}` must be a regular file", current.display()));
            }
        } else if !metadata.is_dir() {
            return Err(format!("precompiled registry input `{}` must be a directory", current.display()));
        }
    }
    Ok(())
}

impl SourceFiles for Files<'_> {
    fn read(&self, path: &Path) -> Result<Option<String>, String> { self.raw.read(path) }
    fn closed_package(&self) -> bool { true }
    fn authenticate_module(&self, path: &Path, owner: &ModuleLoadIdentity, source: &str) -> Result<(), String> {
        if matches!(owner.package().source(), PackageSource::Registry(_)) {
            return match self.members.get(path) {
                Some((package, bytes)) if package == owner.package() && bytes == source => Ok(()),
                _ => Err("registry module is not a member of the independently verified signed tree".into()),
            };
        }
        if self.members.contains_key(path) { return Err("signed registry source cannot be relabeled workspace".into()); }
        // This also rejects caller-supplied workspace paths for non-path
        // dependencies, rather than using absence of a Registry claim as proof.
        for root in path.parent().into_iter().flat_map(Path::ancestors) {
            let Some(contents) = self.raw.read(&root.join("witchy.toml"))? else { continue; };
            let manifest: toml::Value = toml::from_str(&contents).map_err(|error| error.to_string())?;
            if let Some(dependencies) = manifest.get("dependencies") {
                let dependencies = dependencies.as_table().ok_or("project dependencies must be a table")?;
                for (alias, dependency) in dependencies {
                    if dependency.as_table().and_then(|table| table.get("kind"))
                        .and_then(toml::Value::as_str) == Some("precompiled")
                    {
                        continue;
                    }
                    if dependency.get("path").and_then(toml::Value::as_str).is_none()
                        && !self.selected.contains(&(root.to_path_buf(), alias.clone())) {
                        return Err(format!("project registry dependency `{alias}` requires authenticated resolver replay; workspace relabeling is forbidden"));
                    }
                }
            }
            break;
        }
        Ok(())
    }
}

#[cfg(test)]
mod admitted_verifier_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};

    #[test]
    fn signed_source_registry_verifies_on_admitted_wasm() {
        let path = "src/main.witchy";
        let contents = "fn main() -> Int:\n    1\n";
        let manifest = "[rune]\nname=\"acme/math\"\nversion=\"1.0.0\"\n";
        let files = [(path, contents), ("witchy.toml", manifest)];
        let mut bytes = Vec::new();
        for (name, source) in files {
            bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&(source.len() as u64).to_le_bytes());
            bytes.extend_from_slice(source.as_bytes());
        }
        let hash = format!("sha256:{}", Sha256::digest(&bytes).iter()
            .map(|byte| format!("{byte:02x}")).collect::<String>());
        let key = SigningKey::from_bytes(&[71; 32]);
        let rootpub = key.verifying_key().as_bytes().iter()
            .map(|byte| format!("{byte:02x}")).collect::<String>();
        let payload = format!(
            "coven-v1\nname=acme/math\nversion=1.0.0\nstate=released\nhash={hash}\nrt=[]\nbuild=\nuploaded_by=\npromoted_by=\nfactor=\nprovenance=\nreleased_at=0"
        );
        let record = serde_json::json!({
            "name": "acme/math", "version": "1.0.0", "state": "released", "hash": hash,
            "runtime_footprint": [], "released_at": 0, "artifact_digest": "",
            "sig": key.sign(payload.as_bytes()).to_bytes().iter()
                .map(|byte| format!("{byte:02x}")).collect::<String>(),
        }).to_string();
        let lock = format!(
            "registry_snapshot_version=1\nregistry_rootpub=\"{rootpub}\"\n[[rune]]\nname=\"acme/math\"\nalias=\"external_math\"\nversion=\"1.0.0\"\nsource=\"coven\"\nhash=\"{hash}\"\nruntime_footprint=[]\n"
        );
        let mut verifier = Files::new(&AmbientFiles);
        let result = verifier.evaluate_verifier("source", vec![
            lock.clone(), "external_math".into(), record.clone(),
            "witchy.toml".into(), path.into(), manifest.into(), contents.into(),
        ]).expect("signed source verifier runs on admitted Wasm");
        assert_eq!(result, ["ok", &rootpub, "acme/math", "1.0.0"]);
        let tampered = verifier.evaluate_verifier("source", vec![
            lock, "external_math".into(), record,
            "witchy.toml".into(), path.into(), manifest.into(),
            "fn main() -> Int:\n    2\n".into(),
        ]).expect("tampered source is rejected by admitted verifier");
        assert_eq!(tampered.first().map(String::as_str), Some("error"));
        assert!(tampered[1].contains("source hash differs"), "{tampered:?}");
    }

    #[test]
    fn source_registry_verifier_rejects_unpinned_input_on_admitted_wasm() {
        let mut files = Files::new(&AmbientFiles);
        let result = files.evaluate_verifier("source", vec![
            "".into(), "unlisted".into(), "{}".into(),
            "src/main.witchy".into(), "fn main() -> Int:\n    1\n".into(),
        ]).expect("compiler-owned verifier executes on admitted Wasm");
        assert_eq!(result.first().map(String::as_str), Some("error"));
        assert_eq!(result.len(), 4);
    }
}
