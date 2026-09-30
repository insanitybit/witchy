//! Regression coverage for fallible full-URL HTTP helpers.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

#[test]
fn get_url_returns_connect_error_instead_of_trapping() {
    // Reserve then release a loopback port so connect fails immediately while
    // remaining inside the explicitly granted network authority.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("listener address").port();
    let addr = format!("127.0.0.1:{port}");
    drop(listener);

    let source = format!(
        "import http\nfn main(console: Console, net: Net):\n    let target = \"http://127.0.0.1:{port}/\"\n    match http.try_get(net.fetch(http.origin(target)), target):\n        Ok(_) -> console.print(\"unexpected typed success\")\n        Err(http.Network(_message)) -> console.print(\"network\")\n        Err(e) -> console.print(\"unexpected typed error: \" + http.http_error_message(e))\n"
    );
    let expected = vec!["network".to_string()];
    let checked = witchy::resolve_std_only_checked(&source).expect("checked fallible HTTP source");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the canonical path lowers fallible HTTP execution");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
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
