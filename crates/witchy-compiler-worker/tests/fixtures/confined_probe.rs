#![deny(unsafe_code)]

use std::io::{Read, Write};

use witchy_compiler_worker::arm_test_fixture;
use witchy_worker_protocol::{WorkerResponse, decode_request};

fn main() {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    let request = decode_request(&bytes).unwrap();
    assert!(
        std::env::vars_os().next().is_none(),
        "parent environment reached worker"
    );
    arm_test_fixture(&request).unwrap();

    let filesystem = std::fs::read("/etc/passwd").unwrap_err();
    assert_eq!(filesystem.kind(), std::io::ErrorKind::PermissionDenied);
    let network = std::net::TcpStream::connect(("127.0.0.1", 9)).unwrap_err();
    assert_eq!(network.kind(), std::io::ErrorKind::PermissionDenied);
    let process = std::process::Command::new("/bin/true")
        .status()
        .unwrap_err();
    assert_eq!(process.kind(), std::io::ErrorKind::PermissionDenied);

    let response = WorkerResponse::candidate(
        &request,
        b"probe candidate WIR".to_vec(),
        b"probe AuthorityWIR".to_vec(),
        b"probe final-byte plan".to_vec(),
        String::new(),
    )
    .unwrap();
    std::io::stdout()
        .write_all(&response.encode().unwrap())
        .unwrap();
}
