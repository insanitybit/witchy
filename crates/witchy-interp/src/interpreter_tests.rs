    use super::*;

    #[test]
    fn build_step_generates_source_through_confined_caps() {
        // A build step reads a schema (BuildRead) and writes generated source
        // (BuildOut). Its authority is exactly the confined grants minted here.
        let dir = std::env::temp_dir().join(format!("witchy_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src_root = dir.join("proj");
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&src_root).unwrap();
        std::fs::write(src_root.join("api.proto"), "service Foo").unwrap();

        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"api.witchy\", \"// generated from: \" + schema.read_build(\"api.proto\"))\n",
        )
        .expect("parse");
        let grants = BuildGrants {
            out_dir: out_dir.clone(),
            read_roots: vec![src_root.clone()],
            ..Default::default()
        };
        let generated = run_build_step(module, grants).expect("build step runs");
        assert_eq!(generated, vec!["api.witchy".to_string()]);
        let body = std::fs::read_to_string(out_dir.join("api.witchy")).unwrap();
        assert_eq!(body, "// generated from: service Foo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_step_cannot_escape_or_demand_ungranted_caps() {
        let dir = std::env::temp_dir().join(format!("witchy_build_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // BuildRead demanded but not granted ⇒ refused before running.
        let m = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"x\", schema.read_build(\"a\"))\n",
        )
        .unwrap();
        let g = BuildGrants { out_dir: dir.join("out"), ..Default::default() };
        let err = run_build_step(m, g).expect_err("ungranted BuildRead must be refused");
        assert!(err.message.contains("no read grant"), "{}", err.message);
        // A confined BuildOut cannot write outside its sandbox.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let g2 = BuildGrants { out_dir: dir.join("out2"), ..Default::default() };
        let err = run_build_step(m2, g2).expect_err("a `..` write must be refused");
        assert!(err.message.contains("escapes the Dir capability"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn build_env_reads_only_named_variables() {
        // A build step never sees the whole environment: `BuildEnv` carries an
        // allow-list of *named* keys, and reading anything else is refused —
        // even a variable that exists in the process env.
        let dir = std::env::temp_dir().join(format!("witchy_build_env_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let granted = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        )
        .unwrap();
        let g = BuildGrants {
            out_dir: dir.join("out"),
            env: [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into(),
            ..Default::default()
        };
        run_build_step(granted, g).expect("a named key reads fine");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "yes");

        // The same grant cannot read a key it didn't name.
        let denied = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_SECRET\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            env: [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into(),
            ..Default::default()
        };
        let err = run_build_step(denied, g2).expect_err("an unlisted key must be refused");
        assert!(err.message.contains("not in this BuildEnv grant's allow-list"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_net_fetches_only_allow_listed_hosts() {
        // A local one-shot HTTP listener stands in for "the network": the build
        // step may fetch from it only because the grant allow-lists exactly that
        // host:port; any other destination is refused before a packet moves.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let body = "schema-v1";
            let _ = sock.write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                    .as_bytes(),
            );
        });

        let dir = std::env::temp_dir().join(format!("witchy_build_net_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let module = witchy_syntax::parser::parse_module(
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"got.txt\", dl.fetch_build(\"{addr}\", \"/schema\"))\n"
            ),
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            net_hosts: vec![addr.clone()],
            ..Default::default()
        };
        run_build_step(module, grants).expect("allow-listed fetch runs");
        assert_eq!(std::fs::read_to_string(dir.join("out/got.txt")).unwrap(), "schema-v1");
        server.join().unwrap();

        // A host NOT on the allow-list is refused — even one that exists.
        let m2 = witchy_syntax::parser::parse_module(
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"x\", dl.fetch_build(\"{addr}\", \"/\"))\n"
            ),
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            net_hosts: vec!["allowed.example:80".to_string()],
            ..Default::default()
        };
        let e = run_build_step(m2, g2).expect_err("an un-allow-listed host must be refused");
        assert!(e.message.contains("not in this BuildNet grant's allow-list"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_exec_runs_only_allow_listed_tools() {
        // `cat` echoes its stdin, so the generated file is exactly the input —
        // deterministic. The grant allow-lists `cat`; anything else is refused.
        let dir = std::env::temp_dir().join(format!("witchy_build_exec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"cat\", \"piped-input\"))\n",
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            exec_tools: vec!["cat".to_string()],
            ..Default::default()
        };
        let generated = run_build_step(module, grants).expect("cat is allow-listed");
        assert_eq!(generated, vec!["x.txt".to_string()]);
        assert_eq!(std::fs::read_to_string(dir.join("out/x.txt")).unwrap(), "piped-input");

        // A tool NOT on the allow-list is refused before it runs.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"rm\", \"-rf /\"))\n",
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            exec_tools: vec!["cat".to_string()],
            ..Default::default()
        };
        let err = run_build_step(m2, g2).expect_err("an un-allow-listed tool must be refused");
        assert!(err.message.contains("not in this BuildExec grant's allow-list"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_capability_rejects_path_traversal() {
        // A Dir capability is confined to its subtree. `resolve` must reject any
        // path that would escape it, so a holder (e.g. an untrusted library
        // handed a narrow Dir) can read within the subtree but never above it.
        let base = witchy_runtime::confine::ConfinedDir::open_ambient(
            std::path::Path::new("."),
        )
        .unwrap();
        // Positive control: a path inside the subtree resolves (Cargo.toml is at
        // the crate root, the CWD for tests).
        assert!(base.file("Cargo.toml", true).is_ok());
        // `..` is rejected lexically, before any filesystem access.
        assert!(base.file("../secret", true).is_err());
        assert!(base.file("src/../../etc/passwd", true).is_err());
        // Absolute paths are rejected: the capability is a subtree, not root.
        assert!(base.file("/etc/passwd", true).is_err());
    }

    /// The capability thesis at the language level: a function that was never
    /// handed the Console capability cannot print, even though `print` exists.
    #[test]
    fn function_without_capability_cannot_print() {
        let src = r#"
fn leak(secret: String) -> ():
    print(secret)

fn main(console: Console):
    leak("password")
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("method-only") && e.message.contains("console.print"),
            "expected a capability-method error, got: {}",
            e.message
        );
    }

    #[test]
    fn dir_capability_reads_attenuates_and_confines() {
        let root = std::env::temp_dir().join(format!("witchy_fs_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/hi.txt"), "hi!").unwrap();

        // Attenuate to a subdir and read a file within it.
        let ok = r#"
fn main(console: Console, root: Dir):
    let d = root.subtree("sub")
    console.print(d.read("hi.txt"))
"#;
        assert_eq!(run_in(ok, &root).unwrap(), vec!["hi!"]);

        // Confinement: `..` cannot escape the granted subtree.
        let escape = r#"
fn main(console: Console, root: Dir):
    console.print(root.read("../secret"))
"#;
        assert!(run_in(escape, &root).is_err());

        // A function with no Dir cannot read (no way to obtain the capability).
        let no_cap = r#"
fn sneaky() -> String:
    root.read("sub/hi.txt")

fn main(console: Console, root: Dir):
    console.print(sneaky())
"#;
        assert!(run_in(no_cap, &root).is_err());

        // Confinement holds against symlinks: a link inside the subtree pointing
        // outside it must not be followable.
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("witchy_outside_{}", std::process::id()));
            std::fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("sub/escape")).ok();
            let via_symlink = r#"
fn main(console: Console, root: Dir):
    let d = root.subtree("sub")
    console.print(d.read("escape"))
"#;
            assert!(run_in(via_symlink, &root).is_err());
            std::fs::remove_file(&outside).ok();
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn net_capability_connects_attenuates_and_denies() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        // One-shot loopback echo server.
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = r.get_mut().write_all(line.as_bytes());
            }
        });

        // Attenuate to the one held address, connect, send, receive the echo.
        let (host, port) = addr.rsplit_once(':').expect("addr is host:port");
        let ok = format!(
            r#"
fn main(console: Console, net: Net):
    let only = net.only(Net.tcp("{host}", {port}))
    let s = only.connect("{addr}")
    s.send_line("ping")
    console.print(s.recv_line())
"#
        );
        // Link in the bundled std (`policy` is preluded), then run.
        let linked_ok = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(&ok).expect("parse"))],
            "main",
        )
        .expect("link");
        assert_eq!(run_module(linked_ok, ".", vec![addr.clone()]).unwrap(), vec!["ping"]);
        server.join().ok();

        // Denied: connecting to an address not in the allow-list.
        let denied = r#"
fn main(console: Console, net: Net):
    let s = net.connect("10.255.255.1:80")
    s.send_line("x")
"#;
        assert!(run_with(denied, ".", vec![addr.clone()]).is_err());

        // Denied: cannot attenuate to an address not already held.
        let bad_restrict = r#"
fn main(console: Console, net: Net):
    let bad = net.only(Net.tcp("10.255.255.1", 80))
    console.print("unreachable")
"#;
        let linked_bad = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(bad_restrict).expect("parse"))],
            "main",
        )
        .expect("link");
        assert!(run_module(linked_bad, ".", vec![addr]).is_err());
    }

    #[test]
    fn recv_bytes_does_not_preallocate_attacker_count() {
        // (BUG-065) `sock.recv_bytes(n)` must NOT pre-allocate `n` bytes up front —
        // `n` is an attacker-controlled count (an HTTP Content-Length up to i64::MAX),
        // so `vec![0u8; n]` before reading a single byte is a remote OOM. The fix reads
        // in bounded chunks: a huge `n` against a peer that sends only a few bytes then
        // closes returns exactly the bytes received, without allocating the claimed
        // count. (The compiled backend already reads chunked — parity.)
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(b"hi");
                // dropping `stream` closes the connection => EOF for the reader.
            }
        });

        let (host, port) = addr.rsplit_once(':').expect("addr is host:port");
        // Claim ~2 billion bytes but the peer sends 2 then closes.
        let src = format!(
            r#"
fn main(console: Console, net: Net):
    let only = net.only(Net.tcp("{host}", {port}))
    let s = only.connect("{addr}")
    console.print(s.recv_bytes(2000000000))
"#
        );
        let linked = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(&src).expect("parse"))],
            "main",
        )
        .expect("link");
        assert_eq!(run_module(linked, ".", vec![addr.clone()]).unwrap(), vec!["hi"]);
        server.join().ok();
    }


    #[test]
    fn http_client_builder_loopback() {
        // The reqwest-style client builder (get_request().with_header(...).send(fetch))
        // against a raw TCP server: it sends the method/path/header and parses
        // the response status and body.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nhello!!")
                .unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });

        let src = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let req = http.get_request("http://{addr}/path")
        .with_header("X-Test", "abc")
        .with_query("q", "hi")
    match req.send(net.fetch("http://{addr}")):
        Ok(resp) ->
            console.print("${{http.status(resp)}}")
            console.print(http.body(resp))
            console.print("${{http.is_success(resp)}}")
        Err(e) -> console.print("err: " + http.http_error_message(e))
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let out = run_module(linked, ".", vec![addr.clone()]).expect("run");
        assert_eq!(out, vec!["200", "hello!!", "true"]);
        let req = srv.join().unwrap();
        assert!(req.contains("GET /path?q=hi HTTP/1.1"), "req: {req}");
        assert!(req.contains("X-Test: abc"), "req: {req}");
    }










    #[test]
    fn handlers_cannot_reach_the_network() {
        // The capability guarantee: this handler receives no Net, so even trying to
        // open a socket is a compile-time (type) error — it can't be written.
        let src = r#"
import server
from http import Request, Response
fn evil(req: Request) -> Response:
    let s = net.connect("10.0.0.1:80")
    server.text(200, "leaked")
fn main(console: Console, net: Net):
    let app = server.router().get("/", evil)
    server.serve_n(net, "127.0.0.1:0", app, 0)
"#;
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        // Type-check the linked program: `connect` needs a Net the handler lacks.
        assert!(witchy_types::typeck::check(&linked).is_err());
    }

    #[test]
    fn library_cannot_fabricate_a_capability() {
        // `steal` references `console` it was never given — caught at compile
        // time as an unbound variable (no ambient authority to grab).
        let evil = r#"
pub fn steal(secret: String) -> String:
    console.print(secret)
"#;
        let app = r#"
import evil

fn main(console: Console):
    console.print(evil.steal("data"))
"#;
        let linked = crate::pipeline::link(
            vec![
                ("evil".into(), parse_module(evil).unwrap()),
                ("app".into(), parse_module(app).unwrap()),
            ],
            "app",
        )
        .unwrap();
        assert!(witchy_types::typeck::check(&linked).is_err());
    }
