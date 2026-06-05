//! The coven registry server — `witchy coven-serve`. A small synchronous HTTP
//! service (tiny_http) backed by a [`LocalRegistry`], speaking the JSON wire
//! protocol in [`super::wire`]. All the security logic (content-addressing,
//! footprint recomputation, two-phase publish, record signing) lives in the
//! registry; the server is just transport + routing.

use std::collections::HashMap;
use std::path::PathBuf;

use tiny_http::{Header, Method, Response, Server};

use super::manifest::Manifest;
use super::registry::LocalRegistry;
use super::trusted::{Claims, TrustStore};
use super::wire::*;
use super::{PmError, PmResult};

pub fn serve(addr: &str, root: PathBuf, issuers: &[(String, String)]) -> PmResult<()> {
    let reg = LocalRegistry::new(root.clone());
    // Mint the root signing key up front so /coven/rootpub works immediately.
    reg.ensure_key()?;

    // Register the trusted issuers (a JWKS stand-in). Publishing requires a
    // short-lived identity token signed by one of these — never an API key.
    let trust = TrustStore::new(root);
    for (iss, pubhex) in issuers {
        trust.add_issuer(iss, pubhex)?;
    }

    let server = Server::http(addr).map_err(|e| PmError(format!("cannot bind {addr}: {e}")))?;
    let bound = server
        .server_addr()
        .to_ip()
        .map(|a| a.to_string())
        .unwrap_or_else(|| addr.to_string());
    println!(
        "coven registry serving at http://{bound}  (root key {}; {} trusted issuer(s))",
        reg.root_public_hex()?,
        issuers.len()
    );

    for mut request in server.incoming_requests() {
        let (status, body) = handle(&reg, &trust, &mut request);
        let header =
            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("header");
        let resp = Response::from_string(body)
            .with_status_code(status)
            .with_header(header);
        let _ = request.respond(resp);
    }
    Ok(())
}

/// A SLSA-style provenance attestation built from the *verified* identity token
/// claims — bound to the content digest and signed (as part of the record).
fn attestation(claims: &Claims, digest: &str) -> String {
    format!(
        "trusted-publisher|issuer={}|sub={}|repository={}|workflow_ref={}|ref={}|digest={}|verified_at={}",
        claims.iss,
        claims.sub,
        claims.claim("repository").unwrap_or(""),
        claims.claim("workflow_ref").unwrap_or(""),
        claims.claim("ref").unwrap_or(""),
        digest,
        super::tuf::now_unix(),
    )
}

fn handle(reg: &LocalRegistry, trust: &TrustStore, request: &mut tiny_http::Request) -> (u16, String) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url, String::new()),
    };
    let q = parse_query(&query);
    let name = || unwire_name(q.get("name").map(|s| s.as_str()).unwrap_or(""));
    let version = || q.get("version").cloned().unwrap_or_default();

    match (&method, path.as_str()) {
        (Method::Get, "/coven/rootpub") => match reg.root_public_hex() {
            Ok(p) => (200, p),
            Err(e) => errj(500, e),
        },
        (Method::Get, "/coven/index") => json(200, &IndexResp { names: reg.list_all() }),
        (Method::Get, "/coven/timestamp") => match reg.timestamp_signed() {
            Ok(t) => json(200, &t),
            Err(e) => errj(500, e),
        },
        (Method::Get, "/coven/snapshot") => match reg.snapshot_signed() {
            Ok(s) => json(200, &s),
            Err(e) => errj(500, e),
        },
        (Method::Get, "/coven/versions") => json(
            200,
            &VersionsResp {
                records: reg.versions(&name()),
            },
        ),
        (Method::Get, "/coven/record") => match reg.record(&name(), &version()) {
            Ok(r) => json(200, &r),
            Err(e) => errj(404, e),
        },
        (Method::Get, "/coven/source") => match reg.fetch(&name(), &version()) {
            Ok(s) => json(200, &SourceDto::from_source(&s)),
            Err(e) => errj(404, e),
        },
        (Method::Post, "/coven/publish") => {
            let req: PublishReq = match read_json(request) {
                Ok(r) => r,
                Err(e) => return errj(400, e),
            };
            let src = req.source.into_source();
            let manifest = match Manifest::parse(&req.manifest_toml) {
                Ok(m) => m,
                Err(e) => return errj(400, e),
            };
            // Trusted publishing: verify the short-lived identity token, enforce
            // the namespace's publisher policy, and derive the publisher identity
            // + provenance from the *verified* claims (never client-asserted).
            let claims = match trust.verify_token(&req.id_token, super::tuf::now_unix()) {
                Ok(c) => c,
                Err(e) => return errj(401, e),
            };
            if let Err(e) = trust.authorize_publish(&manifest.rune.name, &claims) {
                return errj(403, e);
            }
            let prov = attestation(&claims, &src.hash());
            match reg.publish(&src, &manifest, &claims.sub, Some(prov)) {
                Ok(r) => json(200, &r),
                Err(e) => errj(409, e),
            }
        }
        (Method::Post, "/coven/promote") => {
            let req: PromoteReq = match read_json(request) {
                Ok(r) => r,
                Err(e) => return errj(400, e),
            };
            let claims = match trust.verify_token(&req.id_token, super::tuf::now_unix()) {
                Ok(c) => c,
                Err(e) => return errj(401, e),
            };
            if let Err(e) = trust.authorize_promote(&req.name, &claims) {
                return errj(403, e);
            }
            // Separation of duties: the human releasing must not be the (machine)
            // identity that staged the version.
            if let Ok(rec) = reg.record(&req.name, &req.version)
                && rec.uploaded_by == claims.sub
            {
                return errj(
                    403,
                    PmError(
                        "separation of duties: the identity that published a version may not promote it"
                            .into(),
                    ),
                );
            }
            match reg.promote(&req.name, &req.version, &claims.sub, &req.second_factor) {
                Ok(p) => json(
                    200,
                    &PromoteResp {
                        record: p.record,
                        separation_of_duties: p.separation_of_duties,
                        delta_runtime: p.footprint_delta.runtime.into_iter().collect(),
                        delta_build: p.footprint_delta.build.into_iter().collect(),
                    },
                ),
                Err(e) => errj(400, e),
            }
        }
        (Method::Post, "/coven/yank") => {
            let req: YankReq = match read_json(request) {
                Ok(r) => r,
                Err(e) => return errj(400, e),
            };
            let claims = match trust.verify_token(&req.id_token, super::tuf::now_unix()) {
                Ok(c) => c,
                Err(e) => return errj(401, e),
            };
            if let Err(e) = trust.authorize_promote(&req.name, &claims) {
                return errj(403, e);
            }
            match reg.yank(&req.name, &req.version) {
                Ok(()) => json(200, &OkResp { ok: true }),
                Err(e) => errj(404, e),
            }
        }
        _ => errj(404, PmError(format!("no such endpoint: {path}"))),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(request: &mut tiny_http::Request) -> PmResult<T> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|e| PmError(format!("read body: {e}")))?;
    serde_json::from_str(&body).map_err(|e| PmError(format!("bad request body: {e}")))
}

fn json<T: serde::Serialize>(status: u16, value: &T) -> (u16, String) {
    match serde_json::to_string(value) {
        Ok(s) => (status, s),
        Err(e) => errj(500, PmError(format!("serialize: {e}"))),
    }
}

fn errj(status: u16, e: PmError) -> (u16, String) {
    let body = serde_json::to_string(&ErrResp {
        error: e.to_string(),
    })
    .unwrap_or_else(|_| "{\"error\":\"internal\"}".to_string());
    (status, body)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        } else {
            out.insert(pair.to_string(), String::new());
        }
    }
    out
}
