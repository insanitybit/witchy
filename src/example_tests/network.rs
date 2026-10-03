use super::*;
use crate::{codegen, interpreter, parser, typeck};

    fn fixed_http_server(
        response: &'static str,
        requests: usize,
    ) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local address").port();
        let server = std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 256];
                while let Ok(read) = stream.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, server)
    }

    fn run_fixed_http_program(
        response: &'static str,
        build: impl FnOnce(u16) -> String,
    ) -> Vec<String> {
        let (port, server) = fixed_http_server(response, 1);
        let program = build(port);
        let module = parser::parse_module(&program).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let output = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().expect("server");
        output
    }

    /// BUG-381: `std/rights` must model `Net` as two independent axes:
    /// verbs (`Connect`/`Listen`) and transports (`Tcp`/`Udp`/`Uds`). Omitting
    /// an axis means "all values on that axis", matching the compiler's
    /// capability analyzer and the package/Coven authority gates.
    #[test]
    fn rights_net_axis_coverage_agrees_on_both_backends() {
        let src = r#"import rights

fn mark(v: Bool) -> String:
    if v:
        "T"
    else:
        "F"

fn main(console: Console):
    console.print(mark(rights.covers("Net[Connect]", "Net[Connect, Tcp]")))
    console.print(mark(rights.covers("Net[Tcp]", "Net[Connect, Tcp]")))
    console.print(mark(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    console.print(mark(rights.covers("Net[Connect]", "Net[Listen]")))
    console.print(mark(rights.covers("Net[Tcp]", "Net[Udp]")))
    console.print(mark(rights.covers("Net", "Net[Connect]")))
    console.print(mark(rights.covers("Net[Connect]", "Net")))
    console.print(mark(rights.covers("Dir[Read]", "Dir[Read, Write]")))
    console.print(mark(rights.covers("Dir[Read, Write]", "Dir[Read]")))
"#;
        let expected = ["T", "T", "F", "F", "F", "T", "F", "F", "T"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (SEC-043) The HTTP CRLF / header-injection validators trap LOUDLY and
    /// IDENTICALLY on both backends when a header value / request field carries a
    /// `\r`/`\n` (response/request splitting) or a header name is not an RFC 7230
    /// token — rather than emitting a corrupted, attacker-shaped wire message.
    /// A clean value passes on both backends.
    #[test]
    fn http_crlf_header_validators_trap_on_both_backends() {
        if !is_nightly() {
            return;
        }
        let prog = |call: &str| {
            format!("import http\n\nfn main(console: Console):\n    {call}\n    console.print(\"ok\")\n")
        };
        let server_prog = |call: &str| {
            format!("import server\n\nfn main(console: Console):\n    {call}\n    console.print(\"ok\")\n")
        };
        // A header VALUE with an embedded CRLF must error on both backends.
        let crlf_value = prog("http.check_header(\"x-test\", \"a\\r\\nInjected: 1\")");
        let linked = resolve_std_src(&crlf_value);
        assert!(
            interpreter::run_module(linked, ".", Vec::new()).is_err(),
            "interpreter must trap on a CRLF header value"
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(&crlf_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on a CRLF header value");

        // A header NAME with a space (not a token) must error on both backends.
        let bad_name = prog("http.check_header(\"bad name\", \"ok\")");
        assert!(
            interpreter::run_module(resolve_std_src(&bad_name), ".", Vec::new()).is_err(),
            "interpreter must trap on an invalid header name"
        );
        let bn = codegen::compile_module_binary(&resolve_std_src(&bad_name))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&bn).is_err(), "WASM must trap on an invalid header name");

        // A CR/LF in a request field (path/host/method) errors on both backends.
        let crlf_path = prog("http.check_field(\"request path\", \"/a\\nHost: evil\")");
        assert!(
            interpreter::run_module(resolve_std_src(&crlf_path), ".", Vec::new()).is_err(),
            "interpreter must trap on a CRLF path"
        );
        let cp = codegen::compile_module_binary(&resolve_std_src(&crlf_path))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&cp).is_err(), "WASM must trap on a CRLF path");

        // BUG-506: NUL and other non-CR/LF controls are also forbidden at this
        // raw HTTP rendering boundary.
        let nul_value = prog("let nul = string.from_code(0)\n    http.check_header(\"x-test\", \"a\" + nul + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&nul_value), ".", Vec::new()).is_err(),
            "interpreter must trap on a NUL header value"
        );
        let nv = codegen::compile_module_binary(&resolve_std_src(&nul_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&nv).is_err(), "WASM must trap on a NUL header value");

        let soh_path = prog("let soh = string.from_code(1)\n    http.check_request_field(\"request path\", \"/a\" + soh + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&soh_path), ".", Vec::new()).is_err(),
            "interpreter must trap on a SOH request field"
        );
        let sp = codegen::compile_module_binary(&resolve_std_src(&soh_path))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&sp).is_err(), "WASM must trap on a SOH request field");

        let del_value = prog("let del = string.from_code(127)\n    http.check_header(\"x-test\", \"a\" + del + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&del_value), ".", Vec::new()).is_err(),
            "interpreter must trap on a DEL header value"
        );
        let dv = codegen::compile_module_binary(&resolve_std_src(&del_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&dv).is_err(), "WASM must trap on a DEL header value");

        let nul_response = server_prog(
            "let nul = string.from_code(0)\n    let r = server.with_header(server.text(200, \"ok\"), \"x-test\", \"a\" + nul + \"b\")\n    let _wire = server.render(r)"
        );
        assert!(
            interpreter::run_module(resolve_std_src(&nul_response), ".", Vec::new()).is_err(),
            "interpreter must trap before rendering a response header with NUL"
        );
        let nr = codegen::compile_module_binary(&resolve_std_src(&nul_response))
            .expect_lowered("lowers");
        assert!(
            crate::run_wasm_bytes(&nr).is_err(),
            "WASM must trap before rendering a response header with NUL"
        );

        // A clean header + field passes on both backends (no false positives).
        let clean = prog("http.check_header(\"content-type\", \"application/json\")\n    http.check_header(\"x-tab\", \"a\\tb\")\n    http.check_request_field(\"request path\", \"/api/v1/users\")");
        assert_eq!(link_run(&clean), ["ok"], "interp accepts a clean header/path");
        assert_eq!(run_linked_on_wasm(&[("main", &clean)], "main"), ["ok"], "wasm accepts a clean header/path");
    }

    /// HTTP/query hardening — the cluster of stdlib `http`/`server` fixes must behave
    /// identically on both backends (parity is prime):
    ///   BUG-236/352  query/form values are percent- AND `+`-decoded (`%E2%82%AC` -> €).
    ///   BUG-375      path params and the handler-visible path are percent-decoded,
    ///                while a `%2F` stays inside one segment (no forged separator).
    ///   BUG-268      a nested router's own middleware layers are preserved.
    ///   BUG-390      a request with conflicting Content-Length is rejected (400).
    ///   BUG-203      an overflowing response status code parses to 0, never traps.
    ///   BUG-269      a `chunked` response body is de-chunked.
    ///   BUG-358      the renderer drops a handler-supplied framing header (no dup CL).
    #[test]
    fn http_server_hardening_agrees_on_both_backends() {
        let src = r#"import server
import http
import option
from http import Request, Response

fn hi(req: Request) -> Response:
    server.text(200, "id=" + server.param_or(req, "id", "") + " path=" + server.path(req))

fn tag(inner: fn(Request) -> Response) -> fn(Request) -> Response:
    fn(req: Request):
        match inner(req):
            Response(c, h, b) -> Response(c, h, "[wrapped]" + b)

fn main(console: Console):
    let req = Request("POST", "/x", [], [], [], "q=a%20b&x=1+2&k&e=%E2%82%AC")
    console.print("${server.form_body(req)}")
    let app = server.router().get("/users/:id", hi)
    console.print(http.body(server.handle(app, Request("GET", "/users/a%20b", [], [], [], ""))))
    console.print("${http.status(server.handle(app, Request("GET", "/users/a%2Fb", [], [], [], "")))}")
    let sub = server.router().get("/inner", hi).layer(tag)
    let nested = server.router().nest("/api", sub)
    console.print(http.body(server.handle(nested, Request("GET", "/api/inner", [], [], [], ""))))
    match server.parse_request_response("POST /x HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nabc"):
        Ok(_r) -> console.print("PARSED")
        Err(resp) -> console.print("rejected " + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n"):
        Ok(_r) -> console.print("chunked-request=parsed")
        Err(resp) -> console.print("chunked-request=" + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\nbody"):
        Ok(_r) -> console.print("gzip-request=parsed")
        Err(resp) -> console.print("gzip-request=" + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\nbody"):
        Ok(_r) -> console.print("mixed-framing=parsed")
        Err(resp) -> console.print("mixed-framing=" + "${http.status(resp)}")
    console.print("status=" + "${http.status(http.parse_response("HTTP/1.1 999999999999999999999999 X\r\n\r\nb"))}")
    console.print("chunked=" + http.body(http.parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")))
    console.print("unicode-chunk=" + http.body(http.parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\né\r\n0\r\n\r\n")))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\né\r\n0\r\n\r\n"):
        Ok(resp) -> console.print("unicode-strict=" + http.body(resp))
        Err(e) -> console.print("unicode-strict=" + http.http_error_message(e))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nX\r\nhello\r\n0\r\n\r\n"):
        Ok(_) -> console.print("bad-size=parsed")
        Err(e) -> console.print("bad-size=" + http.http_error_message(e))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhe"):
        Ok(_) -> console.print("truncated=parsed")
        Err(e) -> console.print("truncated=" + http.http_error_message(e))
    let r1 = server.with_header(server.text(200, "hi"), "content-length", "999")
    console.print("cl=" + "${server.render(r1).to_lower().count("content-length")}")
    console.print("${http.is_framing_header("Content-Length")}")
"#;
        let expected = vec![
            "[(q, a b), (x, 1 2), (k, ), (e, €)]".to_string(),
            "id=a b path=/users/a b".to_string(),
            "200".to_string(),
            "[wrapped]id= path=/api/inner".to_string(),
            "rejected 400".to_string(),
            "chunked-request=400".to_string(),
            "gzip-request=400".to_string(),
            "mixed-framing=400".to_string(),
            "status=0".to_string(),
            "chunked=hello world".to_string(),
            "unicode-chunk=é".to_string(),
            "unicode-strict=é".to_string(),
            "bad-size=chunked response has invalid chunk size `X`".to_string(),
            "truncated=chunked response ended before the declared chunk size".to_string(),
            "cl=1".to_string(),
            "true".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// BUG-438: malformed request lines are rejected before reaching handlers.
    /// BUG-432: paths are normalized (collapsed slashes, no trailing slash).
    #[test]
    fn server_request_line_validation_and_path_normalization() {
        let src = r#"import server
import http
from http import Request, Response

fn hi(req: Request) -> Response:
    server.text(200, "path=" + server.path(req))

fn main(console: Console):
    // BUG-438: malformed request lines rejected
    match server.parse_request("GET\r\n\r\n"):
        Ok(_r) -> console.print("bad: no target")
        Err(e) -> console.print(server.request_parse_error_message(e))
    match server.parse_request("GET /\r\n\r\n"):
        Ok(_r) -> console.print("bad: no version")
        Err(e) -> console.print(server.request_parse_error_message(e))
    match server.parse_request("\r\n\r\n"):
        Ok(_r) -> console.print("bad: empty line")
        Err(e) -> console.print(server.request_parse_error_message(e))
    // Valid request line succeeds
    match server.parse_request("GET / HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("ok path=" + server.path(req))
        Err(_e) -> console.print("bad: valid rejected")
    // BUG-432: path normalization
    match server.parse_request("GET //api//coven/index HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("norm=" + server.path(req))
        Err(_e) -> console.print("bad: norm rejected")
    match server.parse_request("GET /api/coven/index/ HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("trail=" + server.path(req))
        Err(_e) -> console.print("bad: trail rejected")
    // Router normalizes paths — double slashes and trailing slashes match
    let app = server.router().get("/api/items", hi)
    console.print(http.body(server.handle(app, Request("GET", "/api/items", [], [], [], ""))))
    console.print(http.body(server.handle(app, Request("GET", "/api//items", [], [], [], ""))))
    console.print(http.body(server.handle(app, Request("GET", "/api/items/", [], [], [], ""))))
"#;
        let expected = vec![
            "malformed request line".to_string(),
            "malformed request line".to_string(),
            "malformed request line".to_string(),
            "ok path=/".to_string(),
            "norm=/api/coven/index".to_string(),
            "trail=/api/coven/index".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-234) A non-http(s) URL scheme (`ftp:`/`file:`/`gopher:`) is REJECTED with
    /// an Err rather than silently dialed as plaintext HTTP to the named host — the
    /// http client speaks only HTTP/1.1. Same rejection on both backends.
    #[test]
    fn http_rejects_non_http_schemes_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};

        let src = "import http\n\n\
                   fn main(net: Net, console: Console):\n\
                   \x20   let fetch = net.fetch(\"\")\n\
                   \x20   for u in [\"ftp://h/x\", \"file:///etc/passwd\", \"gopher://h/1\"]:\n\
                   \x20       match http.try_get(fetch, u):\n\
                   \x20           Ok(_r) -> console.print(\"OK\")\n\
                   \x20           Err(_e) -> console.print(\"rejected\")\n";
        let want = ["rejected", "rejected", "rejected"];
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp"),
            want,
            "interpreter"
        );
        let checked = witchy::resolve_std_only_checked(src).expect("checked HTTP scheme source");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical path lowers HTTP scheme rejection");
        assert!(matches!(codegen::compile_module_binary(&linked),
            codegen::LoweringOutcome::Unsupported(_)), "raw HTTP Fetch encoding must remain unavailable");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime.spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                quiet: true,
                net_allow: Some(Vec::new()),
                net_connect: true,
                ..Default::default()
            },
            64,
        ).expect("spawn with an empty Net allow-list");
        actor.run().expect("compiled scheme rejection");
        assert_eq!(actor.output(), want, "wasm");
    }

    /// (BUG-364 / BUG-255) The request-line validator rejects a SPACE (it would split
    /// the request line into extra tokens — request smuggling), and the response
    /// renderer rejects a status code outside 100..599. Both trap LOUDLY and
    /// identically on both backends rather than emit a malformed message.
    #[test]
    fn http_request_line_and_status_validation_trap_on_both_backends() {
        let cases = [
            "import http\n\nfn main(console: Console):\n    http.check_request_field(\"request path\", \"/a b\")\n    console.print(\"x\")\n",
            "import server\n\nfn main(console: Console):\n    console.print(server.render(server.status_only(700)))\n",
        ];
        for src in cases {
            assert!(
                interpreter::run_module(resolve_std_src(src), ".", Vec::new()).is_err(),
                "interpreter must trap: {src}"
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect_lowered("lowers");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "wasm must trap: {src}");
        }
    }

    /// (BUG-489) The blessed NetPolicy constructors reject impossible ports at
    /// the std boundary. Raw `NetPolicy(...)` remains a separate surface tracked
    /// by BUG-484, but `Net.tcp`/`Net.cidr` should only build meaningful policy
    /// values.
    #[test]
    fn net_policy_constructors_reject_out_of_range_ports_on_both_backends() {
        let ok = "fn main(console: Console):\n    console.print(Net.tcp(\"example.com\", 0).pattern)\n    console.print(Net.cidr(\"10.0.0.0/8\", 65535).pattern)\n";
        let expected = ["example.com:0", "10.0.0.0/8:65535"];
        assert_eq!(link_run(ok), expected, "interp: edge ports");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "wasm: edge ports");

        for call in [
            "Net.tcp(\"example.com\", -1)",
            "Net.tcp(\"example.com\", 70000)",
            "Net.cidr(\"10.0.0.0/8\", -1)",
            "Net.cidr(\"10.0.0.0/8\", 70000)",
        ] {
            let src = format!("fn main(console: Console):\n    let p = {call}\n    console.print(p.pattern)\n");
            let linked = resolve_std_src(&src);
            typeck::check(&linked).expect("typecheck");
            let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must reject out-of-range NetPolicy port")
                .to_string();
            assert!(interp_err.contains("policy: net port must be in 0..65535"), "{call}: {interp_err}");

            let wasm = codegen::compile_module_binary(&linked)

                .expect_lowered("out-of-range NetPolicy program should lower");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must reject out-of-range NetPolicy port")
                .to_string();
            assert!(wasm_err.contains("policy: net port must be in 0..65535"), "{call}: {wasm_err}");
        }
    }

    #[test]
    fn fetch_capability_raw_abi_agrees_across_backends() {
        use crate::runtime::{Capabilities, Runtime};

        let (port, server) = fixed_http_server(
            "HTTP/1.1 200 OK\r\ncontent-length: 11\r\nconnection: close\r\n\r\nfetch works",
            2,
        );
        let origin = format!("http://127.0.0.1:{port}");

        let src = format!(
            "fn main(console: Console, fetch: Fetch):\n    let narrowed = fetch.only(\"{origin}\")\n    let response = narrowed.send_raw(\"GET\", \"{origin}/hello\", \"\", \"\")\n    console.print(response)\n"
        );
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interpreted =
            interpreter::run_module_fetch(linked.clone(), ".", vec![origin.clone()])
                .expect("interpreter");
        let checked = witchy::resolve_std_only_checked(&src).expect("checked Fetch source");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical path lowers Fetch and narrowing");
        assert!(matches!(codegen::compile_module_binary(&linked),
            codegen::LoweringOutcome::Unsupported(_)), "raw Fetch encoding must remain unavailable");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn_admitted(
                &artifact,
                Capabilities {
                    print: true,
                    quiet: true,
                    fetch_grants: vec![vec![origin]],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        let compiled = actor.output();
        assert_eq!(compiled, interpreted, "Fetch response must agree");
        assert!(
            compiled
                .first()
                .is_some_and(|response| response.contains("200") && response.contains("fetch works")),
            "Fetch response must preserve status and body: {compiled:?}"
        );
        server.join().expect("server thread");
    }

    #[test]
    fn fetch_derived_from_net_compiles_without_a_fetch_root_grant() {
        use crate::runtime::{Capabilities, Runtime};

        let (port, server) = fixed_http_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nderived",
            2,
        );
        let address = format!("127.0.0.1:{port}");
        let origin = format!("http://{address}");
        let source = format!(
            "fn main(console: Console, net: Net[Connect, Tcp]):\n    \
             let fetch = net.fetch(\"{origin}\")\n    \
             console.print(fetch.send_raw(\"GET\", \"{origin}/hello\", \"\", \"\"))\n"
        );
        let module = parser::parse_module(&source).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let net_allow = vec![address.to_string()];
        let interpreted =
            interpreter::run_module(linked.clone(), ".", net_allow.clone()).expect("interpreter");
        let checked = witchy::resolve_std_only_checked(&source).expect("checked Net-to-Fetch source");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical path lowers Net-to-Fetch derivation");
        assert!(matches!(codegen::compile_module_binary(&linked),
            codegen::LoweringOutcome::Unsupported(_)), "raw Net-to-Fetch encoding must remain unavailable");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn_admitted(
                &artifact,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(net_allow),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn without a Fetch root grant");
        actor.run().expect("run");
        assert_eq!(actor.output(), interpreted);
        assert!(actor.output()[0].contains("derived"));
        server.join().expect("server");
    }

    /// Run `src` as unoptimized admitted Wasm under `caps`; a refused launch
    /// or a trap is `Err` with its root cause.
    fn run_admitted_net_with(src: &str, caps: crate::runtime::Capabilities) -> Result<Vec<String>, String> {
        struct ResetOptimization;
        impl Drop for ResetOptimization {
            fn drop(&mut self) { crate::opt::set_for_tests(None); }
        }
        crate::opt::set_for_tests(Some(crate::opt::OptSet::none()));
        let _reset = ResetOptimization;
        let checked = witchy::resolve_std_only_checked(src).expect("check Net program");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit Net program");
        let mut rt = crate::runtime::Runtime::batch().expect("runtime");
        let mut vm = rt.spawn_admitted(&artifact, caps, 64).map_err(|error| error.root_cause().to_string())?;
        vm.run().map_err(|error| error.root_cause().to_string())?;
        Ok(vm.output())
    }

    /// `run_admitted_net_with` under a Connect and Listen grant for `net_allow`.
    fn run_admitted_net(src: &str, net_allow: &[String]) -> Result<Vec<String>, String> {
        run_admitted_net_with(src, crate::runtime::Capabilities {
            print: true, quiet: true, net_allow: Some(net_allow.to_vec()), net_connect: true, net_listen: true,
            ..Default::default()
        })
    }

    /// The `CapError` classes a Net operation answers, rendered independently
    /// of `show`.
    const SHOW_NET: &str = "fn shown(r: Result(String, CapError)) -> String:\n    match r:\n        Ok(text) -> \"ok: \" + text\n        Err(Denied(m)) -> \"denied: \" + m\n        Err(AlreadyExists(m)) -> \"exists: \" + m\n        Err(Io(m)) -> \"io: \" + m\n        Err(_) -> \"other\"\n\nfn sent(r: Result((), CapError)) -> String:\n    match r:\n        Ok(_) -> \"sent\"\n        Err(e) -> \"send: \" + e.message()\n\n";

    /// RFC-0126 rule 3 for Net, as admitted Wasm over loopback. An exchange
    /// with an allowlisted echo server succeeds. An address outside the grant
    /// is `Denied`, decided before any socket or DNS activity: `localhost` is
    /// never resolved. A refused connection is `Io` with a fixed reason, and
    /// binding an address already in use is `AlreadyExists`. No message names
    /// more than the address the program supplied.
    #[test]
    fn net_operations_answer_cap_errors() {
        use std::io::{BufRead, Write};
        let echo = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = echo.local_addr().expect("local address").port();
        let busy = std::net::TcpListener::bind("127.0.0.1:0").expect("bind busy");
        let busy_port = busy.local_addr().expect("busy address").port();
        let closed = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
            probe.local_addr().expect("probe address").port()
        };
        let server = std::thread::spawn(move || {
            let (stream, _) = echo.accept().expect("accept");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            reader.get_mut().write_all(format!("echo: {}\n", line.trim_end()).as_bytes()).expect("write");
        });
        let src = format!(r#"{SHOW_NET}fn listened(r: Result(Listener, CapError)) -> String:
    match r:
        Ok(_) -> "listening"
        Err(e) -> shown(Err(e))

fn main(console: Console, net: Net):
    match net.connect("127.0.0.1:{port}"):
        Ok(sock) ->
            console.print(sent(sock.send_line("hello")))
            console.print(shown(sock.recv_line()))
            console.print(shown(sock.recv_all()))
            sock.close()
        Err(e) -> console.print(shown(Err(e)))
    match net.connect("127.0.0.1:{closed}"):
        Ok(_) -> console.print("connected")
        Err(e) -> console.print(shown(Err(e)))
    match net.connect("10.9.9.9:80"):
        Ok(_) -> console.print("connected")
        Err(e) -> console.print(shown(Err(e)))
    match net.resolve("localhost"):
        Ok(_) -> console.print("resolved")
        Err(e) -> console.print(shown(Err(e)))
    match net.resolve("127.0.0.1"):
        Ok(ips) ->
            for ip in ips:
                console.print("ip: " + ip)
        Err(e) -> console.print(shown(Err(e)))
    console.print(listened(net.listen("127.0.0.1:{busy_port}")))
    console.print(listened(net.listen("127.0.0.1:1")))
"#);
        let allow = vec![format!("127.0.0.1:{port}"), format!("127.0.0.1:{closed}"), format!("127.0.0.1:{busy_port}")];
        assert_eq!(run_admitted_net(&src, &allow).expect("run"), vec![
            "sent".to_string(),
            "ok: echo: hello".to_string(),
            "ok: ".to_string(),
            format!("io: `127.0.0.1:{closed}`: connection refused"),
            "denied: `10.9.9.9:80` is not permitted by this Net capability".to_string(),
            "denied: `localhost` is not permitted by this Net capability".to_string(),
            "ip: 127.0.0.1".to_string(),
            format!("exists: `127.0.0.1:{busy_port}` is already in use"),
            "denied: `127.0.0.1:1` is not permitted by this Net capability".to_string(),
        ]);
        server.join().expect("echo server");
        drop(busy);
    }

    /// RFC-0020: a program granted `[::1]:80` that `net.deny(Net.private())`
    /// cannot dial `[::1]:80`: the loopback is CIDR-matched by the deny, so
    /// the dial is `Denied`.
    #[test]
    fn net_deny_private_blocks_internal_ipv6() {
        let src = format!(r#"{SHOW_NET}fn main(console: Console, net: Net):
    match net.deny(Net.private()).connect("[::1]:80"):
        Ok(_) -> console.print("connected")
        Err(e) -> console.print(shown(Err(e)))
"#);
        assert_eq!(run_admitted_net(&src, &["[::1]:80".to_string()]).expect("run"),
            vec!["denied: `[::1]:80` is not permitted by this Net capability".to_string()]);
    }

    /// Net verbs are enforced at the grant: a module that listens traps under a
    /// connect-only grant, and a Net program cannot launch with no grant.
    #[test]
    fn net_rights_enforced_by_the_grant() {
        use crate::runtime::Capabilities;
        let listener = "fn main(console: Console, net: Net):\n    match net.listen(\"127.0.0.1:0\"):\n        Ok(_) -> console.print(\"listening\")\n        Err(_) -> console.print(\"refused\")\n";
        let error = run_admitted_net_with(listener, Capabilities {
            print: true, quiet: true, net_allow: Some(vec!["127.0.0.1:0".into()]), net_connect: true, net_listen: false,
            ..Default::default()
        }).expect_err("listen must not run under a connect-only grant");
        assert!(error.contains("Listen right"), "{error}");
        let client = "fn main(console: Console, net: Net):\n    match net.connect(\"127.0.0.1:1\"):\n        Ok(_) -> console.print(\"x\")\n        Err(_) -> console.print(\"y\")\n";
        assert!(run_admitted_net_with(client, Capabilities { print: true, quiet: true, ..Default::default() }).is_err(),
            "a Net program must not run without a Net grant");
    }

    /// RFC-0126 rule 3: `net.only(policy)` is `Result(Net, CapError)`. A
    /// policy within the receiver's set is `Ok`, and the narrowed `Net` dials
    /// only what it admits; one outside it is `Denied`, decided from the
    /// grant alone (`10.0.0.7` is never dialed or resolved). A union narrows
    /// to the whole set, and `deny` cannot be undone by a later `only`.
    #[test]
    fn net_only_answers_denied_outside_the_receiver_set() {
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().expect("local address").port();
        let echo = std::thread::spawn(move || {
            let (stream, _) = server.accept().expect("accept");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            reader.get_mut().write_all(format!("echo: {}\n", line.trim_end()).as_bytes()).expect("write");
        });
        let src = format!(r#"
fn verdict(r: Result(Net, CapError)) -> String:
    match r:
        Ok(_) -> "ok"
        Err(Denied(m)) -> "denied: " + m
        Err(_) -> "other"

fn main(console: Console, net: Net):
    console.print(verdict(net.only(Net.tcp("127.0.0.1", {port}))))
    console.print(verdict(net.only(Net.tcp("10.0.0.7", 6379))))
    console.print(verdict(net.only(Net.union(Net.tcp("10.0.0.5", 6379), Net.tcp("10.0.0.6", 6379)))))
    console.print(verdict(net.deny(Net.tcp("10.0.0.5", 6379)).only(Net.tcp("10.0.0.5", 6379))))
    match net.only(Net.tcp("127.0.0.1", {port})):
        Ok(narrowed) ->
            console.print(verdict(narrowed.only(Net.tcp("10.0.0.5", 6379))))
            match narrowed.connect("127.0.0.1:{port}"):
                Ok(sock) ->
                    let _ = sock.send_bytes("hello\n")
                    console.print(sock.recv_line() ?? "no line")
                    sock.close()
                Err(_) -> console.print("unexpected")
        Err(_) -> console.print("unexpected")
"#);
        let allow = vec![format!("127.0.0.1:{port}"), "10.0.0.5:6379".to_string(), "10.0.0.6:6379".to_string()];
        assert_eq!(run_admitted_net(&src, &allow).expect("run"), vec![
            "ok".to_string(),
            "denied: `10.0.0.7:6379` is not covered by this Net capability".to_string(),
            "ok".to_string(),
            "denied: `10.0.0.5:6379` is not covered by this Net capability".to_string(),
            "denied: `10.0.0.5:6379` is not covered by this Net capability".to_string(),
            "echo: hello".to_string(),
        ]);
        echo.join().expect("echo server");
    }

    /// Attenuation is real: after narrowing to one address, a sibling that
    /// was in the original grant is no longer reachable.
    #[test]
    fn net_capability_cannot_escalate() {
        let attenuated = r#"
fn main(console: Console, net: Net):
    match net.only(Net.tcp("a.test", 80)):
        Ok(narrow) ->
            match narrow.connect("b.test:80"):
                Ok(_) -> console.print("connected")
                Err(e) -> console.print(e.message())
        Err(_) -> console.print("unexpected")
"#;
        assert_eq!(run_admitted_net(attenuated, &["a.test:80".to_string(), "b.test:80".to_string()]).expect("run"),
            vec!["`b.test:80` is not permitted by this Net capability".to_string()]);
    }

