#![deny(unsafe_code)]

use std::io::{Read, Write};

use witchy_worker_protocol::{WorkerResponse, decode_request};

fn main() {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    let request = decode_request(&bytes).unwrap();
    let response = if std::env::vars_os().next().is_some() {
        WorkerResponse::rejected(&request, "parent environment leaked into worker".into()).unwrap()
    } else {
        WorkerResponse::candidate(
            &request,
            b"untrusted candidate WIR".to_vec(),
            b"untrusted AuthorityWIR".to_vec(),
            b"untrusted final-byte plan".to_vec(),
            String::new(),
        )
        .unwrap()
    };
    std::io::stdout()
        .write_all(&response.encode().unwrap())
        .unwrap();
}
