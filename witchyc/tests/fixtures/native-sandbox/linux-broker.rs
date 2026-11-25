#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/sys.rs"]
mod sys;
#[path = "../../../runtime/native/linux_sandbox.rs"]
mod linux_sandbox;
use std::ffi::{c_char, c_int, c_long, c_void};
unsafe extern "C" {
    fn witchy_confine_broker(
        read_root: c_int,
        write_root: c_int,
        output_parent: c_int,
        ports: *const u16,
        count: u32,
        signal_fd: c_int,
        message_fd: c_int,
    ) -> bool;
    fn socketpair(domain: c_int, kind: c_int, protocol: c_int, result: *mut c_int) -> c_int;
    fn fork() -> c_int;
    fn close(fd: c_int) -> c_int;
    fn read(fd: c_int, data: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, data: *const c_void, count: usize) -> isize;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn _exit(status: c_int) -> !;
}
fn main() {
    let root = std::env::temp_dir().join(format!("witchy-broker-sandbox-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("allowed"), b"A").unwrap();
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    unsafe {
        let dir = open(path.as_ptr(), 0o40000);
        assert!(dir >= 0);
        let mut pair = [-1; 2];
        assert_eq!(socketpair(1, 1, 0, pair.as_mut_ptr()), 0);
        let child = fork();
        assert!(child >= 0);
        if child == 0 {
            close(pair[0]);
            if !witchy_confine_broker(dir, -1, -1, std::ptr::null(), 0, -1, -1) {
                _exit(70);
            }
            let fd = openat(dir, c"allowed".as_ptr(), 0);
            if fd < 0 {
                _exit(71);
            }
            let mut byte = 0u8;
            if read(fd, (&raw mut byte).cast(), 1) != 1 || byte != b'A' {
                _exit(72);
            }
            if openat(dir, c"allowed".as_ptr(), 1) >= 0 {
                _exit(73);
            }
            if open(c"/etc/passwd".as_ptr(), 0) >= 0 {
                _exit(74);
            }
            if syscall(198, 2i32, 1i32, 0i32) >= 0 {
                _exit(75);
            }
            if syscall(222, 0u64, 4096usize, 7i32, 0x22i32, -1i32, 0u64) >= 0 {
                _exit(76);
            }
            if write(pair[1], (&raw const byte).cast(), 1) != 1 {
                _exit(77);
            }
            _exit(0);
        }
        close(pair[1]);
        let mut byte = 0u8;
        let count = read(pair[0], (&raw mut byte).cast(), 1);
        let mut status = 0;
        assert_eq!(waitpid(child, &raw mut status, 0), child);
        assert_eq!(status, 0, "broker child status {status}");
        assert_eq!((count, byte), (1, b'A'));
        close(dir);
        close(pair[0]);
        let dial_child = fork();
        assert!(dial_child >= 0);
        if dial_child == 0 {
            let port = 12345u16;
            if !witchy_confine_broker(-1, -1, -1, &raw const port, 1, -1, -1) {
                _exit(78);
            }
            let stream = syscall(198, 2i32, 1i32, 0i32);
            if stream < 0 {
                _exit(79);
            }
            if syscall(198, 1i32, 1i32, 0i32) >= 0 {
                _exit(80);
            }
            if syscall(198, 2i32, 2i32, 0i32) >= 0 {
                _exit(81);
            }
            if syscall(
                206,
                stream,
                &raw const byte,
                1usize,
                0i32,
                &raw const byte,
                1usize,
            ) >= 0
            {
                _exit(82);
            }
            _exit(0);
        }
        let mut dial_status = 0;
        assert_eq!(waitpid(dial_child, &raw mut dial_status, 0), dial_child);
        assert_eq!(dial_status, 0, "dial broker child status {dial_status}");
    }
    std::fs::remove_dir_all(root).unwrap();
    println!("Linux broker confinement passed");
}
