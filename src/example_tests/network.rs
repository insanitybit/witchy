use super::*;
use crate::{codegen, interpreter, parser, typeck};

    fn fixed_http_server(response: &'static str) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local address").port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
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
        let (port, server) = fixed_http_server(response);
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

    #[test]
    fn rfc0054_server_parse_request_uses_typed_error_and_response_bridge() {
        let src = r#"import http
import server
import show

fn classify(e: server.RequestParseError) -> String:
    match e:
        server.UnsupportedTransferEncoding -> "transfer"
        server.ConflictingContentLength -> "length"
        server.BadRequestLine -> "badline"

fn via_string(raw: String) -> Result(http.Request, String):
    let req = server.parse_request(raw)?
    Ok(req)

fn main(console: Console):
    let conflict = "POST /x HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nabc"
    let chunked = "POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n"
    match server.parse_request(conflict):
        Ok(_r) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(server.request_parse_error_message(e))
            console.print(show.render(e))
    match via_string(chunked):
        Ok(_r) -> console.print("bad")
        Err(e) -> console.print(e)
    match server.parse_request_response(chunked):
        Ok(_r) -> console.print("bad")
        Err(resp) -> console.print("response:${http.status(resp)}:" + http.body(resp))
"#;
        let expected = [
            "length",
            "conflicting Content-Length headers",
            "conflicting Content-Length headers",
            "unsupported Transfer-Encoding",
            "response:400:unsupported Transfer-Encoding",
        ];
        assert_eq!(link_run(src), expected, "interp: server.parse_request typed error");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: server.parse_request typed error",
        );
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

    /// (SEC-043) `has_crlf` agrees on both backends for a control-bearing vs a
    /// clean value — the primitive the CRLF validators are built on.
    #[test]
    fn http_has_crlf_agrees_on_both_backends() {
        let prog = |v: &str| {
            format!(
                "import http\n\nfn main(console: Console):\n    console.print(\"${{http.has_crlf(\"{v}\")}}\")\n"
            )
        };
        for (value, want) in [("a\\r\\nb", "true"), ("plain", "false"), ("tab\\ttab", "false")] {
            let src = prog(value);
            assert_eq!(link_run(&src), [want], "interp has_crlf({value})");
            assert_eq!(run_linked_on_wasm(&[("main", &src)], "main"), [want], "wasm has_crlf({value})");
        }
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
    match server.parse_request("GET / HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("root=" + server.path(req))
        Err(_e) -> console.print("bad: root rejected")
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
            "root=/".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Server accessors distinguish a present empty value from an absent one.
    /// BUG-464: the primary API returns Option; callers that want the old sentinel
    /// behavior must opt in with the `_or` helpers.
    #[test]
    fn server_accessors_return_option_for_absence_on_both_backends() {
        let src = r#"import server
from http import Request

fn show(console: Console, label: String, value: Option(String)):
    match value:
        Some(v) -> console.print(label + "=Some(" + v + ")")
        None -> console.print(label + "=None")

fn main(console: Console):
    let req = Request("POST", "/x", [("id", "")], [("code", ""), ("state", "ready")], [], "a=&b=2")
    show(console, "param-empty", server.param(req, "id"))
    show(console, "param-missing", server.param(req, "missing"))
    show(console, "query-empty", server.query(req, "code"))
    show(console, "query-present", server.query(req, "state"))
    show(console, "query-missing", server.query(req, "missing"))
    show(console, "form-empty", server.form_field(req, "a"))
    show(console, "form-present", server.form_field(req, "b"))
    show(console, "form-missing", server.form_field(req, "missing"))
    console.print("param_or=" + server.param_or(req, "missing", "fallback"))
    console.print("query_or=" + server.query_or(req, "missing", "fallback"))
    console.print("form_field_or=" + server.form_field_or(req, "missing", "fallback"))
"#;
        let expected = vec![
            "param-empty=Some()".to_string(),
            "param-missing=None".to_string(),
            "query-empty=Some()".to_string(),
            "query-present=Some(ready)".to_string(),
            "query-missing=None".to_string(),
            "form-empty=Some()".to_string(),
            "form-present=Some(2)".to_string(),
            "form-missing=None".to_string(),
            "param_or=fallback".to_string(),
            "query_or=fallback".to_string(),
            "form_field_or=fallback".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-234) A non-http(s) URL scheme (`ftp:`/`file:`/`gopher:`) is REJECTED with
    /// an Err rather than silently dialed as plaintext HTTP to the named host — the
    /// http client speaks only HTTP/1.1. Same rejection on both backends.
    #[test]
    fn http_rejects_non_http_schemes_on_both_backends() {
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
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            want,
            "interpreter"
        );
        assert_eq!(run_linked_on_wasm_net(&[("main", src)], "main", &[]), want, "wasm");
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

    /// RFC-0011: `std/policy` builds a typed `NetPolicy` (`Net.tcp(host, port)`)
    /// instead of a hand-written string, and `net.only(policy)` narrows the `Net` to it.
    /// The typed policy carries the same `host:port` pattern the host enforces, so both
    /// backends agree. The grant must admit the pattern.
    #[test]
    fn net_tcp_policy_narrows_on_both_backends() {
        let src = "fn main(net: Net, console: Console):\n    let db = net.only(Net.tcp(\"10.0.0.5\", 6379))\n    console.print(\"confined\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["confined".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["10.0.0.5:6379".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379"]),
            expected,
            "wasm",
        );
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
    fn net_private_denies_internal_addresses_on_both_backends() {
        // RFC-0020: `net.deny(Net.private())` is the one-line SSRF/rebinding
        // defense — a connect to a private IP (here loopback) is refused at the
        // capability layer, identically on both backends. `connect` aborts on a
        // denied address, so a successful run means the deny held.
        let src = "fn main(net: Net, console: Console):\n    let safe = net.deny(Net.private())\n    console.print(\"denied private ranges\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["denied private ranges".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["8.8.8.8:443".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["8.8.8.8:443"]),
            expected,
            "wasm",
        );
    }

    /// RFC-0011: `net.only(policy)` is the typed refinement verb — it narrows a `Net`'s
    /// address set to a `NetPolicy` built by `policy`. It narrows identically on both
    /// backends. (The raw-string form survives only as a `--net`/config grant, not a
    /// language builtin — see `retired_restrict_builtin_is_rejected`.)
    #[test]
    fn net_only_refinement_verb_backends_agree() {
        let src = "fn main(net: Net, console: Console):\n    let m = net.only(Net.tcp(\"10.0.0.5\", 6379))\n    console.print(\"only\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["only".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["10.0.0.5:6379".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379"]),
            expected,
            "wasm",
        );
    }

    /// RFC-0020 step 1: the IPv6 SSRF/rebinding defense, end to end. A program granted `[::1]:80`
    /// that `net.deny(Net.private())` CANNOT connect to `[::1]:80` — the loopback is now
    /// CIDR-matched by the deny (before this, `Net.private()`'s IPv6 ranges only ever
    /// exact-matched, so an internal IPv6 slipped through). Refused identically on both backends
    /// (the allow-list check is the shared `net_allows`).
    #[test]
    fn net_deny_private_blocks_internal_ipv6_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "\
                   fn main(console: Console, net: Net):\n\
                   \x20   let safe = net.deny(Net.private())\n\
                   \x20   let s = safe.connect(\"[::1]:80\")\n\
                   \x20   s.send_line(\"x\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        // Interpreter: granted the loopback, then it denies the private ranges.
        assert!(
            interpreter::run_module(linked.clone(), ".", vec!["[::1]:80".into()]).is_err(),
            "interp must refuse an internal IPv6 connect after net.deny(private())"
        );
        // Compiled: same grant, same refusal.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec!["[::1]:80".to_string()]),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled must refuse an internal IPv6 connect after net.deny(private())"
        );
    }

    /// RFC-0011: `Net.union(a, b)` builds a multi-endpoint `NetPolicy`, and
    /// `net.only(union(...))` narrows to the WHOLE set — so a further refinement to EITHER
    /// endpoint still succeeds (both are admitted). On both backends.
    #[test]
    fn net_only_union_admits_each_endpoint_backends_agree() {
        let src = "fn main(net: Net, console: Console):\n    let pair = net.only(Net.union(Net.tcp(\"10.0.0.5\", 6379), Net.tcp(\"10.0.0.6\", 6379)))\n    let a = pair.only(Net.tcp(\"10.0.0.5\", 6379))\n    let b = pair.only(Net.tcp(\"10.0.0.6\", 6379))\n    console.print(\"both\")\n";
        let expected = vec!["both".to_string()];
        assert_eq!(link_run_net(src, &["10.0.0.5:6379", "10.0.0.6:6379"]), expected, "interp");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379", "10.0.0.6:6379"]),
            expected,
            "wasm",
        );
    }

    /// The full `std/http` client runs in the WASM backend: a real GET against
    /// a local server returns the same status and body on both backends.
    #[test]
    fn std_http_client_runs_in_the_wasm_backend() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // One request per backend run: consume the request head, reply 200.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).expect("read");
                    if line == "\r\n" || line == "\n" || line.is_empty() {
                        break;
                    }
                }
                reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello")
                    .expect("write");
            }
        });

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    let target = \"http://127.0.0.1:{port}/greet\"\n    let res = http.get(net.fetch(http.origin(target)), target)\n    console.print(f\"{{http.status(res)}} {{http.body(res)}}\")\n"
        );
        let want = vec!["200 hello".to_string()];
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");
    }

    #[test]
    fn fetch_capability_raw_abi_agrees_across_backends() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut buf = [0u8; 512];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buf).expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..read]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\nconnection: close\r\n\r\nfetch works",
                    )
                    .expect("write response");
            }
        });

        let src = format!(
            "fn main(console: Console, fetch: Fetch):\n    let narrowed = fetch.only(\"{origin}\")\n    let response = narrowed.send_raw(\"GET\", \"{origin}/hello\", \"\", \"\")\n    console.print(response)\n"
        );
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interpreted =
            interpreter::run_module_fetch(linked.clone(), ".", vec![origin.clone()])
                .expect("interpreter");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers Fetch");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &bytes,
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
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let origin = format!("http://{address}");
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).expect("read");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nderived",
                    )
                    .expect("write");
            }
        });
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
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers Net-to-Fetch derivation");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &bytes,
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

    /// `http.try_get` is fallible: a dial to an ALLOWLISTED-but-closed port
    /// yields `Err(...)` rather than trapping — on BOTH backends. This is the
    /// primitive that lets a proxy answer 502 for a down upstream instead of
    /// aborting the VM. (A capability violation still traps; here the address is
    /// permitted, so only the transient dial failure path is exercised.) The
    /// closed port comes from binding then dropping a loopback listener, so the
    /// address is well-formed and reachable-to-refuse, not merely unroutable.
    #[test]
    fn http_try_get_returns_err_on_closed_port() {
        use crate::runtime::{Capabilities, Runtime};
        // Bind to grab a free loopback port, then drop the listener so a connect
        // is refused fast (RST) rather than hanging.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        drop(listener);

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    let target = \"http://127.0.0.1:{port}/\"\n    match http.try_get(net.fetch(http.origin(target)), target):\n        Ok(_) -> console.print(\"ok\")\n        Err(_) -> console.print(\"err\")\n"
        );
        let want = vec!["err".to_string()];
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter must report Err for a closed-port dial"
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree: Err, not a trap");
    }

    /// The Net family compiles to capability-gated host imports and agrees with
    /// the interpreter: a client connects to an allowlisted loopback server,
    /// exchanges a line on both backends, and a non-allowlisted address FAILS
    /// on both.
    #[test]
    fn net_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", server.local_addr().unwrap().port());
        // One echo exchange per backend run.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read");
                let reply = format!("echo: {}\n", line.trim_end());
                reader.get_mut().write_all(reply.as_bytes()).expect("write");
            }
        });

        let src = format!(
            "fn main(console: Console, net: Net):\n    let sock = net.connect(\"{addr}\")\n    sock.send_line(\"hello\")\n    console.print(sock.recv_line())\n    sock.close()\n"
        );
        let want = vec!["echo: hello".to_string()];
        assert_eq!(
            interpreter::run_with(&src, ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let module = parser::parse_module(&src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr.clone()]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");

        // A non-allowlisted address fails on BOTH backends.
        let bad = "fn main(console: Console, net: Net):\n    let sock = net.connect(\"127.0.0.1:1\")\n    console.print(\"connected\")\n";
        assert!(
            interpreter::run_with(bad, ".", vec![addr.clone()]).is_err(),
            "interp must reject a non-allowlisted address"
        );
        let m = parser::parse_module(bad).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut a = rt
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on a non-allowlisted address");
    }

    /// Net verbs are enforced at the GRANT: a listening module cannot
    /// instantiate under a connect-only grant, and any net import fails with no
    /// grant at all.
    #[test]
    fn net_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let listener_src = "fn main(console: Console, net: Net):\n    let l = net.listen(\"127.0.0.1:39999\")\n    console.print(\"listening\")\n";
        let m = parser::parse_module(listener_src).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities {
                print: true,
                quiet: true,
                net_allow: Some(vec!["127.0.0.1:39999".into()]),
                net_connect: true,
                net_listen: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "listen import must not instantiate under connect-only");
        let client = "fn main(console: Console, net: Net):\n    let s = net.connect(\"127.0.0.1:1\")\n    console.print(\"x\")\n";
        let m = parser::parse_module(client).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "net import must not instantiate without a Net grant");
    }

    #[test]
    fn net_capability_cannot_escalate() {
        // connect outside the granted allow-list is denied.
        let connect_denied = r#"
fn main(console: Console, net: Net):
    net.connect("evil.test:80").send_line("x")
"#;
        let e = interpreter::run_with(connect_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected a connect denial, got: {e}");

        // narrowing to an address not already held is denied (can't widen).
        let restrict_denied = r#"
fn main(console: Console, net: Net):
    net.only(Net.tcp("evil.test", 80)).connect("evil.test:80").send_line("x")
"#;
        // `resolve_std_src` links `policy`; `run_module` grants the Net allow-list.
        let e = interpreter::run_module(resolve_std_src(restrict_denied), ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not in this Net"), "expected a restrict denial, got: {e}");

        // Attenuation is real: after narrowing to one address, a sibling that
        // was in the original grant is no longer reachable.
        let attenuated = r#"
fn main(console: Console, net: Net):
    let narrow = net.only(Net.tcp("a.test", 80))
    narrow.connect("b.test:80").send_line("x")
"#;
        let e = interpreter::run_module(resolve_std_src(attenuated), ".", vec!["a.test:80".into(), "b.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected the sibling to be unreachable, got: {e}");
    }

    #[test]
    fn std_http_get_url_against_loopback() {
        let out = run_fixed_http_program(
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nhello-url",
            |port| format!(
            r#"
import http
fn main(console: Console, net: Net):
    let target = "http://127.0.0.1:{port}/path"
    match http.try_get(net.fetch(http.origin(target)), target):
        Ok(r) -> console.print(http.body(r))
        Err(e) -> console.print(http.http_error_message(e))
"#
            ),
        );
        assert_eq!(out, vec!["hello-url"]);
    }

    #[test]
    fn std_http_get_against_loopback() {
        let out = run_fixed_http_program(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            |port| format!(
            r#"
import http
fn main(console: Console, net: Net):
    let target = "http://127.0.0.1:{port}/"
    let r = http.get(net.fetch(http.origin(target)), target)
    console.print("${{http.status(r)}}")
    console.print(http.body(r))
"#
            ),
        );
        assert_eq!(out, vec!["200".to_string(), "hello".to_string()]);
    }

    #[test]
    fn std_http_rejects_malformed_status_line() {
        // A non-numeric status code (`BAD`) would otherwise trap string_to_int.
        let out = run_fixed_http_program(
            "HTTP/1.1 BAD Weird\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
            |port| format!(
            r#"
import http
fn main(console: Console, net: Net):
    let target = "http://127.0.0.1:{port}/"
    match http.try_get(net.fetch(http.origin(target)), target):
        Ok(_response) -> console.print("unexpected")
        Err(http.ProviderMalformedResponse(_message)) -> console.print("rejected")
        Err(error) -> console.print(http.http_error_message(error))
"#
            ),
        );
        assert_eq!(out, vec!["rejected".to_string()]);
    }

    #[test]
    fn std_http_post_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            // Read the full request: headers, then Content-Length body bytes.
            let mut data: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let text = String::from_utf8_lossy(&data).into_owned();
                if let Some(hdr_end) = text.find("\r\n\r\n") {
                    let clen: usize = text[..hdr_end]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if data.len() >= hdr_end + 4 + clen {
                        break;
                    }
                }
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(k) => data.extend_from_slice(&tmp[..k]),
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&data);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let target = "http://127.0.0.1:{port}/echo"
    let r = http.post(net.fetch(http.origin(target)), target, "hello body")
    console.print("${{http.status(r)}}")
    console.print(http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["200".to_string(), "hello body".to_string()]);
    }

    #[test]
    fn std_http_headers_against_loopback() {
        let out = run_fixed_http_program(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Custom: abc\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
            |port| format!(
            r#"
import http
import option
fn main(console: Console, net: Net):
    let target = "http://127.0.0.1:{port}/"
    let r = http.get(net.fetch(http.origin(target)), target)
    console.print(option.unwrap_or(http.header(r, "Content-Type"), "none"))
    console.print(option.unwrap_or(http.header(r, "x-custom"), "none"))
    console.print(option.unwrap_or(http.header(r, "Missing"), "none"))
"#
            ),
        );
        assert_eq!(out, vec!["application/json", "abc", "none"]);
    }
