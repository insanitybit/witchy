//! Type-resolved RFC-0087 migration census for the repository corpus.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use witchy::{ast, parser, pipeline};
use witchy_syntax::linker;
use witchy_types::migration::{Category, Census};

fn main() {
    if let Err(error) = run() {
        eprintln!("rfc0087-census: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let root = Path::new(&root);
    let mut files = Vec::new();
    for directory in ["std", "examples", "projects", "benchmarks"] {
        collect_witchy_files(&root.join(directory), &mut files)?;
    }
    files.sort();
    let source_index = source_index(&files);

    let mut totals: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut declarations = 0usize;
    let mut calls = 0usize;
    let mut checked = 0usize;
    let mut rejected = Vec::new();

    println!("path\tline\tcategory\tcallee\tparameter\troot");
    let mut parse_cache: BTreeMap<String, ast::Module> = BTreeMap::new();
    for path in &files {
        let report = link_file(path, &source_index, &mut parse_cache)?;
        record_report(
            display_from(root, path),
            report,
            &mut declarations,
            &mut calls,
            &mut checked,
            &mut rejected,
            &mut totals,
        );
    }

    let markdown = markdown_files(root)?;
    let mut markdown_blocks = 0usize;
    let no_user_modules = HashSet::new();
    for path in &markdown {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        for (index, snippet) in extract_witchy_blocks(&source).into_iter().enumerate() {
            markdown_blocks += 1;
            let label = format!("{}#block-{}", display_from(root, path), index + 1);
            let module = parser::parse_module(&snippet).map_err(|e| format!("{label}: {e}"))?;
            let report = pipeline::migration_census(
                vec![("main".to_string(), module)],
                "main",
                &no_user_modules,
            )
                .map_err(|e| format!("{label}: {e}"))?;
            record_report(
                label,
                report,
                &mut declarations,
                &mut calls,
                &mut checked,
                &mut rejected,
                &mut totals,
            );
        }
    }

    println!();
    println!("witchy-files\t{}", files.len());
    println!("markdown-blocks\t{markdown_blocks}");
    println!("checked\t{checked}");
    println!("rejected\t{}", rejected.len());
    println!("var-declarations\t{declarations}");
    println!("resolved-var-calls\t{calls}");
    for category in [
        Category::MechanicalSelfReassignment,
        Category::ExpressionPositionMutator,
        Category::ImmutableVarArgument,
        Category::TemporaryVarArgument,
        Category::AuxiliaryResultDiscard,
    ] {
        println!("{}\t{}", category.as_str(), totals[category.as_str()]);
    }

    if !rejected.is_empty() {
        println!();
        println!("compiler-rejections");
        for rejection in &rejected {
            println!("{rejection}");
        }
        eprintln!(
            "rfc0087-census: {} corpus source(s) failed compiler checking; \
             see the compiler-rejections section",
            rejected.len()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record_report(
    label: String,
    report: Census,
    declarations: &mut usize,
    calls: &mut usize,
    checked: &mut usize,
    rejected: &mut Vec<String>,
    totals: &mut BTreeMap<&'static str, usize>,
) {
    *declarations += report.var_declarations;
    *calls += report.resolved_var_calls;
    if report.checked {
        *checked += 1;
    } else {
        rejected.push(format!(
            "{label}: {}",
            report.check_error.as_deref().unwrap_or("unknown check failure")
        ));
    }
    for (category, count) in report.counts() {
        *totals.entry(category).or_default() += count;
    }
    for finding in report.findings {
        println!(
            "{label}\t{}\t{}\t{}\t{}\t{}",
            finding.line,
            finding.category.as_str(),
            finding.callee,
            finding
                .parameter_name
                .as_deref()
                .map(|name| match finding.parameter {
                    Some(position) => format!("{position}:{name}"),
                    None => name.to_string(),
                })
                .unwrap_or_else(|| "-".to_string()),
            finding.root.as_deref().unwrap_or("-"),
        );
    }
}

fn collect_witchy_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|e| format!("cannot read `{}`: {e}", directory.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|e| format!("cannot read entry in `{}`: {e}", directory.display()))?
            .path();
        if path.is_dir() {
            collect_witchy_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("witchy") {
            files.push(path);
        }
    }
    Ok(())
}

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![
        root.join("README.md"),
        root.join("CONTRIBUTING.md"),
        root.join("examples/README.md"),
    ];
    for directory in ["spec", "book/src"] {
        let path = root.join(directory);
        let entries =
            std::fs::read_dir(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read entry in `{}`: {e}", path.display()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn extract_witchy_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = None;
    for line in markdown.lines() {
        match &mut current {
            None if line.trim_end() == "```witchy" => current = Some(String::new()),
            Some(_) if line.trim_end() == "```" => {
                blocks.push(current.take().expect("open block"));
            }
            Some(body) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {}
        }
    }
    blocks
}

fn source_index(files: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut index: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in files {
        if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
            index.entry(stem.to_string()).or_default().push(path.clone());
        }
    }
    index
}

fn link_file(
    path: &Path,
    source_index: &BTreeMap<String, Vec<PathBuf>>,
    parse_cache: &mut BTreeMap<String, ast::Module>,
) -> Result<Census, String> {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid source path `{}`", path.display()))?
        .to_string();
    let mut modules = Vec::new();
    let mut loaded = HashSet::new();
    let mut user_modules = HashSet::new();
    let mut queue = VecDeque::from([(stem.clone(), path.to_path_buf())]);

    while let Some((name, source_path)) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        let source = match std::fs::read_to_string(&source_path) {
            Ok(source) => {
                if linker::std_source(&name) != Some(source.as_str()) {
                    user_modules.insert(name.clone());
                }
                source
            }
            Err(error) => linker::std_source(&name)
                .map(str::to_string)
                .ok_or_else(|| format!("cannot read `{}`: {error}", source_path.display()))?,
        };
        // Every corpus file transitively re-imports the same shared modules
        // (std above all — ~49 modules, imported by nearly every one of the
        // ~290 corpus files). Re-parsing identical source text per file made
        // this the single slowest test in the suite (~885s measured
        // 2026-08-10, dwarfing everything else). Cache by the exact source
        // TEXT, not name or path: parsing is a pure function of the text, so
        // this is unconditionally correct even for two different projects
        // that happen to define a same-named-but-different-content local
        // module (their text differs, so they get distinct cache entries).
        let module = if let Some(cached) = parse_cache.get(&source) {
            cached.clone()
        } else {
            let parsed = parser::parse_module(&source)
                .map_err(|e| format!("{}: {e}", source_path.display()))?;
            parse_cache.insert(source, parsed.clone());
            parsed
        };
        for import in &module.imports {
            if !loaded.contains(import) {
                let sibling = source_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!("{import}.witchy"));
                let import_path = if sibling.exists() || linker::std_source(import).is_some() {
                    sibling
                } else {
                    dependency_source(&source_path, import)?
                        .or_else(|| {
                            source_index
                                .get(import)
                                .and_then(|candidates| match candidates.as_slice() {
                                    [only] => Some(only.clone()),
                                    _ => None,
                                })
                        })
                        .unwrap_or(sibling)
                };
                queue.push_back((import.clone(), import_path));
            }
        }
        modules.push((name, module));
    }

    let report = pipeline::migration_census(modules, &stem, &user_modules)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(report)
}

fn dependency_source(source: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    let Some(manifest) = source
        .ancestors()
        .map(|directory| directory.join("witchy.toml"))
        .find(|candidate| candidate.is_file())
    else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest.display()))?;
    let document: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("cannot parse `{}`: {e}", manifest.display()))?;
    let Some(path) = document
        .get("dependencies")
        .and_then(|dependencies| dependencies.get(name))
        .and_then(|dependency| dependency.get("path"))
        .and_then(toml::Value::as_str)
    else {
        return Ok(None);
    };
    let root = manifest
        .parent()
        .expect("manifest always has a parent")
        .join(path);
    let bare = name.rsplit('/').next().unwrap_or(name);
    for candidate in [
        root.join("src").join(format!("{bare}.witchy")),
        root.join(format!("{bare}.witchy")),
        root.join("main.witchy"),
    ] {
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    let mut sources = Vec::new();
    collect_witchy_files(&root, &mut sources)?;
    sources.sort();
    match sources.as_slice() {
        [only] => Ok(Some(only.clone())),
        _ => Err(format!(
            "path dependency `{name}` from `{}` has no unambiguous entry source under `{}`",
            manifest.display(),
            root.display()
        )),
    }
}

fn display_from(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}
