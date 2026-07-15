//! Regression coverage for fallible full-URL HTTP helpers.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn get_url_returns_connect_error_on_both_backends() {
    // Reserve then release a loopback port so connect fails immediately while
    // remaining inside the explicitly granted network authority.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("listener address").port();
    let addr = format!("127.0.0.1:{port}");
    drop(listener);

    let source = format!(
        "import http\nfn main(console: Console, net: Net):\n    let target = \"http://127.0.0.1:{port}/\"\n    match http.get_url(net, target):\n        Ok(_) -> console.print(\"unexpected typed success\")\n        Err(http.ConnectFailed(host, failed_port)) -> console.print(\"connect:${{host}}:${{failed_port}}\")\n        Err(e) -> console.print(\"unexpected typed error: \" + http.http_error_message(e))\n    match http.get_url_string(net, target):\n        Ok(_) -> console.print(\"unexpected string success\")\n        Err(e) -> console.print(e)\n"
    );
    let module = parser::parse_module(&source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typecheck");

    let expected = vec![
        format!("connect:127.0.0.1:{port}"),
        format!("connect to 127.0.0.1:{port} failed (unreachable)"),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interpret"),
        expected,
        "interpreter must return Err rather than trap",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("program supports compiled execution");
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
    assert_eq!(
        actor.output(),
        expected,
        "compiled runtime must return the same Err rather than trap",
    );
}
