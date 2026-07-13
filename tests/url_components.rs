//! URL component modeling and HTTP request-target regressions (BUG-476).

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn url_components_round_trip_and_http_omits_fragments() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("listener address").port();
    let addr = format!("127.0.0.1:{port}");
    let (request_tx, request_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let request = String::from_utf8(request).expect("request is UTF-8");
            let first_line = request.lines().next().unwrap_or_default().to_string();
            request_tx.send(first_line).expect("record request line");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write response");
        }
    });

    let source = format!(
        r#"import http
import url

fn optional(value: Option(String)) -> String:
    match value:
        Some(text) -> text
        None -> "-"

fn describe(console: Console, raw: String):
    match url.parse(raw):
        Err(e) -> console.print("error: " + url.url_error_message(e))
        Ok(u) -> console.print(url.pathname(u) + "|" + optional(url.query(u)) + "|" + optional(url.fragment(u)) + "|" + url.request_target(u) + "|" + url.format(u))

fn main(console: Console, net: Net):
    describe(console, "https://h/p")
    describe(console, "https://h?x=1")
    describe(console, "https://h#frag")
    describe(console, "https://h/p?x=1#frag")
    describe(console, "https://h/p?#")
    match http.get_request("http://127.0.0.1:{port}/p?old=1#frag").with_query("x", "a b").send(net):
        Ok(response) -> console.print("status=${{http.status(response)}}")
        Err(e) -> console.print("error: " + http.http_error_message(e))
"#
    );
    let module = parser::parse_module(&source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typecheck");

    let expected = vec![
        "/p|-|-|/p|https://h/p".to_string(),
        "/|x=1|-|/?x=1|https://h?x=1".to_string(),
        "/|-|frag|/|https://h#frag".to_string(),
        "/p|x=1|frag|/p?x=1|https://h/p?x=1#frag".to_string(),
        "/p|||/p?|https://h/p?#".to_string(),
        "status=200".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interpret"),
        expected,
        "interpreter URL behavior",
    );

    let wasm = codegen::compile_module_binary(&linked)
        .expect("compile")
        .expect("program supports compiled execution");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
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
    actor.run().expect("compiled execution");
    assert_eq!(actor.output(), expected, "compiled URL behavior");

    let request_lines: Vec<String> = (0..2)
        .map(|_| {
            request_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("captured request line")
        })
        .collect();
    assert_eq!(
        request_lines,
        [
            "GET /p?old=1&x=a%20b HTTP/1.1",
            "GET /p?old=1&x=a%20b HTTP/1.1",
        ],
        "fragments are client-side only on both backends",
    );
    server.join().expect("server thread");
}
