//! Exercise the guest file ABI with a controlled peer on the real broker transport.
#![allow(dead_code)] // Only the read path from these runtime modules is linked.

#[path = "../../runtime/native/errno.rs"]
mod errno;
#[path = "../../runtime/native/global.rs"]
mod global;
#[path = "../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../runtime/native/budget.rs"]
mod budget;
#[path = "../../runtime/native/directory.rs"]
mod directory;
#[path = "../../runtime/native/file_reader.rs"]
mod file_reader;
#[path = "../../runtime/native/io_budget.rs"]
mod io_budget;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::Command;

unsafe extern "C" {
    fn witchy_directory(operation: u64, handle: u64, value: u64) -> u64;
    fn witchy_file_byte(handle: u64) -> u64;
    fn witchy_file_read(handle: u64, address: u64, count: u64) -> u64;
}

// These symbols are linked by the guest modules but unavailable in this fixture.
#[unsafe(no_mangle)]
extern "C" fn witchy_open_directory_output(_: *const u8, _: usize, _: bool, _: bool) -> u64 {
    std::process::abort()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_check() {
    std::process::abort()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_byte() -> u64 {
    std::process::abort()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_read(_: *mut u8, _: usize) -> u64 {
    std::process::abort()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_close() {
    std::process::abort()
}

#[test]
fn brokered_reads_distinguish_eof_from_failure() {
    const CASE: &str = "WITCHY_TEST_NATIVE_FILE_READ";
    if let Some(scenario) = std::env::var_os(CASE) {
        let scenario = scenario.to_str().unwrap();
        let (reader, scenario) = scenario.split_once(':').unwrap();
        let (budget, reply) = scenario.split_once(':').unwrap();
        let (guest, broker) = UnixStream::pair().unwrap();
        guest.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        broker.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let reply = reply.to_owned();
        let peer = std::thread::spawn(move || serve_read(broker, &reply));

        file_reader::bind_channel(guest.as_raw_fd());
        io_budget::set_connection_filesystem(budget.parse().unwrap());
        const READ_ROOT: u64 = 1024;
        unsafe { witchy_directory(0, READ_ROOT, u64::from(b'f')) };
        let file = unsafe { witchy_directory(1, READ_ROOT, 0) };
        assert!(file >= 16384);
        if reader == "byte" {
            assert_eq!(unsafe { witchy_file_byte(file) }, u64::from(b'A'));
            assert_eq!(unsafe { witchy_file_byte(file) }, 256);
        } else {
            assert_eq!(reader, "bulk");
            let mut byte = 0u8;
            let output = (&raw mut byte) as u64;
            assert_eq!(unsafe { witchy_file_read(file, output, 0) }, 0);
            assert_eq!(unsafe { witchy_file_read(file, output, 1) }, 1);
            assert_eq!(byte, b'A');
            assert_eq!(unsafe { witchy_file_read(file, output, 1) }, 0);
        }
        peer.join().unwrap();
        return;
    }

    for reader in ["byte", "bulk"] {
        for budget in [1, 2] {
            for (reply, expected) in [("eof", 0), ("refused", 2), ("disconnected", 2)] {
                let scenario = format!("{reader}:{budget}:{reply}");
                let output = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "native_file_reads::brokered_reads_distinguish_eof_from_failure"])
                    .env(CASE, &scenario)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(expected), "{scenario}: {output:?}");
                if expected == 2 {
                    assert_eq!(output.stderr, b"witchy: native file read failed; the run is stopped\n");
                }
            }
        }
    }
}

fn serve_read(mut broker: UnixStream, reply: &str) {
    let mut request = [0; 6];
    broker.read_exact(&mut request).unwrap();
    assert_eq!(request, [10, 0, 2, 0, 16, b'f']);
    broker.write_all(&[0, 0, 0]).unwrap();
    let mut request = [0; 4];
    broker.read_exact(&mut request).unwrap();
    assert_eq!(request, [11, 0, 1, 0]);
    broker.write_all(&[0, 0, 1, b'A']).unwrap();
    broker.read_exact(&mut request).unwrap();
    assert_eq!(request, [11, 0, 1, 0]);
    match reply {
        "eof" => broker.write_all(&[0, 0, 0]).unwrap(),
        "refused" => broker.write_all(&[1, 0, 0]).unwrap(),
        "disconnected" => (),
        _ => panic!("unexpected reply {reply}"),
    }
}
