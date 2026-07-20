//! Native source discovery, loading, linking, and expansion.

use crate::{ast, comptime, format, linker, parser, pipeline};

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
    crate::linker::std_source(name)
}

/// Parse and link a source file. Non-std imports resolve from sibling
/// `<name>.witchy` files; reserved std names resolve from the bundled source.
/// Returns the linked module and entry stem.
pub(crate) fn link_file(path: &str) -> Result<(ast::Module, String), String> {
    link_file_with_mode(path, linker::LinkMode::Production)
}

pub(crate) fn link_file_with_mode(path: &str, mode: linker::LinkMode) -> Result<(ast::Module, String), String> {
    link_file_with_deps_mode(path, &std::collections::HashMap::new(), mode)
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
    let (modules, entry_stem, user_modules) = load_file_modules(path, deps)?;
    let checked = pipeline::link_checked_with_user_modules(modules, &entry_stem, &user_modules)
        .map_err(|error| match error {
            pipeline::PipelineError::Ownership(error) => format!("{path}: {error}"),
            pipeline::PipelineError::Link(error) => error.to_string(),
            pipeline::PipelineError::Type(error) => format!("{path}: {error}"),
        })?;
    Ok((checked, entry_stem))
}

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
                        crate::linker::closest_std_module(&name)
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
    use super::expand_file_source;

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
            "fn bump(parts: List(String), holes: List(String)) -> String:\n    \
             \"(\" + list.at(holes, 0) + \" + 1)\"\n",
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
