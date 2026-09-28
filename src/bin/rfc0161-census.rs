//! Type-resolved RFC-0161 concurrency migration census for the repository corpus.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use witchy::{ast, parser};
use witchy_syntax::linker;
use witchy_types::async_migration::{Category, Census};

fn async_migration_census(
    modules: Vec<(String, ast::Module)>, entry: &str, user_modules: &HashSet<String>,
) -> Result<Census, String> {
    let expand = linker::ComptimeExpander::with_sibling_request(
        witchy_types::pipeline::require_expansion_free, |_| false,
    );
    let linked = linker::link_with_user_modules(modules, entry, expand, user_modules)
        .map_err(|error| error.to_string())?;
    Ok(witchy_types::async_migration::census_linked(&linked, entry))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rfc0161-census: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let strict = arguments.iter().any(|argument| argument == "--strict");
    let root = arguments.iter().find(|argument| argument.as_str() != "--strict")
        .cloned().unwrap_or_else(|| ".".to_string());
    let root = Path::new(&root);
    let mut files = Vec::new();
    for directory in ["std", "examples", "projects", "benchmarks", "tests"] {
        collect_witchy_files(&root.join(directory), &mut files)?;
    }
    files.sort();
    let source_index = source_index(&files);
    let mut totals = BTreeMap::new();
    let mut checked = 0usize;
    let mut rejected = Vec::new();

    println!("path\tline\tcategory\tcallee");
    for path in &files {
        record_report(
            display_from(root, path),
            link_file(path, &source_index)?,
            &mut checked,
            &mut rejected,
            &mut totals,
        );
    }
    for path in markdown_files(root)? {
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        for (index, snippet) in extract_witchy_blocks(&source).into_iter().enumerate() {
            let label = format!("{}#block-{}", display_from(root, &path), index + 1);
            let module = parser::parse_module(&snippet).map_err(|error| format!("{label}: {error}"))?;
            let report = async_migration_census(
                vec![("main".to_string(), module)],
                "main",
                &HashSet::new(),
            ).map_err(|error| format!("{label}: {error}"))?;
            record_report(label, report, &mut checked, &mut rejected, &mut totals);
        }
    }

    println!();
    println!("witchy-files\t{}", files.len());
    println!("checked\t{checked}");
    println!("rejected\t{}", rejected.len());
    for category in categories() {
        println!("{}\t{}", category.as_str(), totals.get(category.as_str()).copied().unwrap_or(0));
    }
    let migration_findings = categories().iter()
        .filter(|category| **category != Category::AsyncCallable)
        .map(|category| totals.get(category.as_str()).copied().unwrap_or(0))
        .sum::<usize>();
    println!("migration-findings\t{migration_findings}");
    println!("migration-complete\t{}", rejected.is_empty() && migration_findings == 0);
    if !rejected.is_empty() {
        println!();
        println!("compiler-rejections");
        for rejection in rejected { println!("{rejection}"); }
        return Err("one or more corpus sources failed checking; migration findings remain above".to_string());
    }
    if strict && migration_findings != 0 {
        return Err(format!("RFC-0161 strict census found {migration_findings} migration findings"));
    }
    Ok(())
}

fn categories() -> [Category; 10] {
    [
        Category::AsyncCallable,
        Category::TaskConstruction,
        Category::TaskConsumption,
        Category::LegacyHandle,
        Category::LegacyCancellation,
        Category::LegacySelect,
        Category::LegacyChannelUnbounded,
        Category::LegacyChannelBounded,
        Category::LegacyChannelDynamicCapacity,
        Category::LegacyReceiveQuiescence,
    ]
}

fn record_report(
    label: String,
    report: Census,
    checked: &mut usize,
    rejected: &mut Vec<String>,
    totals: &mut BTreeMap<&'static str, usize>,
) {
    if report.checked {
        *checked += 1;
    } else {
        rejected.push(format!("{label}: {}", report.check_error.as_deref().unwrap_or("unknown check failure")));
    }
    for (category, count) in report.counts() {
        *totals.entry(category).or_default() += count;
    }
    for finding in report.findings {
        println!("{label}\t{}\t{}\t{}", finding.line, finding.category.as_str(), finding.callee);
    }
}

fn collect_witchy_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() { return Ok(()); }
    for entry in std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read `{}`: {error}", directory.display()))?
    {
        let path = entry.map_err(|error| format!("cannot read entry in `{}`: {error}", directory.display()))?.path();
        if path.is_dir() {
            collect_witchy_files(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("witchy") {
            files.push(path);
        }
    }
    Ok(())
}

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![root.join("README.md"), root.join("CONTRIBUTING.md"), root.join("examples/README.md")];
    for directory in ["spec", "book/src"] {
        let path = root.join(directory);
        if !path.exists() { continue; }
        for entry in std::fs::read_dir(&path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?
        {
            let path = entry.map_err(|error| format!("cannot read entry in `{}`: {error}", directory))?.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") { files.push(path); }
        }
    }
    files.retain(|path| path.is_file());
    files.sort();
    Ok(files)
}

fn extract_witchy_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = None;
    for line in markdown.lines() {
        match &mut current {
            None if line.trim_end() == "```witchy" => current = Some(String::new()),
            Some(_) if line.trim_end() == "```" => blocks.push(current.take().expect("open block")),
            Some(body) => { body.push_str(line); body.push('\n'); }
            None => {}
        }
    }
    blocks
}

fn source_index(files: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut index = BTreeMap::new();
    for path in files {
        if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
            index.entry(stem.to_string()).or_insert_with(Vec::new).push(path.clone());
        }
    }
    index
}

fn link_file(path: &Path, source_index: &BTreeMap<String, Vec<PathBuf>>) -> Result<Census, String> {
    let stem = path.file_stem().and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid source path `{}`", path.display()))?.to_string();
    let mut modules = Vec::new();
    let mut loaded = HashSet::new();
    let mut user_modules = HashSet::new();
    let mut pending = VecDeque::from([(stem.clone(), path.to_path_buf())]);
    while let Some((name, source_path)) = pending.pop_front() {
        if !loaded.insert(name.clone()) { continue; }
        let source = match std::fs::read_to_string(&source_path) {
            Ok(source) => {
                if linker::std_source(&name) != Some(source.as_str()) { user_modules.insert(name.clone()); }
                source
            }
            Err(error) => linker::std_source(&name).map(str::to_string)
                .ok_or_else(|| format!("cannot read `{}`: {error}", source_path.display()))?,
        };
        let module: ast::Module = parser::parse_module(&source)
            .map_err(|error| format!("{}: {error}", source_path.display()))?;
        for import in &module.imports {
            if loaded.contains(import) { continue; }
            let sibling = source_path.parent().unwrap_or_else(|| Path::new(".")).join(format!("{import}.witchy"));
            let import_path = if sibling.exists() || linker::std_source(import).is_some() {
                sibling
            } else {
                dependency_source(&source_path, import)?
                    .or_else(|| source_index.get(import).and_then(|candidates| match candidates.as_slice() {
                        [only] => Some(only.clone()),
                        _ => None,
                    }))
                    .unwrap_or(sibling)
            };
            pending.push_back((import.clone(), import_path));
        }
        modules.push((name, module));
    }
    async_migration_census(modules, &stem, &user_modules)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Resolve a local package dependency before falling back to the global corpus
/// index. Package-local names are intentionally allowed to collide, so the
/// global index is only sound when it has exactly one candidate.
fn dependency_source(source: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    let Some(manifest) = source.ancestors().map(|directory| directory.join("witchy.toml"))
        .find(|candidate| candidate.is_file())
    else { return Ok(None); };
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("cannot read `{}`: {error}", manifest.display()))?;
    let document: toml::Value = toml::from_str(&text)
        .map_err(|error| format!("cannot parse `{}`: {error}", manifest.display()))?;
    let Some(path) = document.get("dependencies")
        .and_then(|dependencies| dependencies.get(name))
        .and_then(|dependency| dependency.get("path"))
        .and_then(toml::Value::as_str)
    else { return Ok(None); };
    let root = std::fs::canonicalize(manifest.parent().expect("manifest has a parent").join(path))
        .map_err(|error| format!("cannot resolve path dependency `{name}` from `{}`: {error}", manifest.display()))?;
    let bare = name.rsplit('/').next().unwrap_or(name);
    for candidate in [
        root.join("src").join(format!("{bare}.witchy")),
        root.join(format!("{bare}.witchy")),
        root.join("main.witchy"),
    ] {
        if candidate.is_file() { return Ok(Some(candidate)); }
    }
    let mut sources = Vec::new();
    collect_witchy_files(&root, &mut sources)?;
    sources.sort();
    match sources.as_slice() {
        [only] => Ok(Some(only.clone())),
        _ => Err(format!(
            "path dependency `{name}` from `{}` has no unambiguous entry source under `{}`",
            manifest.display(), root.display(),
        )),
    }
}

fn display_from(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::{dependency_source, extract_witchy_blocks};

    #[test]
    fn extracts_only_complete_witchy_fences() {
        assert_eq!(extract_witchy_blocks("```witchy\nfn main():\n    1\n```\n```text\nignored\n```"), vec!["fn main():\n    1\n"]);
    }

    #[test]
    fn resolves_a_package_local_path_dependency_before_the_global_index() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch").as_nanos();
        let root = std::env::temp_dir().join(format!("witchy-rfc0161-census-{}-{unique}", std::process::id()));
        let app = root.join("app");
        let dependency = root.join("dependency");
        std::fs::create_dir_all(app.join("src")).expect("app source directory");
        std::fs::create_dir_all(dependency.join("src")).expect("dependency source directory");
        std::fs::write(app.join("witchy.toml"), "[dependencies]\nwidget = { path = \"../dependency\" }\n").expect("manifest");
        std::fs::write(app.join("src/main.witchy"), "fn main():\n    1\n").expect("entry source");
        let expected = dependency.join("src/widget.witchy");
        std::fs::write(&expected, "fn value() -> Int:\n    1\n").expect("dependency source");
        assert_eq!(
            dependency_source(&app.join("src/main.witchy"), "widget").expect("dependency lookup"),
            Some(std::fs::canonicalize(expected).expect("canonical dependency source")),
        );
        std::fs::remove_dir_all(root).expect("remove temporary package root");
    }
}
