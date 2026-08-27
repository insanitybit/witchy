#![deny(unsafe_code)]

use std::io::{Read, Write};

use witchy_worker_protocol::decode_request;

fn main() {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    let request = decode_request(&bytes).unwrap();
    let length = usize::try_from(request.limits().max_response_bytes).unwrap() + 1;
    std::io::stdout().write_all(&vec![0; length]).unwrap();
}
