//! Loopback development server for RFC-0109.
//!
//! The server publishes only complete last-good artifact directories. Reload
//! events are authenticated with a per-process token and carry content-derived
//! build identities. State preservation is deliberately unavailable until the
//! compiler can prove the snapshot/model/authority compatibility contract.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use super::{
    check_project_development, write_development, WebCommandError,
};

mod static_site;

const CLIENT_PATH: &str = "/__witchy/client.mjs";
const EVENTS_PATH: &str = "/__witchy/events";
const DIAGNOSTICS_PREFIX: &str = "/__witchy/diagnostics/";
const SOURCES_PREFIX: &str = "/__witchy/sources/";
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Options {
    host: String,
    port: u16,
    root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct Diagnostic {
    generation: u64,
    phase: &'static str,
    severity: &'static str,
    code: &'static str,
    message: String,
    source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SwapContract {
    application: String,
    model: String,
    authorization: String,
    template: String,
    snapshot_format: u64,
    max_snapshot_bytes: u64,
    migrations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputFingerprint {
    compiler: String,
    template: String,
    style: String,
    assets: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Invalidation {
    compiler: bool,
    template: bool,
    style: bool,
    assets: bool,
}

impl Invalidation {
    fn between(previous: &InputFingerprint, next: &InputFingerprint) -> Self {
        Self {
            compiler: previous.compiler != next.compiler,
            template: previous.template != next.template,
            style: previous.style != next.style,
            assets: previous.assets != next.assets,
        }
    }

    fn labels(self) -> Vec<&'static str> {
        [
            (self.compiler, "compiler"),
            (self.template, "template"),
            (self.style, "style"),
            (self.assets, "assets"),
        ]
        .into_iter()
        .filter_map(|(changed, label)| changed.then_some(label))
        .collect()
    }
}

#[derive(Clone, Debug)]
struct BuildWork {
    recompiled: bool,
    invalidated: Vec<&'static str>,
    elapsed_ms: u64,
}

#[derive(Clone, Debug)]
struct Published {
    generation: u64,
    build_id: String,
    assets: Vec<String>,
    artifact_root: PathBuf,
    index: Vec<u8>,
    source_map: Vec<u8>,
    wasm_path: String,
    swap: Option<SwapContract>,
    decision: &'static str,
    reason: String,
    checked: super::CheckedWeb,
    work: BuildWork,
    diagnostic: Option<Diagnostic>,
}


pub(super) fn command(args: &[String]) -> Result<String, WebCommandError> {
    let options = parse_options(args)?;
    let project = super::load_project(&options.root)?;
    if project.delivery == super::Delivery::Static {
        return static_site::command(options, project);
    }
    let checked = check_project_development(&options.root)?;
    let project_root = checked.project.root.clone();
    let cache = project_root.join(".witchy").join("web");
    std::fs::create_dir_all(&cache).map_err(|error| {
        WebCommandError::failure(format!("cannot create `{}`: {error}", cache.display()))
    })?;
    let active = cache.join("active");
    let token = session_token()?;
    let mut published = publish(
        &checked,
        &active,
        &token,
        1,
        None,
        None,
        BuildWork {
            recompiled: true,
            invalidated: vec!["compiler", "template", "style", "assets"],
            elapsed_ms: 0,
        },
    )?;
    let mut fingerprint = input_fingerprint(&checked.project, Some(&checked.compiler_sources))?;

    let address = format!("{}:{}", options.host, options.port);
    let server = Server::http(&address).map_err(|error| {
        WebCommandError::failure(format!("cannot bind development server to `{address}`: {error}"))
    })?;
    let exposed = server.server_addr().to_string();
    if !is_loopback_host(&options.host) {
        eprintln!(
            "warning: Witchy development server is exposed on {exposed}; \
             its session token protects development endpoints, not the application itself"
        );
    }
    println!("Witchy dev serving {} at http://{exposed}", checked.project.name);

    loop {
        if let Some(request) = server.recv_timeout(Duration::from_millis(150)).map_err(|error| {
            WebCommandError::failure(format!("development server receive failed: {error}"))
        })? {
            if let Err(error) = respond(request, &token, &published) {
                eprintln!("Witchy dev request failed: {error}");
            }
        }
        match input_fingerprint(
            &published.checked.project,
            Some(&published.checked.compiler_sources),
        ) {
            Ok(next) if next != fingerprint => {
                let invalidation = Invalidation::between(&fingerprint, &next);
                fingerprint = next;
                let generation = published.generation.saturating_add(1);
                match rebuild(
                    &project_root,
                    &active,
                    &token,
                    generation,
                    &published,
                    invalidation,
                ) {
                    Ok(next) => {
                        let decision = next.decision;
                        published = next;
                        println!("Witchy dev generation {generation}: {decision}");
                    }
                    Err(diagnostic) => {
                        published.diagnostic = Some(diagnostic);
                        eprintln!("Witchy dev generation {generation}: build failed; keeping last good build");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                let generation = published.generation.saturating_add(1);
                published.diagnostic = Some(bounded_diagnostic(
                    generation,
                    &project_root,
                    error.to_string(),
                ));
            }
        }
    }
}

fn parse_options(args: &[String]) -> Result<Options, WebCommandError> {
    let mut host = "127.0.0.1".to_string();
    let mut port = 3000_u16;
    let mut root = None;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--host" => {
                host = rest.next().cloned().ok_or_else(|| {
                    WebCommandError::usage("`--host` requires an address")
                })?;
            }
            value if value.starts_with("--host=") => {
                host = value["--host=".len()..].to_string();
            }
            "--port" => {
                let value = rest.next().ok_or_else(|| {
                    WebCommandError::usage("`--port` requires an integer")
                })?;
                port = parse_port(value)?;
            }
            value if value.starts_with("--port=") => {
                port = parse_port(&value["--port=".len()..])?;
            }
            value if value.starts_with('-') => {
                return Err(WebCommandError::usage(format!("unknown dev option `{value}`")));
            }
            value if root.is_none() => root = Some(PathBuf::from(value)),
            value => {
                return Err(WebCommandError::usage(format!(
                    "unexpected dev argument `{value}`"
                )));
            }
        }
    }
    if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
        return Err(WebCommandError::usage("`--host` is not a valid bind host"));
    }
    Ok(Options { host, port, root: root.unwrap_or_else(|| PathBuf::from(".")) })
}

fn parse_port(value: &str) -> Result<u16, WebCommandError> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| WebCommandError::usage("`--port` must be in 1..=65535"))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn session_token() -> Result<String, WebCommandError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| {
        WebCommandError::failure(format!("cannot create development session identity: {error}"))
    })?;
    Ok(hex(&bytes))
}

fn publish(
    checked: &super::CheckedWeb,
    active: &Path,
    token: &str,
    generation: u64,
    diagnostic: Option<Diagnostic>,
    previous: Option<&Published>,
    work: BuildWork,
) -> Result<Published, WebCommandError> {
    write_development(checked, active)?;
    let manifest_path = active.join("witchy-web-manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|error| {
            WebCommandError::failure(format!("cannot read `{}`: {error}", manifest_path.display()))
        })?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let build_id = manifest["buildIdentity"]
        .as_str()
        .ok_or_else(|| WebCommandError::failure("development manifest has no build identity"))?
        .to_string();
    let assets = manifest["assets"]
        .as_array()
        .ok_or_else(|| WebCommandError::failure("development manifest has no asset graph"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| WebCommandError::failure("development asset identity is not text"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let wasm_path = assets
        .iter()
        .find(|asset| asset.ends_with(".wasm"))
        .map(|asset| format!("/assets/{asset}"))
        .ok_or_else(|| WebCommandError::failure("development manifest has no Wasm asset"))?;
    let swap = swap_contract(&manifest)?;
    let (mut decision, mut reason) =
        swap_decision(previous.and_then(|value| value.swap.as_ref()), swap.as_ref());
    if work.invalidated.contains(&"style") {
        decision = "reload";
        reason = "stylesheet changed".into();
    } else if work.invalidated.contains(&"assets") {
        decision = "reload";
        reason = "document or public assets changed".into();
    }
    let index = client_index(active, token, generation, &build_id)?;
    let mut source_map = json!({
        "schema": "witchy.web.source-map.v1",
        "buildIdentity": &build_id,
        "sourcesEmbedded": false,
        "expressionSpanEncoding": {
            "columns": "unicode-scalar",
            "end": "exclusive",
        },
        "generatedOrigins": &checked.source_origins,
        "taggedLiterals": &checked.tagged_origins,
        "sourceFunctions": &checked.source_functions,
        "wasmFunctions": &checked.wasm_functions,
        "templates": super::compiler_template_registry(&checked.templates, true),
        "operationMappings": super::compiler_operation_source_registry(&checked.templates),
    });
    super::normalize_source_modules(&mut source_map, &checked.project.root);
    let source_map = serde_json::to_vec(&source_map)
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    Ok(Published {
        generation,
        build_id,
        assets,
        artifact_root: active.to_path_buf(),
        index,
        source_map,
        wasm_path,
        swap,
        decision,
        reason,
        checked: checked.clone(),
        work,
        diagnostic,
    })
}

fn rebuild(
    project_root: &Path,
    active: &Path,
    token: &str,
    generation: u64,
    previous: &Published,
    invalidation: Invalidation,
) -> Result<Published, Diagnostic> {
    let started = std::time::Instant::now();
    let result = if invalidation.compiler {
        check_project_development(project_root)
    } else {
        refresh_non_compiler_inputs(&previous.checked, project_root)
    };
    result
        .and_then(|checked| {
            let mut published = publish(
                &checked,
                active,
                token,
                generation,
                None,
                Some(previous),
                BuildWork {
                    recompiled: invalidation.compiler,
                    invalidated: invalidation.labels(),
                    elapsed_ms: 0,
                },
            )?;
            published.work.elapsed_ms =
                started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            Ok(published)
        })
        .map_err(|error| bounded_diagnostic(generation, project_root, error.to_string()))
}

fn refresh_non_compiler_inputs(
    previous: &super::CheckedWeb,
    project_root: &Path,
) -> Result<super::CheckedWeb, WebCommandError> {
    let project = super::load_project(project_root)?;
    if project.entry != previous.project.entry {
        return Err(WebCommandError::failure(
            "compiler input path changed without compiler invalidation",
        ));
    }
    let protocol_manifest = super::read_protocol_manifest(&project.manifest)?;
    let mut checked = previous.clone();
    checked.project = project;
    checked.protocol_manifest = protocol_manifest;
    Ok(checked)
}

fn swap_contract(manifest: &serde_json::Value) -> Result<Option<SwapContract>, WebCommandError> {
    let Some(development) = manifest.get("development") else {
        return Ok(None);
    };
    let snapshot_format = development["snapshotFormat"]
        .as_u64()
        .ok_or_else(|| WebCommandError::failure("development snapshot format is missing"))?;
    if snapshot_format == 0 {
        return Ok(None);
    }
    let text = |key: &str| {
        development
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                WebCommandError::failure(format!(
                    "development manifest has no `{key}` identity"
                ))
            })
    };
    Ok(Some(SwapContract {
        application: text("applicationIdentity")?,
        model: text("modelSchema")?,
        authorization: text("authorizationSchema")?,
        template: text("templateSchema")?,
        snapshot_format,
        max_snapshot_bytes: development["maxSnapshotBytes"]
            .as_u64()
            .ok_or_else(|| WebCommandError::failure("development snapshot limit is missing"))?,
        migrations: development
            .get("migrationSchemas")
            .and_then(serde_json::Value::as_array)
            .map(|schemas| {
                schemas
                    .iter()
                    .map(|schema| {
                        schema.as_str().map(str::to_string).ok_or_else(|| {
                            WebCommandError::failure(
                                "development migration schema must be a string",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default(),
    }))
}

fn swap_decision(
    previous: Option<&SwapContract>,
    next: Option<&SwapContract>,
) -> (&'static str, String) {
    let (Some(previous), Some(next)) = (previous, next) else {
        return (
            "reload",
            "compiler-authenticated snapshot compatibility is unavailable".into(),
        );
    };
    let migrates_model = previous.model != next.model
        && next.migrations.iter().any(|schema| schema == &previous.model);
    let mismatch = if previous.application != next.application {
        Some("application identity changed")
    } else if previous.model != next.model && !migrates_model {
        Some("model schema changed")
    } else if previous.authorization != next.authorization {
        Some("authorization shape changed")
    } else if previous.template != next.template {
        Some("template or sink schema changed")
    } else if previous.snapshot_format != next.snapshot_format {
        Some("snapshot format changed")
    } else if previous.max_snapshot_bytes != next.max_snapshot_bytes {
        Some("snapshot limit changed")
    } else {
        None
    };
    match mismatch {
        Some(reason) => ("reload", reason.into()),
        None if migrates_model => (
            "swap",
            "compiler-authenticated model migration accepts the previous schema".into(),
        ),
        None => ("swap", "all authenticated compatibility identities match".into()),
    }
}

fn client_index(
    active: &Path,
    token: &str,
    generation: u64,
    build_id: &str,
) -> Result<Vec<u8>, WebCommandError> {
    let path = active.join("index.html");
    let source = std::fs::read_to_string(&path).map_err(|error| {
        WebCommandError::failure(format!("cannot read `{}`: {error}", path.display()))
    })?;
    let tag = format!(
        "<script type=\"module\" src=\"{CLIENT_PATH}?token={token}&amp;generation={generation}&amp;build={build_id}&amp;sourceMap={SOURCES_PREFIX}{build_id}.json\"></script>"
    );
    let index = source.rfind("</body>").ok_or_else(|| {
        WebCommandError::failure("generated development index has no closing `</body>`")
    })?;
    let mut output = String::with_capacity(source.len() + tag.len() + 1);
    output.push_str(&source[..index]);
    output.push_str(&tag);
    output.push('\n');
    output.push_str(&source[index..]);
    Ok(output.into_bytes())
}

fn respond(
    request: Request,
    token: &str,
    published: &Published,
) -> Result<(), WebCommandError> {
    if request.method() != &Method::Get && request.method() != &Method::Head {
        return send(
            request,
            Response::from_string("method not allowed\n")
                .with_status_code(StatusCode(405))
                .with_header(header("Allow", "GET, HEAD")?),
        );
    }
    let url = request.url().to_string();
    let (path, query) = split_url(&url);
    if path.starts_with("/__witchy/") && query.get("token").map(String::as_str) != Some(token) {
        return send(
            request,
            Response::from_string("not found\n").with_status_code(StatusCode(404)),
        );
    }
    match path {
        CLIENT_PATH => send_bytes(request, CLIENT_SOURCE.as_bytes(), "text/javascript; charset=utf-8"),
        EVENTS_PATH => {
            let body = event_document(published)?;
            let response = Response::from_string(body)
                .with_header(header("Content-Type", "text/event-stream; charset=utf-8")?)
                .with_header(header("Cache-Control", "no-store")?)
                .with_header(header("X-Content-Type-Options", "nosniff")?);
            send(request, response)
        }
        value if value.starts_with(DIAGNOSTICS_PREFIX) => {
            let requested = value[DIAGNOSTICS_PREFIX.len()..].parse::<u64>().ok();
            let diagnostic = published
                .diagnostic
                .as_ref()
                .filter(|diagnostic| Some(diagnostic.generation) == requested);
            match diagnostic {
                Some(diagnostic) => {
                    let body = serde_json::to_vec(diagnostic)
                        .map_err(|error| WebCommandError::failure(error.to_string()))?;
                    send_bytes(request, &body, "application/json; charset=utf-8")
                }
                None => send(
                    request,
                    Response::from_string("not found\n").with_status_code(StatusCode(404)),
                ),
            }
        }
        value if value.starts_with(SOURCES_PREFIX) => {
            let requested = &value[SOURCES_PREFIX.len()..];
            if requested == format!("{}.json", published.build_id) {
                send_bytes(
                    request,
                    &published.source_map,
                    "application/json; charset=utf-8",
                )
            } else {
                send(
                    request,
                    Response::from_string("not found\n").with_status_code(StatusCode(404)),
                )
            }
        }
        "/" => send_bytes(request, &published.index, "text/html; charset=utf-8"),
        _ => serve_artifact(request, path, &published.artifact_root),
    }
}

fn event_document(published: &Published) -> Result<String, WebCommandError> {
    let (event, payload) = match &published.diagnostic {
        Some(diagnostic) if diagnostic.generation > published.generation => (
            "diagnostic",
            json!({
                "generation": diagnostic.generation,
                "diagnostic": format!("{DIAGNOSTICS_PREFIX}{}", diagnostic.generation),
                "decision": "keep",
            }),
        ),
        _ => (
            "build",
            json!({
                "generation": published.generation,
                "buildId": published.build_id,
                "applicationIdentity": published.swap.as_ref().map(|value| value.application.as_str()),
                "modelSchema": published.swap.as_ref().map(|value| value.model.as_str()),
                "authorizationSchema": published.swap.as_ref().map(|value| value.authorization.as_str()),
                "templateSchema": published.swap.as_ref().map(|value| value.template.as_str()),
                "snapshotFormat": published.swap.as_ref().map(|value| value.snapshot_format),
                "maxSnapshotBytes": published.swap.as_ref().map(|value| value.max_snapshot_bytes),
                "migrationSchemas": published.swap.as_ref().map(|value| value.migrations.as_slice()),
                "assets": published.assets,
                "wasm": published.wasm_path,
                "manifest": "/witchy-web-manifest.json",
                "sourceMap": format!("{SOURCES_PREFIX}{}.json", published.build_id),
                "decision": published.decision,
                "reason": published.reason,
                "work": {
                    "recompiled": published.work.recompiled,
                    "invalidated": published.work.invalidated,
                    "elapsedMs": published.work.elapsed_ms,
                },
            }),
        ),
    };
    let payload = serde_json::to_string(&payload)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    Ok(format!("retry: 250\nevent: {event}\ndata: {payload}\n\n"))
}

fn serve_artifact(
    request: Request,
    url_path: &str,
    artifact_root: &Path,
) -> Result<(), WebCommandError> {
    let relative = if url_path == "/" { "index.html" } else { &url_path[1..] };
    let Some(relative) = safe_url_path(relative) else {
        return send(
            request,
            Response::from_string("not found\n").with_status_code(StatusCode(404)),
        );
    };
    let artifact_root = std::fs::canonicalize(artifact_root).map_err(|error| {
        WebCommandError::failure(format!("cannot resolve development artifact root: {error}"))
    })?;
    let path = artifact_root.join(&relative);
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) if path.starts_with(&artifact_root) && path.is_file() => path,
        _ => {
            return send(
                request,
                Response::from_string("not found\n").with_status_code(StatusCode(404)),
            )
        }
    };
    let bytes = std::fs::read(&canonical).map_err(|error| {
        WebCommandError::failure(format!("cannot read development artifact: {error}"))
    })?;
    send_bytes(request, &bytes, mime_for(&canonical))
}

fn send_bytes(
    request: Request,
    bytes: &[u8],
    content_type: &str,
) -> Result<(), WebCommandError> {
    let head = request.method() == &Method::Head;
    let length = bytes.len();
    let response = if head {
        Response::from_data(Vec::new())
    } else {
        Response::from_data(bytes.to_vec())
    }
    .with_header(header("Content-Type", content_type)?)
    .with_header(header("Content-Length", &length.to_string())?)
    .with_header(header("Cache-Control", "no-store")?)
    .with_header(header("X-Content-Type-Options", "nosniff")?);
    send(request, response)
}

fn send<R: Read + Send + 'static>(
    request: Request,
    response: Response<R>,
) -> Result<(), WebCommandError> {
    request
        .respond(response)
        .map_err(|error| WebCommandError::failure(format!("development response failed: {error}")))
}

fn header(name: &str, value: &str) -> Result<Header, WebCommandError> {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .map_err(|_| WebCommandError::failure(format!("invalid HTTP header `{name}`")))
}

fn split_url(url: &str) -> (&str, BTreeMap<String, String>) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let values = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    (path, values)
}

fn safe_url_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() || value.contains('\\') || value.contains('%') {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("mjs") | Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn input_fingerprint(
    project: &super::Project,
    compiler_sources: Option<&std::collections::BTreeSet<PathBuf>>,
) -> Result<InputFingerprint, WebCommandError> {
    fn walk(
        root: &Path,
        directory: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), WebCommandError> {
        for entry in std::fs::read_dir(directory).map_err(|error| {
            WebCommandError::failure(format!("cannot watch `{}`: {error}", directory.display()))
        })? {
            let entry = entry.map_err(|error| WebCommandError::failure(error.to_string()))?;
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("walk stays below root");
            let first = relative.components().next();
            if matches!(first, Some(Component::Normal(name))
                if name == ".witchy"
                    || name == ".git"
                    || name == "dist"
                    || name.to_string_lossy().starts_with("target"))
            {
                continue;
            }
            let ty = entry.file_type().map_err(|error| WebCommandError::failure(error.to_string()))?;
            if ty.is_symlink() {
                files.push(path);
                continue;
            }
            if ty.is_dir() {
                walk(root, &path, files)?;
            } else if ty.is_file() {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(&project.root, &project.root, &mut files)?;
    if let Some(content) = &project.content {
        if !content.starts_with(&project.root) {
            walk(content, content, &mut files)?;
        }
    }
    files.sort();
    files.dedup();
    let mut compiler = Sha256::new();
    let mut template = Sha256::new();
    let mut style = Sha256::new();
    let mut assets = Sha256::new();
    for (hash, label) in [
        (&mut compiler, b"compiler".as_slice()),
        (&mut template, b"template".as_slice()),
        (&mut style, b"style".as_slice()),
        (&mut assets, b"assets".as_slice()),
    ] {
        hash.update(label);
        hash.update([0]);
    }
    for path in files {
        let content_relative = project
            .content
            .as_ref()
            .and_then(|content| path.strip_prefix(content).ok());
        let relative = if let Some(relative) = content_relative {
            PathBuf::from("<content>").join(relative)
        } else {
            path.strip_prefix(&project.root)
                .expect("walk stays below project or content root")
                .to_path_buf()
        };
        let hash = if content_relative.is_some() || path == project.manifest {
            Some(&mut template)
        } else if project.css.as_ref() == Some(&path) {
            Some(&mut style)
        } else if path == project.index || path.starts_with(&project.public) {
            Some(&mut assets)
        } else if (compiler_sources.is_none_or(|sources| sources.contains(&path))
            && path.extension().and_then(|value| value.to_str()) == Some("witchy"))
            || matches!(relative.to_str(), Some("witchy.toml" | "witchy.lock"))
        {
            Some(&mut compiler)
        } else {
            None
        };
        let Some(hash) = hash else { continue };
        if path.is_symlink() {
            return Err(WebCommandError::failure(format!(
                "development input `{}` is a symlink",
                relative.display()
            )));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            WebCommandError::failure(format!("cannot watch `{}`: {error}", relative.display()))
        })?;
        hash.update(relative.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(InputFingerprint {
        compiler: hex(&compiler.finalize()),
        template: hex(&template.finalize()),
        style: hex(&style.finalize()),
        assets: hex(&assets.finalize()),
    })
}

fn bounded_diagnostic(generation: u64, project_root: &Path, message: String) -> Diagnostic {
    let mut message = message.replace(&project_root.to_string_lossy().to_string(), ".");
    if message.len() > MAX_DIAGNOSTIC_BYTES {
        let mut end = MAX_DIAGNOSTIC_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    Diagnostic {
        generation,
        phase: "build",
        severity: "error",
        code: "WITCHY_WEB_BUILD",
        message,
        source: None,
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

const CLIENT_SOURCE: &str = r#"const ownUrl = new URL(import.meta.url);
const token = ownUrl.searchParams.get("token");
let generation = Number(ownUrl.searchParams.get("generation") || "0");
const initialBuildId = ownUrl.searchParams.get("build");
const initialSourceMap = ownUrl.searchParams.get("sourceMap");
let pendingGeneration = generation;
let lastSwap = null;
let lastBuild = null;
let lastSourceMap = null;

function freezeTree(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) freezeTree(child);
  return Object.freeze(value);
}

async function loadSourceMap(path, expectedBuildId) {
  if (
    typeof path !== "string" ||
    typeof expectedBuildId !== "string" ||
    !/^[0-9a-f]{64}$/.test(expectedBuildId) ||
    path !== `/__witchy/sources/${expectedBuildId}.json`
  ) return null;
  try {
    const response = await fetch(`${path}?token=${encodeURIComponent(token)}`, {
      credentials: "same-origin",
      cache: "no-store",
    });
    if (!response.ok) return null;
    const sourceMap = await response.json();
    if (
      sourceMap?.schema !== "witchy.web.source-map.v1" ||
      sourceMap.buildIdentity !== expectedBuildId ||
      sourceMap.sourcesEmbedded !== false
    ) return null;
    return freezeTree(sourceMap);
  } catch {
    return null;
  }
}

void loadSourceMap(initialSourceMap, initialBuildId).then((sourceMap) => {
  if (sourceMap) lastSourceMap = sourceMap;
});

function overlay(message) {
  let host = document.querySelector("witchy-dev-overlay");
  if (!host) {
    host = document.createElement("witchy-dev-overlay");
    host.style.position = "fixed";
    host.style.inset = "auto 1rem 1rem 1rem";
    host.style.zIndex = "2147483647";
    document.documentElement.appendChild(host);
  }
  const root = host.shadowRoot || host.attachShadow({ mode: "closed" });
  root.textContent = "";
  const panel = document.createElement("pre");
  panel.style.cssText = "margin:0;padding:1rem;max-height:40vh;overflow:auto;background:#170f1f;color:#f8eafa;border:1px solid #9c72b0;border-radius:.5rem;white-space:pre-wrap;font:13px/1.5 ui-monospace,monospace";
  panel.textContent = message;
  root.appendChild(panel);
}

const events = new EventSource(`/__witchy/events?token=${encodeURIComponent(token)}`);
events.addEventListener("build", async (event) => {
  const next = JSON.parse(event.data);
  if (next.generation <= Math.max(generation, pendingGeneration)) return;
  pendingGeneration = next.generation;
  const candidateSourceMap = loadSourceMap(next.sourceMap, next.buildId);
  lastBuild = Object.freeze({
    generation: next.generation,
    buildId: next.buildId,
    decision: next.decision,
    reason: next.reason,
    work: next.work,
  });
  if (next.decision === "swap") {
    const bridge = globalThis.__WITCHY_GLAMOUR_DEV__;
    if (bridge && typeof bridge.swap === "function") {
      try {
        lastSwap = await bridge.swap(next);
        generation = next.generation;
        pendingGeneration = Math.max(pendingGeneration, generation);
        void candidateSourceMap.then((sourceMap) => {
          if (sourceMap && generation === next.generation) lastSourceMap = sourceMap;
        });
        return;
      } catch (error) {
        generation = next.generation;
        pendingGeneration = generation;
        overlay(`swap: ${error instanceof Error ? error.message : "candidate failed"}`);
        return;
      }
    }
  }
  generation = next.generation;
  pendingGeneration = generation;
  location.reload();
});
events.addEventListener("diagnostic", async (event) => {
  const next = JSON.parse(event.data);
  if (next.generation <= generation) return;
  const response = await fetch(`${next.diagnostic}?token=${encodeURIComponent(token)}`, {
    credentials: "same-origin",
    cache: "no-store",
  });
  if (!response.ok) return;
  const diagnostic = await response.json();
  overlay(`${diagnostic.phase}: ${diagnostic.message}`);
});

Object.defineProperty(globalThis, "__WITCHY_DEVTOOLS__", {
  value: Object.freeze({
    get generation() { return generation; },
    lifecycle: "running",
    canDispatch: false,
    canReadSecrets: false,
    get lastSwap() { return lastSwap; },
    get lastBuild() { return lastBuild; },
    get sourceMap() { return lastSourceMap; },
    get runtime() {
      const bridge = globalThis.__WITCHY_GLAMOUR_DEV__;
      return bridge && typeof bridge.inspect === "function" ? bridge.inspect() : null;
    },
  }),
  configurable: false,
  enumerable: false,
  writable: false,
});
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn test_work() -> BuildWork {
        BuildWork {
            recompiled: false,
            invalidated: Vec::new(),
            elapsed_ms: 0,
        }
    }

    fn test_checked() -> super::super::CheckedWeb {
        super::super::CheckedWeb {
            project: super::super::Project {
                root: PathBuf::from("."),
                name: "test".into(),
                version: "0.0.0".into(),
                delivery: super::super::Delivery::Client,
                entry: PathBuf::from("src/main.witchy"),
                index: PathBuf::from("web/index.html"),
                public: PathBuf::from("web/public"),
                manifest: PathBuf::from("web/glamour-manifest.json"),
                grants: Some(PathBuf::from("web/grants.toml")),
                css: None,
                content: None,
                hosting: super::super::HostingProfile::Portable,
            },
            grant: super::super::WebUiGrant {
                schema: "witchy.web.ui-root-grant.v1",
                parameter: "ui".into(),
                capability: "UiRoot",
                policy: "test".into(),
                digest: "0".repeat(64),
            },
            wasm: Vec::new(),
            exports: Default::default(),
            host_imports: Default::default(),
            protocol_manifest: json!({}),
            source_origins: json!([]),
            tagged_origins: json!([]),
            compiler_sources: Default::default(),
            source_functions: json!([]),
            wasm_functions: json!([]),
            templates: Vec::new(),
            development: None,
            packages: Vec::new(),
        }
    }

    #[test]
    fn options_default_to_loopback_and_reject_ambiguous_inputs() {
        assert_eq!(
            parse_options(&[]).expect("defaults"),
            Options {
                host: "127.0.0.1".into(),
                port: 3000,
                root: PathBuf::from("."),
            }
        );
        assert!(parse_options(&["--port".into(), "0".into()]).is_err());
        assert!(parse_options(&["--wat".into()]).is_err());
        assert!(parse_options(&["one".into(), "two".into()]).is_err());
    }

    #[test]
    fn request_paths_are_normalized_without_percent_or_parent_decoding() {
        assert_eq!(safe_url_path("assets/app.wasm"), Some(PathBuf::from("assets/app.wasm")));
        assert_eq!(safe_url_path("index.html"), Some(PathBuf::from("index.html")));
        assert_eq!(safe_url_path("../secret"), None);
        assert_eq!(safe_url_path("assets%2fsecret"), None);
        assert_eq!(safe_url_path("assets\\secret"), None);
    }

    #[test]
    fn generation_events_explain_conservative_reload() {
        let published = Published {
            generation: 7,
            build_id: "build-seven".into(),
            assets: vec!["app.wasm".into()],
            artifact_root: PathBuf::from("."),
            index: Vec::new(),
            source_map: br#"{"buildIdentity":"build-seven"}"#.to_vec(),
            wasm_path: "/assets/app.wasm".into(),
            swap: None,
            decision: "reload",
            reason: "compatibility unavailable".into(),
            checked: test_checked(),
            work: test_work(),
            diagnostic: None,
        };
        let event = event_document(&published).expect("event");
        assert!(event.contains("\"decision\":\"reload\""));
        assert!(event.contains("\"modelSchema\":null"));
        assert!(event.contains("/__witchy/sources/build-seven.json"));
        assert!(!event.contains("\"decision\":\"swap\""));
    }

    #[test]
    fn swap_requires_every_authenticated_identity_to_match() {
        let contract = SwapContract {
            application: "app".into(),
            model: "model".into(),
            authorization: "authority".into(),
            template: "template".into(),
            snapshot_format: 1,
            max_snapshot_bytes: 1024,
            migrations: Vec::new(),
        };
        assert_eq!(
            swap_decision(Some(&contract), Some(&contract)),
            (
                "swap",
                "all authenticated compatibility identities match".into()
            )
        );
        let mut changed = contract.clone();
        changed.model = "other-model".into();
        assert_eq!(
            swap_decision(Some(&contract), Some(&changed)),
            ("reload", "model schema changed".into())
        );
        changed.migrations.push(contract.model.clone());
        assert_eq!(
            swap_decision(Some(&contract), Some(&changed)),
            (
                "swap",
                "compiler-authenticated model migration accepts the previous schema".into(),
            )
        );
        changed = contract.clone();
        changed.authorization = "other-authority".into();
        assert_eq!(
            swap_decision(Some(&contract), Some(&changed)),
            ("reload", "authorization shape changed".into())
        );
        assert_eq!(
            swap_decision(Some(&contract), None).0,
            "reload",
            "missing compiler ABI cannot swap"
        );
    }

    #[test]
    fn aggregate_development_metadata_requires_reload_without_snapshot_authority() {
        let manifest = json!({
            "development": {
                "snapshotFormat": 0,
                "modelSchema": "model",
                "authorizationSchema": "authority",
            },
        });
        assert_eq!(swap_contract(&manifest).expect("aggregate contract"), None);
        assert_eq!(
            swap_decision(None, swap_contract(&manifest).expect("aggregate contract").as_ref()).0,
            "reload",
        );
    }

    #[test]
    fn diagnostics_are_utf8_safe_and_bounded() {
        let root = Path::new("/private/witchy-project");
        let diagnostic = bounded_diagnostic(
            4,
            root,
            format!("{}/src/main.witchy: {}", root.display(), "🧙".repeat(MAX_DIAGNOSTIC_BYTES)),
        );
        assert!(diagnostic.message.is_char_boundary(diagnostic.message.len()));
        assert!(diagnostic.message.len() <= MAX_DIAGNOSTIC_BYTES + "…".len());
        assert!(diagnostic.message.ends_with('…'));
        assert!(!diagnostic.message.contains("/private/witchy-project"));
        assert!(diagnostic.message.starts_with("./src/main.witchy"));
    }

    #[test]
    fn overlay_uses_only_text_content_and_read_only_tools() {
        assert!(CLIENT_SOURCE.contains("panel.textContent = message"));
        assert!(!CLIENT_SOURCE.contains("innerHTML"));
        assert!(CLIENT_SOURCE.contains("canDispatch: false"));
        assert!(CLIENT_SOURCE.contains("canReadSecrets: false"));
        assert!(CLIENT_SOURCE.contains("candidateSourceMap = loadSourceMap"));
        assert!(CLIENT_SOURCE.contains("typeof bridge.inspect === \"function\""));
        assert!(!CLIENT_SOURCE.contains("bridge.application"));
        assert!(CLIENT_SOURCE.contains("attachShadow({ mode: \"closed\" })"));
    }

    #[test]
    fn malformed_and_unauthenticated_requests_are_local_failures() {
        let server = Server::http("127.0.0.1:0").expect("bind loopback");
        let address = server.server_addr().to_string();
        let published = Published {
            generation: 1,
            build_id: "build-one".into(),
            assets: vec![],
            artifact_root: PathBuf::from("."),
            index: b"last good".to_vec(),
            source_map: br#"{"buildIdentity":"build-one"}"#.to_vec(),
            wasm_path: "/assets/app.wasm".into(),
            swap: None,
            decision: "reload",
            reason: "compatibility unavailable".into(),
            checked: test_checked(),
            work: test_work(),
            diagnostic: None,
        };
        let worker = std::thread::spawn(move || {
            for _ in 0..5 {
                let request = server.recv().expect("request");
                respond(request, "secret", &published).expect("request-local response");
            }
        });

        assert!(http_get(&address, "/__witchy/events").starts_with("HTTP/1.1 404"));
        assert!(
            http_get(&address, "/__witchy/sources/other.json?token=secret")
                .starts_with("HTTP/1.1 404")
        );
        let source_map =
            http_get(&address, "/__witchy/sources/build-one.json?token=secret");
        assert!(source_map.starts_with("HTTP/1.1 200"));
        assert!(source_map.ends_with(r#"{"buildIdentity":"build-one"}"#));
        assert!(http_get(&address, "/%2e%2e/Cargo.toml").starts_with("HTTP/1.1 404"));
        let root = http_get(&address, "/");
        assert!(root.starts_with("HTTP/1.1 200"));
        assert!(root.ends_with("last good"));
        worker.join().expect("server worker");
    }

    #[test]
    fn failed_rebuild_keeps_last_good_artifacts() {
        let root = temp_path("last-good");
        super::super::create_project_atomically(
            &root,
            &super::super::client_fixture_files("last-good-web", "last_good_web"),
        )
        .expect("create project");
        let cache = root.join(".witchy/web");
        std::fs::create_dir_all(&cache).expect("cache");
        let active = cache.join("active");
        let checked = check_project_development(&root).expect("initial check");
        let published = publish(
            &checked,
            &active,
            "test-token",
            1,
            None,
            None,
            test_work(),
        )
            .expect("initial publish");
        let source_map: serde_json::Value =
            serde_json::from_slice(&published.source_map).expect("source map JSON");
        assert_eq!(source_map["buildIdentity"], published.build_id);
        assert_eq!(source_map["sourcesEmbedded"], false);
        assert_eq!(source_map["expressionSpanEncoding"]["columns"], "unicode-scalar");
        assert_eq!(source_map["expressionSpanEncoding"]["end"], "exclusive");
        assert!(source_map.get("generatedOrigins").is_some());
        assert!(source_map.get("taggedLiterals").is_some());
        assert!(source_map["sourceFunctions"]
            .as_array()
            .is_some_and(|functions| !functions.is_empty()));
        assert!(source_map["wasmFunctions"]
            .as_array()
            .is_some_and(|functions| !functions.is_empty()));
        assert!(source_map["wasmFunctions"]
            .as_array()
            .is_some_and(|functions| functions.iter().any(|function| function["source"].is_object())));
        assert!(source_map["wasmFunctions"].as_array().is_some_and(|functions| {
            functions.iter().any(|function| {
                function["source"].is_object()
                    && function["statementMappings"]
                        .as_array()
                        .is_some_and(|mappings| !mappings.is_empty())
            })
        }));
        assert!(source_map["wasmFunctions"].as_array().is_some_and(|functions| {
            functions.iter().all(|function| {
                let start = function["bodyOffset"].as_u64().expect("body start");
                let end = function["bodyEnd"].as_u64().expect("body end");
                function["instructionOffsets"]
                    .as_array()
                    .is_some_and(|offsets| {
                        !offsets.is_empty()
                            && offsets
                                .iter()
                                .all(|offset| offset.as_u64().is_some_and(|offset| {
                                    offset >= start && offset < end
                                }))
                            && offsets.windows(2).all(|pair| {
                                pair[0].as_u64().expect("left instruction offset")
                                    < pair[1].as_u64().expect("right instruction offset")
                            })
                    })
            })
        }));
        assert!(source_map.get("templates").is_some());
        assert!(source_map["operationMappings"].as_array().is_some());
        assert!(!String::from_utf8(published.source_map.clone())
            .expect("UTF-8 source map")
            .contains(&root.to_string_lossy().to_string()));
        let published_index = String::from_utf8(published.index.clone()).expect("UTF-8 index");
        assert!(published_index.contains(&format!("build={}", published.build_id)));
        assert!(published_index.contains(&format!(
            "sourceMap={SOURCES_PREFIX}{}.json",
            published.build_id
        )));
        assert!(published.swap.is_some());
        let development_runtime = std::fs::read_to_string(
            std::fs::read_dir(active.join("assets"))
                .expect("assets")
                .map(Result::unwrap)
                .find(|entry| {
                    entry.file_name().to_string_lossy().starts_with("glamour-runtime-")
                })
                .expect("development runtime")
                .path(),
        )
        .expect("read development runtime");
        assert!(development_runtime.contains("__WITCHY_GLAMOUR_DEV__"));
        assert!(development_runtime.contains("installDevelopmentSwap"));
        let prior_index = published.index.clone();
        let prior_wasm = std::fs::read(
            std::fs::read_dir(active.join("assets"))
                .expect("assets")
                .map(Result::unwrap)
                .find(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("wasm"))
                .expect("wasm")
                .path(),
        )
        .expect("read wasm");

        let source = root.join("src/last_good_web.witchy");
        std::fs::write(&source, "this is not Witchy\n").expect("break source");
        let diagnostic = rebuild(
            &root,
            &active,
            "test-token",
            2,
            &published,
            Invalidation {
                compiler: true,
                ..Invalidation::default()
            },
        )
            .expect_err("broken rebuild");
        assert_eq!(diagnostic.generation, 2);
        assert_eq!(published.generation, 1);
        assert_eq!(published.index, prior_index);
        let live_wasm = std::fs::read(
            std::fs::read_dir(active.join("assets"))
                .expect("assets")
                .map(Result::unwrap)
                .find(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("wasm"))
                .expect("wasm")
                .path(),
        )
        .expect("read wasm");
        assert_eq!(live_wasm, prior_wasm);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rebuild_swaps_only_for_matching_compiler_contracts() {
        let root = temp_path("swap-contract");
        super::super::create_project_atomically(
            &root,
            &super::super::client_fixture_files("swap-web", "swap_web"),
        )
        .expect("create project");
        let active = root.join(".witchy/web/active");
        std::fs::create_dir_all(active.parent().expect("active parent")).expect("cache");
        let checked = check_project_development(&root).expect("initial check");
        let first = publish(
            &checked,
            &active,
            "test-token",
            1,
            None,
            None,
            test_work(),
        )
            .expect("initial publish");
        assert_eq!(first.decision, "reload");

        let source = root.join("src/swap_web.witchy");
        let original = std::fs::read_to_string(&source).expect("source");
        std::fs::write(&source, format!("{original}\n")).expect("compatible edit");
        let second = rebuild(
            &root,
            &active,
            "test-token",
            2,
            &first,
            Invalidation {
                compiler: true,
                ..Invalidation::default()
            },
        )
        .expect("compatible rebuild");
        assert!(second.work.recompiled);
        assert_eq!(second.work.invalidated, ["compiler"]);
        assert_eq!(second.decision, "swap");
        assert_eq!(
            second.reason,
            "all authenticated compatibility identities match"
        );

        let incompatible = original
            .replace("BrowserState(Bool)", "BrowserState(Int)")
            .replace("BrowserState(true)", "BrowserState(1)");
        std::fs::write(&source, incompatible).expect("schema edit");
        let third = rebuild(
            &root,
            &active,
            "test-token",
            3,
            &second,
            Invalidation {
                compiler: true,
                ..Invalidation::default()
            },
        )
            .expect("incompatible rebuild");
        assert_eq!(third.decision, "reload");
        assert_eq!(third.reason, "model schema changed");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn template_and_asset_edits_bypass_witchy_recompilation() {
        let root = temp_path("incremental-assets");
        super::super::create_project_atomically(
            &root,
            &super::super::client_fixture_files("incremental-web", "incremental_web"),
        )
        .expect("create project");
        let active = root.join(".witchy/web/active");
        std::fs::create_dir_all(active.parent().expect("active parent")).expect("cache");
        let checked = check_project_development(&root).expect("initial check");
        let mut fingerprint = input_fingerprint(
            &checked.project,
            Some(&checked.compiler_sources),
        )
        .expect("initial fingerprint");
        let first = publish(
            &checked,
            &active,
            "test-token",
            1,
            None,
            None,
            test_work(),
        )
        .expect("initial publish");

        let index = root.join("web/index.html");
        let index_source = std::fs::read_to_string(&index).expect("index");
        std::fs::write(
            &index,
            index_source.replace("<title>Witchy app</title>", "<title>Edited</title>"),
        )
        .expect("edit index");
        let next = input_fingerprint(
            &first.checked.project,
            Some(&first.checked.compiler_sources),
        )
        .expect("asset fingerprint");
        let invalidation = Invalidation::between(&fingerprint, &next);
        assert_eq!(
            invalidation,
            Invalidation {
                assets: true,
                ..Invalidation::default()
            }
        );
        fingerprint = next;
        let second = rebuild(
            &root,
            &active,
            "test-token",
            2,
            &first,
            invalidation,
        )
        .expect("asset-only rebuild");
        assert!(!second.work.recompiled);
        assert_eq!(second.work.invalidated, ["assets"]);
        assert_eq!(second.checked.wasm, first.checked.wasm);
        assert_ne!(second.build_id, first.build_id);
        assert_eq!(second.decision, "reload");
        assert_eq!(second.reason, "document or public assets changed");

        let manifest = root.join("web/glamour-manifest.json");
        let manifest_source = std::fs::read_to_string(&manifest).expect("manifest");
        std::fs::write(
            &manifest,
            manifest_source.replace("Hello from Witchy", "Changed template"),
        )
        .expect("edit template");
        let next = input_fingerprint(
            &second.checked.project,
            Some(&second.checked.compiler_sources),
        )
        .expect("template fingerprint");
        let invalidation = Invalidation::between(&fingerprint, &next);
        assert_eq!(
            invalidation,
            Invalidation {
                template: true,
                ..Invalidation::default()
            }
        );
        let third = rebuild(
            &root,
            &active,
            "test-token",
            3,
            &second,
            invalidation,
        )
        .expect("template-only rebuild");
        assert!(!third.work.recompiled);
        assert_eq!(third.work.invalidated, ["template"]);
        assert_eq!(third.checked.wasm, second.checked.wasm);
        assert_eq!(third.decision, "reload");
        assert_eq!(third.reason, "template or sink schema changed");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compiler_invalidation_follows_the_loaded_local_source_graph() {
        let root = temp_path("incremental-source-graph");
        let mut files = super::super::client_fixture_files("source-graph", "source_graph");
        files
            .get_mut(Path::new("src/source_graph.witchy"))
            .expect("starter source")
            .insert_str(0, "import helper\n");
        files.insert(
            PathBuf::from("src/helper.witchy"),
            "fn loaded_value() -> Int:\n    1\n".into(),
        );
        files.insert(
            PathBuf::from("src/unloaded.witchy"),
            "fn unrelated_value() -> Int:\n    1\n".into(),
        );
        super::super::create_project_atomically(&root, &files).expect("create project");
        let checked = check_project_development(&root).expect("checked source graph");
        assert!(checked
            .compiler_sources
            .iter()
            .any(|path| path.ends_with("src/helper.witchy")));
        assert!(!checked
            .compiler_sources
            .iter()
            .any(|path| path.ends_with("src/unloaded.witchy")));
        let first = input_fingerprint(&checked.project, Some(&checked.compiler_sources))
            .expect("initial fingerprint");

        std::fs::write(
            root.join("src/unloaded.witchy"),
            "fn unrelated_value() -> Int:\n    2\n",
        )
        .expect("edit unimported module");
        let unrelated = input_fingerprint(&checked.project, Some(&checked.compiler_sources))
            .expect("unrelated fingerprint");
        assert_eq!(unrelated, first, "unimported source must not invalidate the compiler");

        std::fs::write(
            root.join("src/helper.witchy"),
            "fn loaded_value() -> Int:\n    2\n",
        )
        .expect("edit imported module");
        let imported = input_fingerprint(&checked.project, Some(&checked.compiler_sources))
            .expect("imported fingerprint");
        assert_eq!(
            Invalidation::between(&unrelated, &imported),
            Invalidation {
                compiler: true,
                ..Invalidation::default()
            }
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn declared_content_edits_invalidate_static_templates() {
        let base = temp_path("static-content-watch");
        let root = base.join("site");
        let content = base.join("content");
        std::fs::create_dir_all(&content).expect("content root");
        std::fs::write(content.join("page.md"), "first").expect("initial content");
        let mut files = super::super::static_fixture_files("content-watch", "content_watch");
        files
            .get_mut(Path::new("witchy.toml"))
            .expect("manifest")
            .push_str("content = \"../content\"\n");
        files.insert(
            PathBuf::from("src/content_watch.witchy"),
            r#"from glamour import Site, StaticContent

type Message:
    Unused

pub fn web(content: StaticContent) -> Site:
    let text = glamour.static_content_get(content, "page.md") ?? "missing"
    glamour.site([glamour.static_page("/", glamour.ui(glamour.text(text)))])
"#
            .into(),
        );
        super::super::create_project_atomically(&root, &files).expect("create project");
        let project = super::super::load_project(&root).expect("load");
        let first = input_fingerprint(&project, None).expect("first fingerprint");

        std::fs::write(content.join("page.md"), "second").expect("edit content");
        let second = input_fingerprint(&project, None).expect("second fingerprint");
        assert_eq!(
            Invalidation::between(&first, &second),
            Invalidation {
                template: true,
                ..Invalidation::default()
            }
        );
        std::fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn static_development_injects_only_the_dev_client_and_keeps_last_good_output() {
        let root = temp_path("static-last-good");
        let mut files = super::super::static_fixture_files("static-dev", "static_dev");
        files.insert(
            PathBuf::from("src/static_dev.witchy"),
            r#"from glamour import Site

type Message:
    Unused

fn page(text: String) -> glamour.Ui(Message):
    glamour.ui(glamour.element("main", [], [glamour.text(text)]))

pub fn web() -> Site:
    glamour.site([
        glamour.static_page("/", page("Home")),
        glamour.static_page("/about", page("About")),
    ])
"#
            .into(),
        );
        super::super::create_project_atomically(&root, &files).expect("create project");
        let project = super::super::load_project(&root).expect("load");
        let checked = super::super::check_static_project(project).expect("check");
        let active = root.join(".witchy/web/active");
        let published = static_site::publish_static(&checked, &active, 1, None)
            .expect("publish static development build");
        let production_index =
            std::fs::read(active.join("index.html")).expect("production static index");
        assert!(!production_index
            .windows(CLIENT_PATH.len())
            .any(|window| window == CLIENT_PATH.as_bytes()));
        let development_index =
            static_site::inject_development_client(&production_index, "test-token", 1)
                .expect("inject client");
        let development_index = String::from_utf8(development_index).expect("UTF-8");
        assert!(development_index.contains(CLIENT_PATH));
        assert!(development_index.contains("token=test-token"));
        assert!(static_site::static_event_document(&published)
            .expect("event")
            .contains("\"decision\":\"reload\""));

        let server = Server::http("127.0.0.1:0").expect("bind loopback");
        let address = server.server_addr().to_string();
        let served = published.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..4 {
                let request = server.recv().expect("request");
                static_site::respond_static(request, "test-token", &served)
                    .expect("static response");
            }
        });
        let about = http_get(&address, "/about");
        assert!(about.starts_with("HTTP/1.1 200"), "{about}");
        assert!(about.contains("About"));
        assert!(about.contains(CLIENT_PATH));
        assert!(http_get(&address, "/__witchy/events").starts_with("HTTP/1.1 404"));
        let events = http_get(&address, "/__witchy/events?token=test-token");
        assert!(events.starts_with("HTTP/1.1 200"));
        assert!(events.contains("\"decision\":\"reload\""));
        assert!(http_get(&address, "/%2e%2e/Cargo.toml").starts_with("HTTP/1.1 404"));
        worker.join().expect("server worker");

        let prior_about = std::fs::read(active.join("about/index.html")).expect("last good route");
        std::fs::write(root.join("src/static_dev.witchy"), "not Witchy\n")
            .expect("break source");
        let error = super::super::load_project(&root)
            .and_then(super::super::check_static_project)
            .expect_err("broken static rebuild");
        let diagnostic = bounded_diagnostic(2, &root, error.to_string());
        assert_eq!(diagnostic.generation, 2);
        assert_eq!(
            std::fs::read(active.join("about/index.html")).expect("retained route"),
            prior_about
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    fn temp_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("witchy-web-dev-{name}-{}-{nonce}", std::process::id()))
    }

    fn http_get(address: &str, path: &str) -> String {
        let mut stream = std::net::TcpStream::connect(address).expect("connect");
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        response
    }
}
