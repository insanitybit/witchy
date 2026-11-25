//! Compile the selected owner's test entry and adapters against one resolved source snapshot.
use super::{compiler_session::CheckedSource, files, methods, proof_digest, proof_session};
use crate::ast::{Declaration, Expression, Statement};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    path::Path,
    time::Instant,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Word,
    Input,
    ReadDir,
    WriteDir,
    /// A heap of the case's own: the test host gives every case a fresh store, so each gets
    /// an empty, bounded heap and none sees another's allocations. Nothing crosses the wire.
    Allocator,
    /// The same heap, owned: a fixture function takes it to keep in the record it returns.
    OwnedAllocator,
}
impl Slot {
    // The transport accepts these concrete host inputs. A checked Witchy provider can
    // build any language-level fixture from them, including a record with no host inputs.
    fn transport(ty: super::logical_body::Ty) -> Option<Self> {
        use super::{
            host::{Host, Right},
            logical_body::{Pointee, Ty},
        };
        match ty {
            Ty::U64 => Some(Self::Word),
            Ty::Borrow {
                mutable: false,
                pointee: Pointee::Input,
            } => Some(Self::Input),
            Ty::Borrow {
                mutable: false,
                pointee: Pointee::Host(Host::Dir(Right::Read)),
            } => Some(Self::ReadDir),
            Ty::Borrow {
                mutable: false,
                pointee: Pointee::Host(Host::Dir(Right::Write)),
            } => Some(Self::WriteDir),
            Ty::Borrow {
                mutable: false,
                pointee: Pointee::Allocator,
            } => Some(Self::Allocator),
            Ty::Allocator => Some(Self::OwnedAllocator),
            _ => None,
        }
    }
    /// Held by the adapter's `main` as a parameter of its own and lent to the case.
    fn object(self) -> bool {
        matches!(
            self,
            Self::ReadDir | Self::WriteDir | Self::Allocator | Self::OwnedAllocator
        )
    }
    fn label(self) -> &'static str {
        match self {
            Self::Word => "U64",
            Self::Input => "&Console<Read>",
            Self::ReadDir => "&Dir<Read>",
            Self::WriteDir => "&Dir<Write>",
            Self::Allocator => "&Allocator",
            Self::OwnedAllocator => "Allocator",
        }
    }
}

/// What a case or its fixture may take from the test host, as the refusal of anything else says.
const EXTERNAL: &str = "`U64` (a word the runner passes), `&Console<Read>` (bytes the runner passes), `&Dir<Read>` or `&Dir<Write>` (a directory the runner binds) and `&Allocator` or, in a fixture function, `Allocator` (a fresh, bounded heap for each case)";

struct Case {
    declaration: String,
    entry: String,
    span: std::ops::Range<usize>,
    provider: Option<(u32, String)>,
    fields: Vec<Field>,
    opening: Option<RecordPattern>,
    number: usize,
    module: u32,
    name: String,
    member: String,
    line: usize,
    slots: Vec<(String, Slot)>,
}

struct Field {
    name: String,
    ty: String,
    argument: String,
    /// Bind a mutable local after opening a nominal record, which yields immutable names.
    mutable_local: Option<String>,
}

struct RecordPattern {
    nominal: Option<String>,
    bindings: Vec<(String, String)>,
}
impl RecordPattern {
    fn bind(&self, value: crate::ast::Expr) -> Statement {
        match &self.nominal {
            Some(record) => Statement::Open {
                record: record.clone(),
                names: self.bindings.iter().map(|(_, n)| n.clone()).collect(),
                value,
            },
            None => Statement::OpenFields {
                fields: self.bindings.clone(),
                value,
                mutable: false,
                ty: None,
            },
        }
    }
}

fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Adapter {
    source: String,
    /// Offsets of callees generated for declarations hidden from authored code.
    callees: BTreeMap<usize, String>,
    /// Factory calls whose result is taken apart into independently owned arguments.
    openings: BTreeMap<usize, usize>,
    cases: Vec<(std::ops::Range<usize>, usize)>,
}

fn adapter(
    cases: &[Case],
    selected: &[usize],
    dirs: &[Slot],
    scheduler: Option<usize>,
    reporter: Option<usize>,
) -> Adapter {
    let mut origins = Vec::new();
    let mut callees = BTreeMap::new();
    let mut openings = BTreeMap::new();
    let mut code = String::new();
    if let Some(module) = reporter {
        writeln!(code, "import reporter {module}").unwrap();
    }
    if let Some(module) = scheduler {
        writeln!(code, "import runner {module}").unwrap();
    }
    for &index in selected {
        let case = &cases[index];
        if let Some((module, _)) = &case.provider {
            writeln!(code, "import factory{} {}", case.number, module).unwrap();
        }
        writeln!(code, "import target{} {}", case.number, case.module).unwrap();
    }
    code.push_str("fn main(control: Console<Read>, report: Console<Write>");
    for (i, d) in dirs.iter().enumerate() {
        write!(code, ", object{i}: {}", d.label().trim_start_matches('&')).unwrap();
    }
    code.push_str("):\n    let number = control.next_byte()\n");
    for &index in selected {
        let case = &cases[index];
        let start = code.len();
        writeln!(code, "    if number == {}:", case.number).unwrap();
        let mut dir = 0;
        let mut args = Vec::new();
        for (i, (_, slot)) in case.slots.iter().enumerate() {
            args.push(match slot {
                Slot::Word => {
                    writeln!(code,"        var word{i}: U64 = 0\n        for j{i} in 0..8:\n            word{i} = (word{i} << 8) | control.next_byte()").unwrap();
                    format!("word{i}")
                }
                Slot::Input => "&control".into(),
                Slot::OwnedAllocator => { dir += 1; format!("move object{}", dir - 1) }
                _ => { dir += 1; format!("&object{}",dir-1) }
            });
        }
        if let Some((_, name)) = &case.provider {
            code.push_str("        var fixture = ");
            if case.opening.is_some() {
                openings.insert(code.len(), index);
            }
            writeln!(code, "factory{}.{name}({})", case.number, args.join(", ")).unwrap();
            for (i, field) in case.fields.iter().enumerate() {
                if let Some(value) = &field.mutable_local {
                    writeln!(code, "        var lent{i} = {value}").unwrap();
                }
            }
            args = case.fields.iter().map(|f| f.argument.clone()).collect();
        }
        code.push_str("        reporter.report_result(");
        callees.insert(code.len(), case.entry.clone());
        writeln!(
            code,
            "invoke({}), &report)\n        return",
            args.join(", ")
        )
        .unwrap();
        origins.push((start..code.len(), index));
    }
    if scheduler.is_some() {
        code.push_str("    if number == 255:\n        var suite = runner.empty_tests()\n        let count = control.next_byte() + control.next_byte() * 256\n        for ticket in 0..count:\n            let chosen = control.next_byte()\n");
        if cases.is_empty() {
            code.push_str("            unreachable\n");
        }
        for c in cases {
            let schema = c
                .slots
                .iter()
                .map(|(n, t)| format!("{n}: {}", t.label()))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                code,
                "            if chosen == {}:\n                runner.add_test(&mut suite, {}, {})",
                c.number,
                quoted(&format!("{}::{}", c.member, c.name)),
                quoted(&schema)
            )
            .unwrap();
        }
        code.push_str("        ");
        callees.insert(code.len(), "#package_schedule".into());
        code.push_str(
            "invoke(suite, runner.grant_service(move report, move control))\n        return\n",
        );
    }
    code.push_str("    unreachable\n");
    Adapter {
        source: code,
        callees,
        openings,
        cases: origins,
    }
}

// The final custom section distinguishes these images even if they are passed to the release host.
fn section(catalog: &str) -> Result<Vec<u8>, String> {
    let digest = proof_digest::bytes(catalog.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = b"\0\x4d\x0cwitchy.tests".to_vec();
    out.extend(digest.bytes());
    Ok(out)
}

pub fn build_package_tests(
    input: &Path,
    output: &Path,
    coverage: bool,
    production: Option<&Path>,
) -> Result<String, String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = loop {
        let path = parent.join(format!(
            ".witchy-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let result = (|| {
        let catalog = build_snapshot(input, &temporary, coverage, production)?;
        std::fs::write(temporary.join("catalog.json"), &catalog).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, output)
            .map_err(|e| format!("publish test images to a fresh or empty directory: {e}"))?;
        Ok(catalog)
    })();
    if result.is_err() {
        std::fs::remove_dir_all(&temporary).map_err(|e| e.to_string())?;
    }
    result
}

fn source_identity(path: &Path) -> Result<std::path::PathBuf, String> {
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("package source entry must be a regular file".into());
    }
    path.canonicalize().map_err(|e| e.to_string())
}

fn build_snapshot(
    input: &Path,
    output: &Path,
    coverage: bool,
    production: Option<&Path>,
) -> Result<String, String> {
    let input = source_identity(input)?;
    if input.file_name().and_then(|n| n.to_str()) != Some("test.wy") {
        return Err("package tests start at test.wy".into());
    }
    let root = input.parent().unwrap();
    let snapshot = files::BuildInput::read_under(&input, Some(root))?;
    let parts = &snapshot.parts;
    let mut ordinary = None;
    let mut production_module = None;
    if let Some(path) = production {
        let path = source_identity(path)?;
        if !path.starts_with(root) || path == input {
            return Err("invalid production entry".into());
        }
        let start = Instant::now();
        let release = files::BuildInput::read_under(&path, Some(root))?;
        if release.parts.iter().any(|p| {
            p.local() && (p.identity == input || p.identity.starts_with(root.join("tests")))
        }) {
            return Err("production entry imports a package test member".into());
        }
        let mut session =
            proof_session::Session::publishing().map_err(|e| e.message.to_string())?;
        let (receipt, work) = files::build_image(
            &path,
            &output.join("production.wasm"),
            None,
            session.as_mut(),
            true,
            start,
            &release,
            None,
            &|| Ok(()),
            &release.source,
            Some((root, &[])),
        )?;
        production_module = parts
            .iter()
            .position(|p| p.identity == path)
            .map(|i| i as u32);
        ordinary = Some((path, release, receipt, work));
    }
    let starts = files::starts(parts);
    let names = files::names(parts, root);
    let mut graph_bytes = Vec::new();
    for (name, part) in names.iter().zip(parts) {
        for field in [name.as_bytes(), part.raw.as_bytes()] {
            graph_bytes.extend((field.len() as u64).to_be_bytes());
            graph_bytes.extend(field);
        }
        for (alias, home) in &part.edges {
            graph_bytes.extend(alias.bytes());
            graph_bytes.extend(home.to_be_bytes());
        }
    }
    graph_bytes.extend(snapshot.policy.as_deref().unwrap_or("").bytes());
    let graph = proof_digest::bytes(&graph_bytes).map_err(|e| e.to_string())?;
    let direct = super::compiler_session::optional_text(&input.with_extension("direct"))?;
    for p in parts.iter().filter(|p| p.local()) {
        for (_, home) in &p.edges {
            if let Some(name) = parts[*home as usize].package_name()
                && direct
                    .as_ref()
                    .is_some_and(|s| !s.lines().any(|edge| edge == name))
            {
                return Err(format!("root package does not directly require @{name}"));
            }
        }
    }
    if parts.iter().any(|p| p.local() && p.identity.starts_with(root.join("tests/reject"))) {
        return Err("a rejection fixture cannot be imported into the runnable suite".into());
    }
    let members: BTreeSet<_> = parts
        .iter()
        .enumerate()
        .filter(|(i, p)| p.local() && (names[*i] == "test.wy" || names[*i].starts_with("tests/")))
        .map(|(i, _)| i as u32)
        .collect();
    if let Some((path, _, _, _)) = &ordinary
        && let Some(authorized) =
            super::compiler_session::optional_text(&path.with_extension("direct"))?
    {
        for (i, p) in parts
            .iter()
            .enumerate()
            .filter(|(i, p)| p.local() && !members.contains(&(*i as u32)))
        {
            for (_, to) in &p.edges {
                if let Some(dep) = parts[*to as usize].package_name()
                    && !authorized.lines().any(|name| name == dep)
                {
                    return Err(format!(
                        "{} imports @{dep} outside its production dependencies",
                        names[i]
                    ));
                }
            }
        }
    }
    for (from, p) in parts.iter().enumerate() {
        if !members.contains(&(from as u32)) && p.edges.iter().any(|(_, to)| members.contains(to)) {
            return Err("ordinary modules cannot import test members".into());
        }
    }
    let parse = |text: &str, offsets: &[usize]| {
        crate::parser::parse_for_tests(
            text,
            offsets,
            &members,
            Some(starts.len() as u32 - 1),
            production_module,
        )
        .map_err(|e| files::render(text, parts, &e))
    };
    let discovery = parse(&snapshot.source, &starts)?;
    let edges: Vec<_> = parts
        .iter()
        .enumerate()
        .flat_map(|(i, p)| p.edges.iter().map(move |(n, m)| (i as u32, n.clone(), *m)))
        .collect();
    if edges != discovery.imports {
        return Err("test module imports must come from the resolved package graph".into());
    }
    let mut cases = Vec::new();
    let mut sites = BTreeMap::new();
    let mut groups: BTreeMap<Vec<Slot>, Vec<usize>> = BTreeMap::new();
    let lifted =
        methods::lift(discovery.clone()).map_err(|e| files::render(&snapshot.source, parts, &e))?;
    let diagnostic = |e| files::render(&snapshot.source, parts, &e);
    let declarations = super::check::reflect(&lifted).map_err(diagnostic)?;
    let types = &declarations.layouts;
    let reporter = parts.iter().position(|p| p.standard_module("testing"));
    let report = reporter.and_then(|m| declarations.declared(m as u32, "report_result"));
    let result_type = report.and_then(|(_, s)| s.params.first());
    for decl in &discovery.declarations {
        let Declaration::Function(f) = decl else {
            continue;
        };
        let Some(provider) = discovery.test_cases.get(&f.name) else {
            continue;
        };
        let signature = &declarations.signatures[&f.name];
        if Some(&signature.result) != result_type {
            return Err(format!(
                "{}: a case must return the result accepted by testing.report_result",
                f.name
            ));
        }
        let factory = provider
            .as_ref()
            .map(|(home, name)| {
                let (key, sig) = declarations
                    .declared(*home, name)
                    .ok_or_else(|| format!("unknown fixture provider {name} in selected module"))?;
                if discovery.test_cases.contains_key(key) || !sig.generics.is_empty() {
                    return Err(format!(
                        "fixture provider {name} must be an ordinary nongeneric function"
                    ));
                }
                if !sig.public && sig.module != f.module && !parts[sig.module as usize].local() {
                    return Err(format!("fixture provider {name} is private to its package"));
                }
                Ok(sig)
            })
            .transpose()?;
        use super::logical_body::Ty;
        let layout = if let Some(factory) = factory {
            Some(
                types
                    .get(factory.result)
                    .filter(|r| matches!(r.kind, super::semantic_types::Kind::Record(_)))
                    .ok_or_else(|| {
                        format!("{}: the fixture provider must return a record", f.name)
                    })?,
            )
        } else {
            None
        };
        // Moving one field out would consume the wrapper. Open it once in declaration
        // order when any argument takes ownership, then call in parameter order.
        let takes_owner = signature
            .params
            .iter()
            .any(|ty| !matches!(ty, Ty::Borrow { .. }) && !types.properties(*ty).copy);
        let opening = factory
            .filter(|_| takes_owner)
            .zip(layout)
            .map(|(factory, record)| RecordPattern {
                nominal: types
                    .record_name(factory.result)
                    .filter(|_| !types.is_anonymous(factory.result))
                    .map(str::to_owned),
                bindings: record
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| (field.name.clone(), format!("part{i}")))
                    .collect(),
            });
        let mut fields = Vec::new();
        for (i, (name, ty)) in signature.names.iter().zip(&signature.params).enumerate() {
            let mut mutable_local = None;
            let mut argument = String::new();
            if let Some(record) = layout {
                let field = record
                    .fields
                    .iter()
                    .position(|field| field.name == *name)
                    .ok_or_else(|| {
                        diagnostic(crate::diagnostic::Diagnostic::new(
                            "W0401",
                            f.span.clone(),
                            format!("the selected fixture has no field `{name}`"),
                        ))
                    })?;
                let field_ty = record.fields[field].ty;
                let place = opening.as_ref().map_or_else(
                    || format!("fixture.{name}"),
                    |o| o.bindings[field].1.clone(),
                );
                let take = if types.properties(field_ty).copy {
                    ""
                } else {
                    "move "
                };
                argument = match ty {
                    Ty::Borrow { mutable: true, .. }
                        if !matches!(field_ty, Ty::Borrow { .. }) && opening.is_some() =>
                    {
                        mutable_local = Some(format!("{take}{place}"));
                        format!("&mut lent{i}")
                    }
                    Ty::Borrow { mutable, .. } if !matches!(field_ty, Ty::Borrow { .. }) => {
                        format!("&{}{place}", if *mutable { "mut " } else { "" })
                    }
                    Ty::Borrow { .. } => place,
                    _ => format!("{take}{place}"),
                };
            }
            fields.push(Field {
                name: name.clone(),
                ty: types.type_name(*ty).map_err(diagnostic)?,
                argument,
                mutable_local,
            });
        }
        let external = factory.unwrap_or(signature);
        let mut slots = Vec::new();
        for (label, ty) in external.names.iter().zip(&external.params) {
            let Some(slot) = Slot::transport(*ty) else {
                let name = types.type_name(*ty).map_err(diagnostic)?;
                // At the case, by file and line: the case's own parameter, or its fixture's.
                let whose = if factory.is_some() { "the fixture of this case takes" } else { "this case takes" };
                return Err(diagnostic(crate::diagnostic::Diagnostic::new(
                    "W0401",
                    f.span.clone(),
                    format!(
                        "{whose} `{label}: {name}`, which the test host cannot supply. A case, or the fixture function it names with `using`, may take {EXTERNAL}; build any other value in a fixture function from those"
                    ),
                )));
            };
            slots.push((label.clone(), slot));
        }
        if slots.len() > 10
            || slots.iter().filter(|(_, s)| *s == Slot::Input).count() > 1
            || slots.iter().filter(|(_, s)| matches!(s, Slot::Allocator | Slot::OwnedAllocator)).count() > 1
        {
            return Err("case slot limit exceeded".into());
        }
        let dirs = slots
            .iter()
            .filter(|(_, s)| s.object())
            .map(|(_, s)| *s)
            .collect();
        let number = cases.len();
        if number >= 255 {
            return Err("at most 255 cases per package".into());
        }
        groups.entry(dirs).or_default().push(number);
        let member = names[f.module as usize].clone();
        let name = f.name.split('@').next().unwrap().to_owned();
        let line = snapshot.source[starts[f.module as usize]..f.span.start]
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            + 1;
        cases.push(Case {
            declaration: f.name.clone(),
            entry: format!("#test{number}"),
            span: f.span.clone(),
            provider: provider.clone(),
            fields,
            opening,
            number,
            module: f.module,
            name,
            member,
            line,
            slots,
        });
    }
    let schedule = discovery
        .declarations
        .iter()
        .any(|d| matches!(d, Declaration::Function(f) if f.name == "#package_schedule"));
    let runner = parts.iter().position(|p| p.standard_module("test_runner"));
    if schedule {
        let entry = discovery
            .declarations
            .iter()
            .find_map(|d| match d {
                Declaration::Function(f) if f.name == "#package_schedule" => Some(f),
                _ => None,
            })
            .unwrap();
        let runner = runner.ok_or("a package test main imports test_runner")? as u32;
        let service = |name| {
            declarations
                .declared(runner, name)
                .map(|(_, s)| s.result)
                .ok_or("missing standard test runner service")
        };
        let signature = &declarations.signatures[&entry.name];
        if signature.params != [service("empty_tests")?, service("grant_service")?]
            || signature.result != super::logical_body::Ty::Unit
            || !signature.generics.is_empty()
            || entry.suspends
            || entry.defaults.iter().any(Option::is_some)
        {
            return Err(
                "test main takes the Tests and TestGrants provided by test_runner and returns unit"
                    .into(),
            );
        }
    }
    if schedule {
        groups.entry(vec![]).or_default();
    }
    if groups.is_empty() {
        groups.insert(vec![], vec![]);
    }
    let mut catalogs = Vec::new();
    let mut images = Vec::new();
    for (group, (dirs, selected)) in groups.iter().enumerate() {
        let start = Instant::now();
        let code = adapter(
            &cases,
            selected,
            dirs,
            runner.filter(|_| schedule && dirs.is_empty()),
            reporter,
        );
        let mut source = snapshot.source.clone();
        source.push('\n');
        let mut offsets = starts.clone();
        offsets.push(source.len());
        let generated = source.len();
        source.push_str(&code.source);
        let mut syntax = parse(&source, &offsets)?;
        let adapter = starts.len() as u32;
        syntax.test_adapter = Some(adapter);
        syntax.test_reporter = report.map(|(key, _)| key.clone());
        if coverage {
            syntax.coverage_modules = parts
                .iter()
                .enumerate()
                .filter(|(i, p)| p.local() && !members.contains(&(*i as u32)))
                .map(|(i, _)| i as u32)
                .collect();
        }
        for d in &mut syntax.declarations {
            let Declaration::Function(f) = d else {
                continue;
            };
            if let Some(c) = cases
                .iter()
                .find(|c| f.module == c.module && f.name == c.declaration)
            {
                f.name.clone_from(&c.entry);
            }
            if f.module == adapter && f.name == "main" {
                for statement in &mut f.body {
                    if let Statement::If { yes, .. } = statement {
                        for binding in yes.iter_mut() {
                            if let Statement::Bind { value, .. } = binding
                                && let Some(pattern) = value
                                    .span
                                    .start
                                    .checked_sub(generated)
                                    .and_then(|at| code.openings.get(&at))
                                    .and_then(|i| cases[*i].opening.as_ref())
                            {
                                *binding = pattern.bind(value.clone());
                            }
                        }
                    }
                    statement.visit_exprs_mut(&mut |e| {
                        if let Expression::Call(target, _) = &mut e.kind {
                            if let Some(key) = target
                                .span
                                .start
                                .checked_sub(generated)
                                .and_then(|offset| code.callees.get(&offset))
                            {
                                target.kind = Expression::Name(key.clone());
                            }
                        }
                        false
                    });
                }
            }
        }
        for (from, _, to) in &syntax.imports {
            if (*from == adapter || members.contains(from))
                && parts.get(*to as usize).is_some_and(|p| p.local())
            {
                syntax.test_edges.insert((*from, *to));
            }
        }
        let checked = methods::lift(syntax)
            .and_then(CheckedSource::from_syntax)
            .map_err(|mut e| {
                if let Some(offset) = e.span.start.checked_sub(generated)
                    && let Some((_, i)) =
                        code.cases.iter().find(|(range, _)| range.contains(&offset))
                {
                    e.span = cases[*i].span.clone();
                }
                files::render(&source, parts, &e)
            })?;
        let uses = super::caps::file_uses(
            &checked.logical,
            super::caps::main_grant(&checked.syntax),
            &names,
        );
        if uses.iter().any(|u| {
            members.iter().any(|m| names[*m as usize] == u.file) && !u.unchecked.is_empty()
        }) {
            return Err("test members cannot use unchecked memory operations".into());
        }
        if let Some((path, release, _, _)) = &ordinary
            && let Some(rules) = &release.policy
        {
            let base = path.parent().unwrap();
            let policy_names = files::names(parts, base);
            // Apply the production ceiling to the target code and its dependencies.
            // A fixture-only dependency has its own ceiling in the suite's policy.
            let mut todo: Vec<_> = parts
                .iter()
                .enumerate()
                .filter(|(i, p)| {
                    !members.contains(&(*i as u32))
                        && (p.local() || release.parts.iter().any(|r| r.identity == p.identity))
                })
                .map(|(i, _)| i)
                .collect();
            let mut allowed = BTreeSet::new();
            while let Some(i) = todo.pop() {
                if allowed.insert(policy_names[i].clone()) {
                    todo.extend(parts[i].edges.iter().map(|(_, to)| *to as usize));
                }
            }
            let mut target_uses = super::caps::file_uses(
                &checked.logical,
                super::caps::main_grant(&checked.syntax),
                &policy_names,
            );
            target_uses.retain(|u| allowed.contains(&u.file));
            let mut pins: BTreeMap<_, _> =
                files::digests(&release.parts, base)?.into_iter().collect();
            pins.extend(
                files::digests(parts, base)?
                    .into_iter()
                    .filter(|(n, _)| allowed.contains(n)),
            );
            let mut violations = super::caps::check_policy(&target_uses, rules)?;
            violations.extend(super::caps::check_digests(
                &pins.into_iter().collect::<Vec<_>>(),
                rules,
            )?);
            if !violations.is_empty() {
                return Err(violations.join("\n"));
            }
        }
        if sites.is_empty() {
            sites = site_catalog(&checked.logical.assertion_sites, &snapshot, &names, &cases)?;
        }
        let mut group_catalog = Vec::new();
        for &index in selected {
            let c = &cases[index];
            let id = quoted(&format!("{}::{}", c.member, c.name));
            let slots = c
                .slots
                .iter()
                .map(|(n, s)| format!("{{\"name\":{},\"type\":{}}}", quoted(n), quoted(s.label())))
                .collect::<Vec<_>>()
                .join(",");
            let parameters = c
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{{\"name\":{},\"type\":{}}}",
                        quoted(&f.name),
                        quoted(&f.ty)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            group_catalog.push(format!("{{\"id\":{id},\"line\":{},\"group\":{group},\"number\":{},\"parameters\":[{parameters}],\"slots\":[{slots}]}}",c.line,c.number));
        }
        let catalog = group_catalog.join(",");
        let marker = section(&catalog)?;
        catalogs.extend(group_catalog);
        let dest = output.join(format!("{group}.wasm"));
        let mut session =
            proof_session::Session::publishing().map_err(|e| e.message.to_string())?;
        let (receipt, work) = files::build_image(
            &input,
            &dest,
            None,
            session.as_mut(),
            true,
            start,
            &snapshot,
            Some(checked),
            &|| Ok(()),
            &source,
            Some((root, &marker)),
        )?;
        let image_id = proof_digest::bytes(&std::fs::read(&dest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        images.push(format!(
            "{{\"sha256\":\"{image_id}\",\"index\":\"{}\",\"receipt\":{receipt},\"work\":{}}}",
            std::str::from_utf8(&marker[15..]).unwrap(),
            work.json()
        ));
    }
    let sites = sites
        .into_iter()
        .map(|(n, site)| format!("\"{n}\":{site}"))
        .collect::<Vec<_>>()
        .join(",");
    let production = ordinary.map_or("null".to_owned(), |(_, _, receipt, work)| {
        format!("{{\"receipt\":{receipt},\"work\":{}}}", work.json())
    });
    Ok(format!(
        "{{\"v\":1,\"production\":{production},\"graph\":\"{graph}\",\"schedule\":{schedule},\"cases\":[{}],\"modules\":[{}],\"sites\":{{{sites}}}}}",
        catalogs.join(","),
        images.join(",")
    ))
}

fn site_catalog(
    checked: &BTreeMap<(usize, usize), (u32, String, String)>,
    snapshot: &files::BuildInput,
    names: &[String],
    cases: &[Case],
) -> Result<BTreeMap<u64, String>, String> {
    let mut sites = BTreeMap::new();
    let mut occurrences = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for (&(start, end), (module, function, label)) in checked {
        let Some(part) = snapshot.parts.get(*module as usize) else {
            continue;
        };
        let Some(before) = snapshot.source.get(part.start..start) else {
            continue;
        };
        let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
        let declaration = cases
            .iter()
            .find(|c| c.module == *module && c.entry == *function)
            .map_or(function.as_str(), |c| c.name.as_str());
        let member = &names[*module as usize];
        let prefix = format!("{member}::{declaration}");
        let text = snapshot
            .source
            .get(start..end)
            .ok_or("invalid assertion span")?;
        let key = if label.is_empty() {
            let mut frame = Vec::new();
            for t in crate::lexer::lex(text).map_err(|e| e.message.to_string())? {
                use crate::token::Kind;
                if matches!(
                    t.kind,
                    Kind::End | Kind::Indent | Kind::Dedent | Kind::Newline
                ) {
                    continue;
                }
                let word = &text.as_bytes()[t.span];
                frame.extend((word.len() as u32).to_be_bytes());
                frame.extend(word);
            }
            let digest = proof_digest::bytes(&frame).map_err(|e| e.to_string())?;
            let n = occurrences
                .entry((prefix.clone(), digest.clone()))
                .or_insert(0);
            let key = format!("{prefix}@{digest}.{n}");
            *n += 1;
            key
        } else {
            format!("{prefix}@{label}")
        };
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate assertion site {key}"));
        }
        let ordinal = ((start as u64) << 21) + end as u64 + 1;
        let raw_start = part
            .raw
            .split_inclusive('\n')
            .take(line - 1)
            .map(str::len)
            .sum::<usize>()
            + before.rsplit('\n').next().unwrap().len();
        sites.insert(ordinal,format!("{{\"member\":{},\"line\":{line},\"column\":{col},\"start\":{raw_start},\"end\":{},\"key\":{}}}",quoted(member),raw_start+text.len(),quoted(&key)));
    }
    Ok(sites)
}

/// Compile one negative example in its ordinary consumer context. The supervisor owns
/// the temporary output and compares the primary diagnostic with the expected site.
pub fn check_test_consumer(input: &Path, owner: &Path, output: &Path) -> Result<(), String> {
    let root = owner.canonicalize().map_err(|e| e.to_string())?;
    let entry = source_identity(input)?;
    if !entry.starts_with(root.join("tests/reject")) {
        return Err("invalid rejection entry".into());
    }
    let start = Instant::now();
    let snapshot = files::BuildInput::read_under(&entry, Some(&root))?;
    if snapshot.parts.iter().any(|p| {
        p.identity != entry
            && p.local()
            && (p.identity == root.join("test.wy") || p.identity.starts_with(root.join("tests")))
    }) {
        return Err("a consumer rejection cannot import package test members".into());
    }
    let mut session = proof_session::Session::publishing().map_err(|e| e.message.to_string())?;
    files::build_image(
        &entry,
        output,
        None,
        session.as_mut(),
        true,
        start,
        &snapshot,
        None,
        &|| Ok(()),
        &snapshot.source,
        Some((&root, &[])),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejection_members_cannot_be_collected_by_an_import() {
        let root = std::env::temp_dir().join(format!("witchy-reject-role-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let entry = root.join("test.wy");
        fs::create_dir_all(root.join("tests/reject")).unwrap();
        fs::write(&entry, "import \"tests/reject/example.wy\"\n").unwrap();
        fs::write(root.join("tests/reject/example.wy"), "import testing\ntest fn number() -> Result<(), Failure>:\n    return testing.ok(true)\n").unwrap();
        let result = build_package_tests(&entry, &root.join("images"), false, None);
        fs::remove_dir_all(root).unwrap();
        assert!(result.unwrap_err().contains("rejection fixture"));
    }

    /// A case that takes `&Allocator` is lent a heap the adapter's `main` holds as a parameter
    /// of its own (the test host grants it, fresh in each case's store); a fixture function
    /// that takes `Allocator` is given it to keep. Neither crosses the wire as an input.
    #[test]
    fn a_case_is_lent_a_heap_and_a_fixture_may_own_it() {
        use crate::compiler::logical_body::{Pointee, Ty};
        let lent = Ty::Borrow { mutable: false, pointee: Pointee::Allocator };
        assert!(Slot::transport(lent) == Some(Slot::Allocator));
        assert!(Slot::transport(Ty::Allocator) == Some(Slot::OwnedAllocator));
        assert!(Slot::transport(Ty::Borrow { mutable: true, pointee: Pointee::Allocator }).is_none());
        assert!(Slot::Allocator.object() && Slot::OwnedAllocator.object() && !Slot::Word.object());
        let case = |number: usize, slot: Slot, provider: Option<(u32, String)>| Case {
            declaration: format!("case{number}"),
            entry: format!("#test{number}"),
            span: 0..0,
            provider,
            fields: vec![Field { name: "allocator".into(), ty: "&Allocator".into(), argument: "&fixture.allocator".into(), mutable_local: None }],
            opening: None,
            number,
            module: 0,
            name: format!("case{number}"),
            member: "test.wy".into(),
            line: 1,
            slots: vec![("count".into(), Slot::Word), ("allocator".into(), slot)],
        };
        let cases = [case(0, Slot::Allocator, None), case(1, Slot::OwnedAllocator, Some((1, "owned".into())))];
        let borrowed = adapter(&cases, &[0], &[Slot::Allocator], None, Some(2)).source;
        assert!(borrowed.contains("fn main(control: Console<Read>, report: Console<Write>, object0: Allocator):"), "{borrowed}");
        assert!(borrowed.contains("invoke(word0, &object0), &report)"), "{borrowed}");
        let owned = adapter(&cases, &[1], &[Slot::OwnedAllocator], None, Some(2)).source;
        assert!(owned.contains("object0: Allocator):"), "{owned}");
        assert!(owned.contains("var fixture = factory1.owned(word0, move object0)"), "{owned}");
        assert!(owned.contains("invoke(&fixture.allocator), &report)"), "{owned}");
        assert!(EXTERNAL.contains("`&Allocator`") && EXTERNAL.contains("`&Dir<Write>`") && EXTERNAL.contains("`U64`"));
    }
}
