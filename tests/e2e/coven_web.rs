//! e2e: coven web tests (extracted from tests/e2e.rs).

use std::process::{Command, Stdio};

use super::support::coven::*;

fn http_get_raw(addr: &str, path: &str) -> String {
    http_get_raw_cookie(addr, path, "")
}

/// Like [`http_get_raw`] but also sends a `Cookie:` header — the test's cookie jar for the
/// OAuth nonce that binds `/login` to `/callback` (SEC-007).
fn http_get_raw_cookie(addr: &str, path: &str, cookie: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(8))).unwrap();
    let cookie_hdr = if cookie.is_empty() { String::new() } else { format!("Cookie: {cookie}\r\n") };
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n{cookie_hdr}Connection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf
}

/// The `name=value` of a Set-Cookie header (dropping the attributes after the first `;`).
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap_or("").trim().to_string()
}

/// The value of a response header (case-insensitive), or None.
fn header_value(response: &str, name: &str) -> Option<String> {
    for line in response.lines() {
        if line.trim().is_empty() {
            break; // end of headers
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Pull a query parameter's value out of a URL.
fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(v.to_string());
        }
    }
    None
}

/// Query pairs reach a handler decoded. The coven-web proxy must encode them again
/// before making the upstream request, or escaped separators become new parameters.
#[test]
fn coven_web_proxy_reencodes_decoded_query_values() {
    use std::io::{Read, Write};

    let upstream_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap().to_string();
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let upstream = std::thread::spawn(move || {
        let (mut stream, _) = upstream_listener.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(8))).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).is_ok() {
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&head);
        request_tx.send(request.lines().next().unwrap_or("").to_string()).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });

    let seed_root = unique("cw-proxy-seed");
    let seed = seed_root.join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let assets = unique("cw-proxy-assets");
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &upstream_addr,
            "--signing-key",
            seed.to_str().unwrap(),
            &cw_src,
            &web_addr,
            &upstream_addr,
            &format!("http://{web_addr}"),
            "localhost",
        ])
        .current_dir(&assets)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    let (status, body) = http_get(&web_addr, "/api/coven/versions?name=acme%26state%3Dyanked");
    let request_line = request_rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap();

    let _ = child.kill();
    let _ = child.wait();
    upstream.join().unwrap();
    let _ = std::fs::remove_dir_all(&seed_root);
    let _ = std::fs::remove_dir_all(&assets);

    assert_eq!(status, 200, "proxy response: {body}");
    assert_eq!(
        request_line,
        "GET /coven/versions?name=acme%26state%3Dyanked HTTP/1.1",
        "encoded query separators must remain value data"
    );
}

/// "Log in with GitHub" end to end through the REAL coven-web server: a mock GitHub (a
/// local rustls server) returns a token then a user; coven-web's OAuth `/callback`
/// verifies the signed state, exchanges the code, reads the user, and mints a bearer
/// session. Proves the whole social-login flow — TLS, HTTPS, the OAuth dance, and
/// session minting — composes on the deployed server.
#[test]
fn coven_web_github_login_completes_a_session() {
    use std::io::{Read, Write};
    use std::sync::Arc;
    // Mock-GitHub TLS server with a self-signed `localhost` cert coven-web will trust.
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
    let cert_der = ck.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
    let tls_config = Arc::new(
        rustls::ServerConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
    );
    let gh_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gh_port = gh_listener.local_addr().unwrap().port();
    let cert_path = unique("witchy-cw-gh-cert").join("cert.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();

    let sc = tls_config.clone();
    let gh = std::thread::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = gh_listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while tls.read_exact(&mut b).is_ok() {
                head.push(b[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request_line = String::from_utf8_lossy(&head);
            let body: &[u8] = if request_line.starts_with("POST /login/oauth/access_token") {
                b"{\"access_token\":\"gho_e2e\",\"token_type\":\"bearer\"}"
            } else {
                b"{\"login\":\"octocat\",\"id\":583231}"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            let _ = tls.write_all(resp.as_bytes());
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });

    // Spawn coven-web pointed at the mock for both GitHub base URLs.
    let seed = unique("cw-seed").join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let mock = format!("https://localhost:{gh_port}");
    let assets = unique("cw-assets");
    std::fs::create_dir_all(&assets).unwrap();
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &format!("localhost:{gh_port}"),
            "--signing-key",
            seed.to_str().unwrap(),
            "--secret",
            "github_client_secret=e2esecret",
            &cw_src,
            &web_addr,
            "127.0.0.1:1",
            &format!("http://{web_addr}"),
            "localhost",
            "Ov23liE2E",
            &mock,
            &mock,
        ])
        .current_dir(&assets)
        .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    // /login → 303 to the mock authorize URL; capture the signed anti-CSRF state AND the
    // per-login nonce cookie that binds this flow to the browser (SEC-007).
    let login = http_get_raw(&web_addr, "/auth/github/login");
    let location = header_value(&login, "location").expect("login redirect has a Location");
    assert!(
        location.starts_with(&format!("{mock}/login/oauth/authorize")),
        "login must redirect to the GitHub authorize URL: {location}"
    );
    let state = query_param(&location, "state").expect("authorize URL carries state");
    let cookie = cookie_pair(&header_value(&login, "set-cookie").expect("login sets an oauth_nonce cookie"));

    // SEC-007: the callback WITHOUT the matching nonce cookie must be refused (a replayed
    // state from another session can't complete the login). A refusal is a 403, not a redirect.
    let no_cookie = http_get_raw(&web_addr, &format!("/auth/github/callback?code=thecode&state={state}"));
    assert!(
        header_value(&no_cookie, "location").is_none(),
        "callback without the nonce cookie must be refused, got: {no_cookie}"
    );

    // /callback WITH the cookie → coven-web exchanges the code, reads the user, mints a
    // session, and redirects to the SPA with the bearer in the fragment.
    let callback = http_get_raw_cookie(&web_addr, &format!("/auth/github/callback?code=thecode&state={state}"), &cookie);
    let cb_loc = header_value(&callback, "location").expect("callback redirects with a session");

    let _ = child.kill();
    let _ = child.wait();
    let _ = gh.join();
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_dir_all(&assets);

    assert!(cb_loc.contains("login=octocat"), "session redirect must name the signed-in user: {cb_loc}");
    assert!(cb_loc.contains("token="), "session redirect must carry a bearer token: {cb_loc}");
}

/// base64url, no padding — JWT segment encoding.
fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        if c.len() > 1 {
            out.push(A[(n >> 6 & 63) as usize] as char);
        }
        if c.len() > 2 {
            out.push(A[(n & 63) as usize] as char);
        }
    }
    out
}

/// The two INTEGER contents of a DER `SEQUENCE { INTEGER, INTEGER }` (an RSA public key).
fn two_ints(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
    fn len_at(b: &[u8], i: &mut usize) -> usize {
        let mut len = b[*i] as usize;
        *i += 1;
        if len & 0x80 != 0 {
            let nbytes = len & 0x7f;
            len = 0;
            for _ in 0..nbytes {
                len = (len << 8) | b[*i] as usize;
                *i += 1;
            }
        }
        len
    }
    fn tlv(b: &[u8], i: &mut usize) -> Vec<u8> {
        *i += 1;
        let len = len_at(b, i);
        let v = b[*i..*i + len].to_vec();
        *i += len;
        v
    }
    let mut i = 0;
    i += 1;
    let _ = len_at(der, &mut i);
    (tlv(der, &mut i), tlv(der, &mut i))
}

/// "Log in with Google" (OIDC) end to end through the REAL coven-web server: a mock
/// Google (local rustls) returns a TEST-SIGNED `id_token` then serves its JWKS; coven-web's
/// callback verifies the id_token's signature against the JWKS (and issuer/audience) and
/// mints a session. Proves the OIDC login path — the id_token verification wired through
/// the deployed server, distinct from GitHub's userinfo fetch.
#[test]
fn coven_web_google_login_verifies_id_token_and_completes_a_session() {
    use std::io::{Read, Write};
    use std::sync::Arc;
    // A signing key for the id_token; its public n/e go in the mock JWKS.
    use aws_lc_rs::signature::KeyPair;
    let idk = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("idk");
    let (n_int, e_int) = two_ints(idk.public_key().as_ref());
    let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
    let jwks = format!(
        r#"{{"keys":[{{"kty":"RSA","kid":"g1","n":"{}","e":"{}"}}]}}"#,
        b64url(&strip(&n_int)),
        b64url(&strip(&e_int))
    );
    // The id_token is signed per-login so it can echo the OIDC `nonce` coven-web sends
    // (SEC-008); the mock serves whatever the test puts in `token_slot` after capturing it.
    let header_b64 = b64url(br#"{"alg":"RS256","kid":"g1","typ":"JWT"}"#);
    let sign_id_token = |nonce: &str| -> String {
        let iat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_secs();
        let exp = iat + 3600;
        let payload = format!(
            r#"{{"iss":"https://accounts.google.com","aud":"gClientID","email":"alice@example.com","email_verified":true,"sub":"1","iat":{iat},"exp":{exp},"nbf":0,"nonce":"{nonce}"}}"#
        );
        let signed = format!("{header_b64}.{}", b64url(payload.as_bytes()));
        let mut sig = vec![0u8; idk.public_modulus_len()];
        idk.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut sig,
        )
        .expect("sign id_token");
        format!(r#"{{"id_token":"{signed}.{}","token_type":"bearer"}}"#, b64url(&sig))
    };
    let token_slot = Arc::new(std::sync::Mutex::new(String::new()));

    // Mock-Google TLS server: POST /token -> the id_token; GET /certs -> the JWKS.
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
    let cert_der = ck.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
    let tls_config = Arc::new(
        rustls::ServerConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
    );
    let g_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let g_port = g_listener.local_addr().unwrap().port();
    let cert_path = unique("witchy-cw-google-cert").join("cert.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();

    let sc = tls_config.clone();
    let token_slot_mock = Arc::clone(&token_slot);
    let g = std::thread::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = g_listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while tls.read_exact(&mut b).is_ok() {
                head.push(b[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = if String::from_utf8_lossy(&head).starts_with("POST /token") {
                token_slot_mock.lock().unwrap().clone()
            } else {
                jwks.clone()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = tls.write_all(resp.as_bytes());
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });

    let seed = unique("cw-g-seed").join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let mock = format!("https://localhost:{g_port}");
    let assets = unique("cw-g-assets");
    std::fs::create_dir_all(&assets).unwrap();
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &format!("localhost:{g_port}"),
            "--signing-key",
            seed.to_str().unwrap(),
            "--secret",
            "google_client_secret=gsecret",
            &cw_src,
            &web_addr,
            "127.0.0.1:1",
            &format!("http://{web_addr}"),
            "localhost",
            "", // github disabled
            "https://github.com",
            "https://api.github.com",
            "gClientID",
            &format!("{mock}/authorize"),
            &format!("{mock}/token"),
            &format!("{mock}/certs"),
        ])
        .current_dir(&assets)
        .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    let login = http_get_raw(&web_addr, "/auth/google/login");
    let location = header_value(&login, "location").expect("login redirect has a Location");
    let state = query_param(&location, "state").expect("authorize URL carries state");
    let cookie = cookie_pair(&header_value(&login, "set-cookie").expect("login sets an oauth_nonce cookie"));
    // SEC-008: the authorize URL carries the OIDC nonce; the mock's id_token must echo it.
    let nonce = query_param(&location, "nonce").expect("the Google authorize URL carries the OIDC nonce");
    *token_slot.lock().unwrap() = sign_id_token(&nonce);

    let callback = http_get_raw_cookie(&web_addr, &format!("/auth/google/callback?code=thecode&state={state}"), &cookie);
    let cb_loc = header_value(&callback, "location")
        .unwrap_or_else(|| panic!("callback did not redirect; raw response:\n{callback}"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = g.join();
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_dir_all(&assets);

    assert!(
        cb_loc.contains("login=alice%40example.com"),
        "session redirect must name the verified email: {cb_loc}"
    );
    assert!(cb_loc.contains("token="), "session redirect must carry a bearer token: {cb_loc}");
}
