    use super::*;

    #[test]
    fn evaluates_arithmetic_and_precedence() {
        let out = run(r#"
fn main(console: Console):
    print(console, __render((1 + (2 * 3))))
"#)
            .unwrap();
        assert_eq!(out, vec!["7"]);
    }

    #[test]
    fn mints_a_grantable_user_cap() {
        // (RFC-0038) a `main` binding a bare grantable cap gets a sealed record
        // minted from the `[user_caps]` grant fields, readable in its own module.
        let src = "grantable capability UiRoot:\n    policy: String\n\nfn policy_of(u: UiRoot) -> String:\n    match u:\n        UiRoot(p) -> p\n\nfn main(console: Console, ui: UiRoot):\n    print(console, policy_of(ui))\n";
        let module = witchy_syntax::parser::parse_module(src).unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "coven-web".to_string());
        let mut grants: UserCapGrants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        let out = run_module_user_caps(module, ".", vec![], vec![], vec![], grants).unwrap();
        assert_eq!(out, vec!["coven-web".to_string()]);

        // Without the grant, minting fails loudly (an under-grant).
        let module2 = witchy_syntax::parser::parse_module(src).unwrap();
        let err = run_module_user_caps(module2, ".", vec![], vec![], vec![], Default::default())
            .unwrap_err();
        assert!(err.message.contains("UiRoot") && err.message.contains("user_caps"), "{}", err.message);
    }

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
            "fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"api.witchy\", \"// generated from: \" + read_build(schema, \"api.proto\"))\n",
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
            "fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x\", read_build(schema, \"a\"))\n",
        )
        .unwrap();
        let g = BuildGrants { out_dir: dir.join("out"), ..Default::default() };
        let err = run_build_step(m, g).expect_err("ungranted BuildRead must be refused");
        assert!(err.message.contains("no read grant"), "{}", err.message);
        // A confined BuildOut cannot write outside its sandbox.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut):\n    write_out(out, \"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let g2 = BuildGrants { out_dir: dir.join("out2"), ..Default::default() };
        let err = run_build_step(m2, g2).expect_err("a `..` write must be refused");
        assert!(err.message.contains("escapes the Dir capability"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_generated_source_compiles_and_runs() {
        // The whole point: a build step emits real witchy source, which then flows
        // into the normal compile and runs. Here `build` writes a `greet` module,
        // and a consumer imports and calls it.
        let dir = std::env::temp_dir().join(format!("witchy_build_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let build_mod = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    write_out(out, \"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"hi from generated code\\\"\" + nl)\n",
        )
        .expect("parse build module");
        let gen_dir = dir.join("gen");
        let files = run_build_step(build_mod, BuildGrants { out_dir: gen_dir.clone(), ..Default::default() })
            .expect("build step runs");
        assert_eq!(files, vec!["greet.witchy".to_string()]);
        let generated = std::fs::read_to_string(gen_dir.join("greet.witchy")).unwrap();
        // The generated source links with a consumer and runs.
        let consumer = "import greet\nfn main(console: Console):\n    print(console, greet.greeting())\n";
        let out = run_program(&[("greet", generated.as_str()), ("main", consumer)], "main")
            .expect("generated source compiles and runs");
        assert_eq!(out, vec!["hi from generated code"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_read_spans_multiple_granted_roots() {
        // A BuildRead grant can name several confined roots; `read_build` resolves
        // a path against the first root that holds it — and still nothing else.
        let dir = std::env::temp_dir().join(format!("witchy_build_mr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("from_a.txt"), "ALPHA").unwrap();
        std::fs::write(b.join("from_b.txt"), "BETA").unwrap();

        let module = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, src: BuildRead):\n    write_out(out, \"g.txt\", read_build(src, \"from_a.txt\") + \"/\" + read_build(src, \"from_b.txt\"))\n",
        )
        .unwrap();
        let grants = BuildGrants {
            out_dir: dir.join("out"),
            read_roots: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        run_build_step(module, grants).expect("reads across both roots");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "ALPHA/BETA");

        // A file in neither root is refused.
        let m2 = witchy_syntax::parser::parse_module(
            "fn build(out: BuildOut, src: BuildRead):\n    write_out(out, \"g.txt\", read_build(src, \"nope.txt\"))\n",
        )
        .unwrap();
        let g2 = BuildGrants { out_dir: dir.join("out2"), read_roots: vec![a, b], ..Default::default() };
        let e = run_build_step(m2, g2).expect_err("a path in no granted root must fail");
        assert!(e.message.contains("not found in any granted read root"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_env_reads_only_named_variables() {
        // A build step never sees the whole environment: `BuildEnv` carries an
        // allow-list of *named* keys, and reading anything else is refused —
        // even a variable that exists in the process env.
        let dir = std::env::temp_dir().join(format!("witchy_build_env_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("WITCHY_BUILD_ALLOWED", "yes") };
        unsafe { std::env::set_var("WITCHY_BUILD_SECRET", "leak?") };
        let granted = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match get_build_env(env, \"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    write_out(out, \"g.txt\", v)\n",
        )
        .unwrap();
        let g = BuildGrants {
            out_dir: dir.join("out"),
            env_keys: vec!["WITCHY_BUILD_ALLOWED".to_string()],
            ..Default::default()
        };
        run_build_step(granted, g).expect("a named key reads fine");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "yes");

        // The same grant cannot read a key it didn't name.
        let denied = witchy_syntax::parser::parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match get_build_env(env, \"WITCHY_BUILD_SECRET\"):\n        Some(x) -> x\n        None -> \"unset\"\n    write_out(out, \"g.txt\", v)\n",
        )
        .unwrap();
        let g2 = BuildGrants {
            out_dir: dir.join("out2"),
            env_keys: vec!["WITCHY_BUILD_ALLOWED".to_string()],
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
                "fn build(out: BuildOut, dl: BuildNet):\n    write_out(out, \"got.txt\", fetch_build(dl, \"{addr}\", \"/schema\"))\n"
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
                "fn build(out: BuildOut, dl: BuildNet):\n    write_out(out, \"x\", fetch_build(dl, \"{addr}\", \"/\"))\n"
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
            "fn build(out: BuildOut, cc: BuildExec):\n    write_out(out, \"x.txt\", run_tool(cc, \"cat\", \"piped-input\"))\n",
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
            "fn build(out: BuildOut, cc: BuildExec):\n    write_out(out, \"x.txt\", run_tool(cc, \"rm\", \"-rf /\"))\n",
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
        use std::path::Path;
        let base = Path::new(".");
        // Positive control: a path inside the subtree resolves (Cargo.toml is at
        // the crate root, the CWD for tests).
        assert!(resolve(base, "Cargo.toml").is_ok());
        // `..` is rejected lexically, before any filesystem access.
        assert!(resolve(base, "../secret").is_err());
        assert!(resolve(base, "src/../../etc/passwd").is_err());
        // Absolute paths are rejected: the capability is a subtree, not root.
        assert!(resolve(base, "/etc/passwd").is_err());
    }

    #[test]
    fn calls_user_functions_and_concats_strings() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    print(console, ("doubled: " + __render(double(21))))
"#;
        assert_eq!(run(src).unwrap(), vec!["doubled: 42"]);
    }

    #[test]
    fn pipelines_thread_left_to_right() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    let result = __render(double(4))
    print(console, result)
"#;
        assert_eq!(run(src).unwrap(), vec!["8"]);
    }

    #[test]
    fn match_with_constructors_and_guards() {
        let src = r#"
fn describe(e: Event) -> String:
    match e:
        Click(x, _) if (x > 0) -> "right click"
        Click(_, _) -> "other click"
        Closed -> "closed"
        _ -> "unknown"

fn main(console: Console):
    print(console, describe(Click(5, 9)))
    print(console, describe(Click((-1), 0)))
    print(console, describe(Closed))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["right click", "other click", "closed"]
        );
    }

    #[test]
    fn if_else_and_let_bindings() {
        let src = r#"
fn sign(n: Int) -> String:
    let label = if (n > 0): "positive" else: "non-positive"
    label

fn main(console: Console):
    print(console, sign(3))
    print(console, sign((-2)))
"#;
        assert_eq!(run(src).unwrap(), vec!["positive", "non-positive"]);
    }

    #[test]
    fn recursion_works() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main(console: Console):
    print(console, __render(fact(5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["120"]);
    }

    #[test]
    fn reports_unknown_function() {
        let e = run(r#"
fn main():
    nope()
"#).unwrap_err();
        assert!(e.message.contains("unknown function"));
    }

    /// The capability thesis at the language level: a function that was never
    /// handed the Console capability cannot print, even though `print` exists.
    #[test]
    fn function_without_capability_cannot_print() {
        let src = r#"
fn leak(secret: String) -> Nil:
    print(secret)

fn main(console: Console):
    leak("password")
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("Console capability"),
            "expected a capability error, got: {}",
            e.message
        );
    }

    /// Holding the capability, the same effect succeeds — capabilities
    /// propagate by being passed explicitly.
    #[test]
    fn capability_can_be_threaded_to_a_helper() {
        let src = r#"
fn announce(console: Console, who: String) -> Nil:
    print(console, ("hello, " + who))

fn main(console: Console):
    announce(console, "witchy")
"#;
        assert_eq!(run(src).unwrap(), vec!["hello, witchy"]);
    }

    #[test]
    fn dir_capability_reads_attenuates_and_confines() {
        let root = std::env::temp_dir().join(format!("witchy_fs_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/hi.txt"), "hi!").unwrap();

        // Attenuate to a subdir and read a file within it.
        let ok = r#"
fn main(console: Console, root: Dir):
    let d = subtree(root, "sub")
    print(console, read(d, "hi.txt"))
"#;
        assert_eq!(run_in(ok, &root).unwrap(), vec!["hi!"]);

        // Confinement: `..` cannot escape the granted subtree.
        let escape = r#"
fn main(console: Console, root: Dir):
    print(console, read(root, "../secret"))
"#;
        assert!(run_in(escape, &root).is_err());

        // A function with no Dir cannot read (no way to obtain the capability).
        let no_cap = r#"
fn sneaky() -> String:
    read(root, "sub/hi.txt")

fn main(console: Console, root: Dir):
    print(console, sneaky())
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
    let d = subtree(root, "sub")
    print(console, read(d, "escape"))
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
    let s = connect(only, "{addr}")
    send_line(s, "ping")
    print(console, recv_line(s))
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
    let s = connect(net, "10.255.255.1:80")
    send_line(s, "x")
"#;
        assert!(run_with(denied, ".", vec![addr.clone()]).is_err());

        // Denied: cannot attenuate to an address not already held.
        let bad_restrict = r#"
fn main(console: Console, net: Net):
    let bad = net.only(Net.tcp("10.255.255.1", 80))
    print(console, "unreachable")
"#;
        let linked_bad = crate::pipeline::link(
            vec![("main".to_string(), witchy_syntax::parser::parse_module(bad_restrict).expect("parse"))],
            "main",
        )
        .expect("link");
        assert!(run_module(linked_bad, ".", vec![addr]).is_err());
    }

    #[test]
    fn net_server_listen_accept_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        // A free port to hand the witchy server (bind+drop to discover it).
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
fn main(console: Console, net: Net):
    let server = listen(net, "{addr}")
    let sock = accept(server)
    let line = recv_line(sock)
    print(console, line)
    send_bytes(sock, "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello witchy")
    close(sock)
"#
        );
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_with(&src, ".", allow));

        // Connect once the server has bound (retry through the bind race).
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect to witchy server");
        stream.write_all(b"GET /hi HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.ends_with("hello witchy"), "resp: {resp}");

        let out = server.join().unwrap().unwrap();
        assert_eq!(out, vec!["GET /hi HTTP/1.1\r"]);
    }

    #[test]
    fn recv_bytes_does_not_preallocate_attacker_count() {
        // (BUG-065) `recv_bytes(sock, n)` must NOT pre-allocate `n` bytes up front —
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
    let s = connect(only, "{addr}")
    print(console, recv_bytes(s, 2000000000))
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
    fn serve_loopback_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router()
        .get("/", fn(req: Request): server.text(200, "home"))
        .get("/users/:id", fn(req: Request): server.text(200, "user " + server.param(req, "id")))
        .post("/echo", fn(req: Request): server.text(201, server.request_body(req)))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        // Link in the bundled std (http + its deps), then run on a thread.
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.ends_with("home"), "r1: {r1}");
        let r2 = request("GET /users/42 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("user 42"), "r2: {r2}");
        let r3 = request("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello");
        assert!(r3.contains("201 ") && r3.ends_with("hello"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn http_client_builder_loopback() {
        // The reqwest-style client builder (get_request().with_header(...).send(net))
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
    match req.send(net):
        Ok(resp) ->
            print(console, __render(http.status(resp)))
            print(console, http.body(resp))
            print(console, __render(http.is_success(resp)))
        Err(e) -> print(console, "err: " + e)
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
    fn serve_status_constructors_roundtrip() {
        // The status-named response constructors (created/bad_request/
        // unauthorized/no_content) render the right status line and reason.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/make", fn(req: Request): server.created("made")).get("/bad", fn(req: Request): server.bad_request("nope")).get("/secret", fn(req: Request): server.unauthorized("auth")).delete("/item", fn(req: Request): server.no_content())
    server.serve_n(net, "{addr}", app, 4)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("POST /make HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(r1.contains("201 Created") && r1.ends_with("made"), "r1: {r1}");
        let r2 = request("GET /bad HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("400 Bad Request") && r2.ends_with("nope"), "r2: {r2}");
        let r3 = request("GET /secret HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("401 Unauthorized") && r3.ends_with("auth"), "r3: {r3}");
        let r4 = request("DELETE /item HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r4.contains("204 No Content"), "r4: {r4}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_method_not_allowed_vs_not_found() {
        // A known path with the wrong method is a 405; an unknown path is a 404.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/items", fn(req: Request): server.created("ok"))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let ok = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(ok.contains("201 Created"), "ok: {ok}");
        let wrong = request("GET /items HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(wrong.contains("405 Method Not Allowed"), "wrong: {wrong}");
        let missing = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.contains("404 Not Found"), "missing: {missing}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_var_receiver_method_resolution() {
        // Method calls on a variable receiver (`var app = router(); app = app.get(...)`)
        // resolve the overloaded `get`/`post` by the tracked variable type (Router),
        // even though http/server/json all export `get`.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
from http import Request, Response
fn main(console: Console, net: Net):
    var app = server.router()
    app = app.get("/", fn(req: Request): server.ok("home"))
    app = app.post("/items", fn(req: Request): server.created("made"))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("home"), "g: {g}");
        let p = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(p.contains("201 Created") && p.ends_with("made"), "p: {p}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_any_method_route_roundtrip() {
        // An `any` route answers every verb (the `*` wildcard method).
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().any("/ping", fn(req: Request): server.ok(server.method(req)))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("GET"), "g: {g}");
        let d = request("DELETE /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(d.contains("200 OK") && d.ends_with("DELETE"), "d: {d}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_middleware_nest_and_notfound_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response

// A tower-style Layer that tags every response with a header.
fn tagger(next: fn(Request) -> Response) -> fn(Request) -> Response:
    fn(req: Request): tag(next(req))

fn tag(resp: Response) -> Response:
    server.with_header(resp, "x-by", "witchy")

fn main(console: Console, net: Net):
    let api = server.router().get("/ping", fn(req: Request): server.text(200, "pong"))
    let app = server.router().get("/", fn(req: Request): server.text(200, "root")).nest("/api", api).layer(tagger)
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        // Middleware tagged the response; root handler ran.
        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("x-by: witchy") && r1.ends_with("root"), "r1: {r1}");
        // Nested route under /api.
        let r2 = request("GET /api/ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("pong"), "r2: {r2}");
        // Unknown path -> 404 (still tagged by the layer).
        let r3 = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("404 ") && r3.contains("x-by: witchy"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_handler_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
from http import Request, Response
from json import Json
fn greet(req: Request) -> Response:
    server.json_value(200, JsonObject([("hello", JsonString(server.param(req, "name")))]))
fn main(console: Console, net: Net):
    let app = server.router().get("/hello/:name", greet)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        stream.write_all(b"GET /hello/witchy HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("application/json"), "resp: {resp}");
        assert!(resp.contains("\"hello\"") && resp.contains("\"witchy\""), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_body_decode_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
import option
from http import Request, Response
from json import Json
fn name_of(doc: Json) -> String:
    match json.get(doc, "name"):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "?"
fn echo_name(req: Request) -> Response:
    match server.json_body(req):
        Ok(doc) -> server.text(200, name_of(doc))
        Err(e) -> server.text(400, e)
fn main(console: Console, net: Net):
    let app = server.router().post("/", echo_name)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "{\"name\":\"witchy\"}";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_form_field_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn main(console: Console, net: Net):
    let app = server.router().post("/", fn(req: Request): server.text(200, server.form_field(req, "name")))
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "name=witchy&lang=rust";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_static_files_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // The handler captures a Dir rooted at examples/data and serves from it.
        let src = format!(
            r#"
import http
import server
from http import Request, Response
fn file_server(dir: Dir) -> fn(Request) -> Response:
    fn(req: Request): serve_file(dir, server.param(req, "path"))
fn serve_file(dir: Dir, p: String) -> Response:
    if exists(dir, p):
        server.text(200, read(dir, p))
    else:
        server.not_found()
fn main(console: Console, net: Net, root: Dir):
    let examples = subtree(root, "examples")
    let data = subtree(examples, "data")
    let app = server.router().get("/files/*path", file_server(data))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = witchy_syntax::parser::parse_module(&src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, concat!(env!("CARGO_MANIFEST_DIR"), "/../.."), allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("GET /files/greeting.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.contains("sandboxed Dir"), "r1: {r1}");
        let r2 = request("GET /files/nope.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("404 "), "r2: {r2}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn handlers_cannot_reach_the_network() {
        // The capability guarantee: a pure handler has no Net, so even trying to
        // open a socket is a compile-time (type) error — it can't be written.
        let src = r#"
import server
from http import Request, Response
fn evil(req: Request) -> Response:
    let s = connect(net, "10.0.0.1:80")
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
    fn nonexhaustive_match_diagnostic_renders_home_type_bare() {
        // BUG-292: a home-module type/variant renders bare (the spelling the reader
        // wrote) in a non-exhaustive-match diagnostic — never the `prog.Color`
        // file-stem qualifier — and the missing-variant list is backticked.
        let src = r#"
type Color:
    Red
    Blue

fn pick(c: Color) -> Int:
    match c:
        Red -> 1

fn main(console: Console):
    print(console, "${pick(Red)}")
"#;
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("prog".to_string(), parsed)], "prog").expect("link");
        let err = witchy_types::typeck::check(&linked).expect_err("non-exhaustive match");
        assert!(err.message.contains("non-exhaustive match on `Color`"), "{}", err.message);
        assert!(err.message.contains("missing `Blue`"), "{}", err.message);
        assert!(
            !err.message.contains("prog.Color") && !err.message.contains("prog.Blue"),
            "home-module file stem leaked: {}",
            err.message
        );
    }

    #[test]
    fn modules_qualified_calls() {
        let strutil = r#"
fn shout(name: String) -> String:
    ("HELLO, " + name)
"#;
        let app = r#"
import strutil

fn main(console: Console):
    print(console, strutil.shout("witchy"))
"#;
        assert_eq!(
            run_program(&[("strutil", strutil), ("app", app)], "app").unwrap(),
            vec!["HELLO, witchy"]
        );
    }

    #[test]
    fn library_uses_only_passed_capabilities() {
        // The app chooses to hand the logger its Console.
        let logger = r#"
fn log(console: Console, msg: String):
    print(console, ("[log] " + msg))
"#;
        let app = r#"
import logger

fn main(console: Console):
    logger.log(console, "hi")
"#;
        assert_eq!(
            run_program(&[("logger", logger), ("app", app)], "app").unwrap(),
            vec!["[log] hi"]
        );
    }

    #[test]
    fn library_cannot_fabricate_a_capability() {
        // `steal` references `console` it was never given — caught at compile
        // time as an unbound variable (no ambient authority to grab).
        let evil = r#"
fn steal(secret: String) -> String:
    print(console, secret)
"#;
        let app = r#"
import evil

fn main(console: Console):
    print(console, evil.steal("data"))
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

    #[test]
    fn calling_unimported_module_is_a_link_error() {
        let app = r#"
fn main(console: Console):
    print(console, other.foo())
"#;
        assert!(run_program(&[("app", app)], "app").is_err());
    }

    #[test]
    fn float_arithmetic() {
        let src = r#"
fn half(x: Float) -> Float:
    (x / 2.0)

fn main(console: Console):
    print(console, __render(half(7.0)))
"#;
        assert_eq!(run(src).unwrap(), vec!["3.5"]);
    }

    #[test]
    fn boolean_operators() {
        let src = r#"
fn classify(n: Int) -> String:
    if ((n > 0) && (n < 10)):
        "small positive"
    else if ((n <= 0) || (n >= 100)):
        "out of range"
    else:
        "other"

fn main(console: Console):
    print(console, classify(5))
    print(console, classify((-1)))
    print(console, classify(50))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["small positive", "out of range", "other"]
        );
    }

    #[test]
    fn tuples_destructure_and_match() {
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main(console: Console):
    let (q, r) = divmod(17, 5)
    print(console, __render(q))
    print(console, __render(r))
    let pair = (1, "one")
    match pair:
        (n, name) -> print(console, ((__render(n) + "=") + name))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "2", "1=one"]);
    }

    #[test]
    fn generic_identity_runs() {
        let src = r#"
fn id(x: a) -> a:
    x

fn main(console: Console):
    print(console, id("hi"))
    print(console, __render(id(5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["hi", "5"]);
    }

    #[test]
    fn generic_adt_runs() {
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(n) -> ("ok " + __render(n))
        Err(msg) -> ("err " + msg)

fn main(console: Console):
    print(console, show(Ok(7)))
    print(console, show(Err("boom")))
"#;
        assert_eq!(run(src).unwrap(), vec!["ok 7", "err boom"]);
    }

    /// Run a no-parameter `main` with a small step ceiling, so an infinite loop
    /// is caught quickly instead of hanging the test.
    fn run_capped(src: &str, limit: u64) -> Result<Vec<String>, RuntimeError> {
        let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
        let mut interp = Interpreter::new(module);
        interp.step_limit = limit;
        interp.call("main", vec![])?;
        Ok(interp.output)
    }

    #[test]
    fn early_return_exits_function_and_loop() {
        let src = r#"
fn first_even(xs: List(Int)) -> Int:
    for x in xs:
        if ((x % 2) == 0):
            return x
    (0 - 1)

fn main(console: Console):
    print(console, __render(first_even([1, 3, 8, 5])))
    print(console, __render(first_even([1, 3, 5])))
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "-1"]);
    }

    #[test]
    fn negative_int_patterns_match() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        -1 -> "neg one"
        0 -> "zero"
        _ -> "other"

fn main(console: Console):
    print(console, classify((-1)))
    print(console, classify(0))
    print(console, classify(3))
"#;
        assert_eq!(run(src).unwrap(), vec!["neg one", "zero", "other"]);
    }

    #[test]
    fn deep_recursion_is_a_graceful_error_not_a_crash() {
        // Runaway recursion must hit the depth limit and return an error rather
        // than overflowing the stack and aborting the host.
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    print(console, __render(rec(5000000)))
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("too deep"), "got: {}", e.message);
    }

    #[test]
    fn moderate_recursion_succeeds() {
        // Recursion well within the limit still works.
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    print(console, __render(rec(10000)))
"#;
        assert_eq!(run(src).unwrap(), vec!["0"]);
    }

    #[test]
    fn integer_overflow_wraps_like_the_wasm_backend() {
        // Multiplication that overflows i64 WRAPS (two's complement), identical to
        // the WASM backend's `i64.mul`, never panicking the host.
        let src = r#"
fn main(console: Console):
    let big = 9999999999
    print(console, __render((big * big)))
"#;
        assert_eq!(run(src).unwrap(), vec!["7766279611452241921"]);
    }

    #[test]
    fn negating_int_min_wraps_not_panics() {
        // -(i64::MIN) wraps back to i64::MIN (matching the WASM backend), never a
        // host panic.
        let src = r#"
fn main(console: Console):
    let lo = ((0 - 9223372036854775807) - 1)
    print(console, __render((-lo)))
"#;
        assert_eq!(run(src).unwrap(), vec!["-9223372036854775808"]);
    }

    #[test]
    fn runtime_errors_report_their_source_line() {
        // Division by zero happens on the third line.
        let src = "fn main(console: Console):\n    let a = 1\n    print(console, __render(a / 0))\n";
        let e = run(src).unwrap_err();
        assert!(e.message.contains("line 3"), "got: {}", e.message);
    }

    #[test]
    fn runtime_errors_name_the_innermost_function() {
        // The error must be attributed to `risky`, not the caller `main`.
        let src = r#"
fn risky(n: Int) -> Int:
    (n / 0)

fn main(console: Console):
    print(console, __render(risky(5)))
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("risky"), "got: {}", e.message);
    }

    #[test]
    fn assertion_failures_report_the_user_call_site_not_stdlib() {
        // Regression (M6): a failed `std/testing` assertion used to report the
        // `fail` line buried inside std/testing (always the same line, for every
        // failure). It must instead point at the user's call site — and at the
        // call STATEMENT's line even when an argument is a nested call that moves
        // the line cursor (`helper(1)` here is on line 3, the assertion on line 5).
        let src = "import testing\nfn helper(n: Int) -> Int:\n    n + 1\nfn main(console: Console):\n    testing.assert_int_eq(helper(1), 5)\n";
        let parsed = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let e = run_module(linked, ".", vec![]).unwrap_err();
        assert!(e.message.contains("`main`, line 5"), "got: {}", e.message);
        assert!(!e.message.contains("testing."), "should not name the stdlib frame: {}", e.message);
        assert!(e.message.contains("got 2, want 5"), "got: {}", e.message);
    }

    #[test]
    fn runaway_loop_is_bounded_not_hung() {
        let src = r#"
fn main() -> Int:
    var i = 0
    while true:
        i = (i + 1)
    i
"#;
        let e = run_capped(src, 100_000).unwrap_err();
        assert!(e.message.contains("step budget"), "got: {}", e.message);
    }

    #[test]
    fn normal_program_runs_within_budget() {
        // A finite loop well under the ceiling completes normally.
        let src = r#"
fn main() -> Int:
    var sum = 0
    var i = 0
    while (i < 1000):
        sum = (sum + i)
        i = (i + 1)
    sum
"#;
        assert!(run_capped(src, 100_000).is_ok());
    }

    #[test]
    fn dict_values_and_pairs_iterate() {
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "a", 10)
    d = dict.insert(d, "b", 20)
    var sum = 0
    for v in dict.values(d):
        sum = (sum + v)
    print(console, __render(sum))
    var report = ""
    for e in dict.pairs(d):
        let (k, v) = e
        report = ((((report + k) + "=") + __render(v)) + ";")
    print(console, report)
"#;
        assert_eq!(run(src).unwrap(), vec!["30", "a=10;b=20;"]);
    }

    #[test]
    fn dict_insert_get_has_keys_and_immutability() {
        let src = r#"
fn main(console: Console):
    let a = dict.insert(dict.new(), "x", 1)
    let b = dict.insert(a, "y", 2)
    let c = dict.insert(b, "x", 9)
    print(console, __render(dict.get_or(c, "x", 0)))
    print(console, __render(dict.get_or(c, "y", 0)))
    print(console, __render(dict.get_or(c, "z", 0)))
    print(console, __render(dict.length(c)))
    print(console, __render(dict.get_or(a, "x", 0)))
    print(console, __render(dict.contains_key(c, "y")))
    print(console, __render(list.length(dict.keys(c))))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["9", "2", "0", "2", "1", "true", "2"]
        );
    }

    #[test]
    fn sqrt_builtin_computes() {
        let src = r#"
fn main(console: Console):
    print(console, __render(math.sqrt(2.0)))
    print(console, __render(math.to_int(math.sqrt(144.0))))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["1.4142135623730951", "12"]
        );
    }

    #[test]
    fn string_slicing_and_search() {
        let src = r#"
fn main(console: Console):
    let s = "abcdef"
    print(console, string.substring(s, 1, 4))
    print(console, string.substring(s, 4, 100))
    print(console, string.substring(s, 3, 1))
    print(console, __render(string.find(s, "cd")))
    print(console, __render(string.find(s, "z")))
    print(console, __render(string.ends_with(s, "ef")))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["bcd", "ef", "", "2", "-1", "true"]
        );
    }

    #[test]
    fn substring_is_char_based_not_byte_based() {
        // A multi-byte char must count as one position, not its byte length.
        let src = r#"
fn main(console: Console):
    let s = "héllo"
    print(console, string.substring(s, 0, 2))
"#;
        assert_eq!(run(src).unwrap(), vec!["hé"]);
    }

    #[test]
    fn string_split_contains_replace() {
        let src = r#"
fn main(console: Console):
    let parts = string.split("a,b,c", ",")
    print(console, __render(list.length(parts)))
    print(console, list.at(parts, 1))
    print(console, string.replace("a,b,c", ",", "-"))
    print(console, __render(string.contains("hello", "ell")))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "b", "a-b-c", "true"]);
    }

    #[test]
    fn push_is_immutable_and_concat_joins() {
        let src = r#"
fn main(console: Console):
    let a = [1, 2]
    let b = list.push(a, 3)
    print(console, __render(list.length(a)))
    print(console, __render(list.length(b)))
    let c = list.concat(a, [9, 9])
    print(console, __render(list.at(c, 3)))
"#;
        assert_eq!(run(src).unwrap(), vec!["2", "3", "9"]);
    }

    #[test]
    fn closure_captures_environment() {
        let src = r#"
fn adder(n: Int) -> fn(Int) -> Int:
    fn(x: Int): (x + n)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let inc = adder(1)
    let plus100 = adder(100)
    print(console, __render(apply(inc, 5)))
    print(console, __render(apply(plus100, 5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["6", "105"]);
    }

    #[test]
    fn try_inside_lambda_returns_from_lambda() {
        // `?` inside a lambda short-circuits the lambda, not the outer function.
        let src = r#"
type Option:
    Some(a)
    None
fn run(f: fn(Option(Int)) -> Option(Int), o: Option(Int)) -> Option(Int):
    f(o)
fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> __render(n)
        None -> "none"
fn main(console: Console):
    let g = fn(o: Option(Int)):
        let n = o?
        Some(n + 1)
    print(console, render(run(g, Some(7))))
    print(console, render(run(g, None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "none"]);
    }

    #[test]
    fn record_update_does_not_mutate_the_original() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = Point(x: 10, y: ((p).y + 1), ..p)
    print(console, (__render((p).x) + __render((p).y)))
    print(console, (__render((q).x) + __render((q).y)))
"#;
        assert_eq!(run(src).unwrap(), vec!["12", "103"]);
    }

    #[test]
    fn record_field_access_runs() {
        let src = r#"
type Person:
    name: String
    age: Int

fn main(console: Console):
    let p = Person("witchy", 7)
    print(console, (((p).name + " is ") + __render((p).age)))
"#;
        assert_eq!(run(src).unwrap(), vec!["witchy is 7"]);
    }

    #[test]
    fn list_pattern_head_tail() {
        let src = r#"
fn len(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [_, ..tail] -> (1 + len(tail))

fn main(console: Console):
    print(console, __render(len([5, 6, 7, 8])))
"#;
        assert_eq!(run(src).unwrap(), vec!["4"]);
    }

    #[test]
    fn for_in_accumulates() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for n in [10, 20, 30]:
        total = (total + n)
    print(console, __render(total))
"#;
        assert_eq!(run(src).unwrap(), vec!["60"]);
    }

    #[test]
    fn try_option_short_circuits() {
        // `?` on `None` returns `None` from `first_word`; on `Some` it unwraps.
        let src = r#"
type Option:
    Some(a)
    None

fn head(o: Option(Int)) -> Option(Int):
    let n = (o)?
    Some((n + 100))

fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> __render(n)
        None -> "none"

fn main(console: Console):
    print(console, render(head(Some(1))))
    print(console, render(head(None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["101", "none"]);
    }

    #[test]
    fn conversions() {
        let src = r#"
fn main(console: Console):
    print(console, __render(math.to_float(7)))
    print(console, __render(math.to_int(3.9)))
    print(console, __render(string.to_int("42")))
"#;
        assert_eq!(run(src).unwrap(), vec!["7.0", "3", "42"]);
    }

    #[test]
    fn string_stdlib() {
        let src = r#"
fn main(console: Console):
    print(console, string.to_upper("witchy"))
    print(console, __render(string.length("hello")))
    print(console, string.trim("  hi  "))
    if string.starts_with("witchy", "wit"):
        print(console, "yes")
    else:
        print(console, "no")
"#;
        assert_eq!(run(src).unwrap(), vec!["WITCHY", "5", "hi", "yes"]);
    }

    #[test]
    fn while_loop_and_modulo() {
        let src = r#"
fn main(console: Console):
    var i = 1
    var total = 0
    while (i <= 5):
        total = (total + i)
        i = (i + 1)
    print(console, __render(total))
    print(console, __render((10 % 3)))
"#;
        assert_eq!(run(src).unwrap(), vec!["15", "1"]);
    }

    #[test]
    fn boolean_not_and_short_circuit() {
        let src = r#"
fn is_zero(n: Int) -> Bool:
    (n == 0)

fn main(console: Console):
    if (!is_zero(5)):
        print(console, "nonzero")
    else:
        print(console, "zero")
"#;
        assert_eq!(run(src).unwrap(), vec!["nonzero"]);
    }

    #[test]
    fn lists_length_and_index() {
        let src = r#"
fn main(console: Console):
    let xs = [10, 20, 30]
    print(console, __render(list.length(xs)))
    print(console, __render(list.at(xs, 1)))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "20"]);
    }

    #[test]
    fn let_bindings_are_immutable() {
        let src = r#"
fn main(console: Console):
    let x = 1
    x = 2
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("immutable"), "got: {}", e.message);
    }

    #[test]
    fn var_bindings_are_mutable() {
        let src = r#"
fn main(console: Console):
    var x = 1
    x = (x + 41)
    print(console, __render(x))
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    /// Hylo-style mutable value semantics: an `var` parameter mutates the
    /// caller's variable in place — easy mutability, no pointers.
    #[test]
    fn var_parameter_writes_back_to_caller() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main(console: Console):
    var x = 41
    bump(x)
    print(console, __render(x))
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    #[test]
    fn var_requires_a_mutable_variable() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main(console: Console):
    let x = 41
    bump(x)
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("var") || e.message.contains("immutable"),
            "got: {}",
            e.message
        );
    }
