#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/sys.rs"]
mod sys;
#[path = "../../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/clock_random.rs"]
mod clock_random;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/file_snapshot.rs"]
mod file_snapshot;
#[path = "../../../runtime/native/input_stream.rs"]
mod input_stream;
#[path = "../../../runtime/native/io_budget.rs"]
mod io_budget;
#[path = "../../../runtime/native/linux_stat.rs"]
mod linux_stat;
#[path = "../../../runtime/native/secret_guard.rs"]
mod secret_guard;

#[test]
fn snapshot_reads_only_the_initialized_remainder() {
    use std::{
        fs::{OpenOptions, remove_file},
        io::{Seek, Write},
        os::fd::IntoRawFd,
        ptr,
    };

    unsafe extern "C" {
        fn witchy_snapshot_read(out: *mut u8, count: usize) -> u64;
        fn witchy_snapshot_close();
    }

    let path = std::env::temp_dir().join(format!("witchy-snapshot-copy-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(b"a\x00\xffbc").unwrap();
    file.rewind().unwrap();
    assert!(file_snapshot::snapshot_opened(file.into_raw_fd()));
    remove_file(path).unwrap();

    let mut out = [0xA5; 8];
    io_budget::set_connection_filesystem(2);
    assert_eq!(unsafe { witchy_snapshot_read(ptr::null_mut(), 0) }, 0);
    assert_eq!(io_budget::witchy_filesystem_remaining(), 2);
    assert_eq!(unsafe { witchy_snapshot_read(out.as_mut_ptr(), out.len()) }, 2);
    assert_eq!(io_budget::witchy_filesystem_remaining(), 0);
    assert_eq!(out, [b'a', 0, 0xA5, 0xA5, 0xA5, 0xA5, 0xA5, 0xA5]);
    assert_eq!(unsafe { witchy_snapshot_read(ptr::null_mut(), 0) }, 0);
    io_budget::set_connection_filesystem(u64::MAX);
    out.fill(0xA5);
    assert_eq!(unsafe { witchy_snapshot_read(out.as_mut_ptr(), out.len()) }, 3);
    assert_eq!(out, [0xFF, b'b', b'c', 0xA5, 0xA5, 0xA5, 0xA5, 0xA5]);
    assert_eq!(unsafe { witchy_snapshot_read(ptr::null_mut(), 8) }, 0);
    unsafe { witchy_snapshot_close() };
}
