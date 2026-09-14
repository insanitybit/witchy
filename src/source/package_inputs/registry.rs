//! A pinned vendor selection is derived from raw offline resolver inputs.
//! Source membership is checked for every loaded module, not merely the root
//! dependency: an unsigned sibling cannot inherit a signed package's identity.
use super::*;
use std::collections::BTreeSet;
use witchy_interp::interpreter::{CompilerValue, evaluate_checked};

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
        if self.verifier.is_none() {
            // Only these compiler-bundled bytes enter the evaluator. The
            // supplied data cannot select a body or obtain a root capability.
            let compiler = PackageCoordinate::new(
                PackageSource::Toolchain, "witchy/compiler", env!("CARGO_PKG_VERSION"),
            ).map_err(|error| error.to_string())?;
            let mut assignments = vec![
                ("registry_recipe".to_owned(), ModuleLoadIdentity::new(compiler.clone(),
                    ["source", "package_inputs", "registry_recipe"]).map_err(|error| error.to_string())?),
                ("gm_core".to_owned(), ModuleLoadIdentity::new(compiler,
                    ["projects", "grimoire", "src", "gm_core"]).map_err(|error| error.to_string())?),
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
                "registry_recipe".to_owned(), "gm_core".to_owned(),
            ]);
            self.verifier = Some(witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(vec![
                ("registry_recipe".into(), include_bytes!("registry_verify.witchy").to_vec()),
                ("gm_core".into(), include_bytes!("../../../projects/grimoire/src/gm_core.witchy").to_vec()),
            ], "registry_recipe", comptime::expand_compile_time, &user_modules, owners)
                .map_err(|error| error.to_string())?);
        }
        let strings = |values: Vec<String>| CompilerValue::List(values.into_iter().map(CompilerValue::String).collect());
        let result = evaluate_checked(self.verifier.as_ref().expect("verifier initialized"), "registry_recipe.verify", vec![
            CompilerValue::String(lock), CompilerValue::String(alias.into()), CompilerValue::String(record),
            strings(sources.iter().map(|(name, _)| name.clone()).collect()),
            strings(sources.into_iter().map(|(_, source)| source).collect()),
        ]).map_err(|error| format!("pinned registry `{alias}`: {error}"))?;
        let CompilerValue::List(values) = result else { return Err("registry verifier returned invalid result kind".into()); };
        let [CompilerValue::String(status), CompilerValue::String(root), CompilerValue::String(name), CompilerValue::String(version)] = values.as_slice()
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
