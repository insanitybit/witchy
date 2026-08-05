//! Last-good development serving for zero-runtime static sites.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct StaticPublished {
    generation: u64,
    build_id: String,
    artifact_root: PathBuf,
    project: super::super::Project,
    diagnostic: Option<Diagnostic>,
}
pub(super) fn command(
    options: Options,
    project: super::super::Project,
) -> Result<String, WebCommandError> {
    let checked = super::super::check_static_project(project)?;
    let project_root = checked.project.root.clone();
    let cache = project_root.join(".witchy").join("web");
    std::fs::create_dir_all(&cache).map_err(|error| {
        WebCommandError::failure(format!("cannot create `{}`: {error}", cache.display()))
    })?;
    let active = cache.join("active");
    let token = session_token()?;
    let mut published = publish_static(&checked, &active, 1, None)?;
    let mut fingerprint = input_fingerprint(&checked.project, None)?;
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
    println!(
        "Witchy dev serving static {} at http://{exposed}",
        checked.project.name
    );
    loop {
        if let Some(request) = server.recv_timeout(Duration::from_millis(150)).map_err(|error| {
            WebCommandError::failure(format!("development server receive failed: {error}"))
        })? {
            if let Err(error) = respond_static(request, &token, &published) {
                eprintln!("Witchy dev request failed: {error}");
            }
        }
        match input_fingerprint(&published.project, None) {
            Ok(next) if next != fingerprint => {
                fingerprint = next;
                let generation = published.generation.saturating_add(1);
                let rebuilt = super::super::load_project(&project_root)
                    .and_then(super::super::check_static_project)
                    .and_then(|checked| publish_static(&checked, &active, generation, None));
                match rebuilt {
                    Ok(next) => {
                        published = next;
                        println!("Witchy dev generation {generation}: reload");
                    }
                    Err(error) => {
                        published.diagnostic = Some(bounded_diagnostic(
                            generation,
                            &project_root,
                            error.to_string(),
                        ));
                        eprintln!(
                            "Witchy dev generation {generation}: build failed; keeping last good build"
                        );
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

pub(super) fn publish_static(
    checked: &super::super::CheckedStaticSite,
    active: &Path,
    generation: u64,
    diagnostic: Option<Diagnostic>,
) -> Result<StaticPublished, WebCommandError> {
    super::super::write_static_production(checked, active)?;
    let manifest_path = active.join("witchy-web-manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|error| {
            WebCommandError::failure(format!("cannot read `{}`: {error}", manifest_path.display()))
        })?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let build_id = manifest["buildIdentity"]
        .as_str()
        .ok_or_else(|| WebCommandError::failure("static manifest has no build identity"))?
        .to_string();
    Ok(StaticPublished {
        generation,
        build_id,
        artifact_root: active.to_path_buf(),
        project: checked.project.clone(),
        diagnostic,
    })
}

pub(super) fn respond_static(
    request: Request,
    token: &str,
    published: &StaticPublished,
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
        CLIENT_PATH => send_bytes(
            request,
            CLIENT_SOURCE.as_bytes(),
            "text/javascript; charset=utf-8",
        ),
        EVENTS_PATH => {
            let body = static_event_document(published)?;
            let response = Response::from_string(body)
                .with_header(header("Content-Type", "text/event-stream; charset=utf-8")?)
                .with_header(header("Cache-Control", "no-store")?)
                .with_header(header("X-Content-Type-Options", "nosniff")?);
            send(request, response)
        }
        value if value.starts_with(DIAGNOSTICS_PREFIX) => {
            let requested = value[DIAGNOSTICS_PREFIX.len()..].parse::<u64>().ok();
            match published
                .diagnostic
                .as_ref()
                .filter(|diagnostic| Some(diagnostic.generation) == requested)
            {
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
        value if value.starts_with("/__witchy/") => send(
            request,
            Response::from_string("not found\n").with_status_code(StatusCode(404)),
        ),
        _ => serve_static_artifact(request, path, token, published),
    }
}

pub(super) fn static_event_document(published: &StaticPublished) -> Result<String, WebCommandError> {
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
                "assets": [],
                "wasm": null,
                "manifest": "/witchy-web-manifest.json",
                "sourceMap": null,
                "decision": "reload",
                "reason": "static site rebuilt",
                "work": {
                    "recompiled": true,
                    "invalidated": ["compiler", "style", "assets"],
                    "elapsedMs": 0,
                },
            }),
        ),
    };
    let payload = serde_json::to_string(&payload)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    Ok(format!("retry: 250\nevent: {event}\ndata: {payload}\n\n"))
}

fn serve_static_artifact(
    request: Request,
    url_path: &str,
    token: &str,
    published: &StaticPublished,
) -> Result<(), WebCommandError> {
    let requested = url_path.strip_prefix('/').unwrap_or(url_path);
    let candidate = if requested.is_empty() {
        Some(PathBuf::from("index.html"))
    } else {
        safe_url_path(requested)
    };
    let Some(mut relative) = candidate else {
        return send(
            request,
            Response::from_string("not found\n").with_status_code(StatusCode(404)),
        );
    };
    if relative.extension().is_none() {
        relative.push("index.html");
    }
    let artifact_root = std::fs::canonicalize(&published.artifact_root).map_err(|error| {
        WebCommandError::failure(format!("cannot resolve static artifact root: {error}"))
    })?;
    let path = artifact_root.join(&relative);
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) if path.starts_with(&artifact_root) && path.is_file() => path,
        _ => {
            return send(
                request,
                Response::from_string("not found\n").with_status_code(StatusCode(404)),
            );
        }
    };
    let mut bytes = std::fs::read(&canonical).map_err(|error| {
        WebCommandError::failure(format!("cannot read static development artifact: {error}"))
    })?;
    if canonical.extension().and_then(|value| value.to_str()) == Some("html") {
        bytes = inject_development_client(&bytes, token, published.generation)?;
    }
    send_bytes(request, &bytes, mime_for(&canonical))
}

pub(super) fn inject_development_client(
    source: &[u8],
    token: &str,
    generation: u64,
) -> Result<Vec<u8>, WebCommandError> {
    let source = std::str::from_utf8(source)
        .map_err(|_| WebCommandError::failure("static route HTML is not UTF-8"))?;
    let tag = format!(
        "<script type=\"module\" src=\"{CLIENT_PATH}?token={token}&amp;generation={generation}\"></script>"
    );
    let index = source.rfind("</body>").ok_or_else(|| {
        WebCommandError::failure("generated static route has no closing `</body>`")
    })?;
    let mut output = String::with_capacity(source.len() + tag.len() + 1);
    output.push_str(&source[..index]);
    output.push_str(&tag);
    output.push('\n');
    output.push_str(&source[index..]);
    Ok(output.into_bytes())
}
