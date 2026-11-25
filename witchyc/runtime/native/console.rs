//! Plain console rendering. The launcher charges the original input bytes.

fn hex(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + (nibble - 10)
    }
}

fn render(bytes: &[u8], line_start: &mut bool, mut write: impl FnMut(&[u8]) -> bool) -> bool {
    for &byte in bytes {
        if *line_start && !write(b"| ") {
            return false;
        }
        *line_start = byte == b'\n';
        if byte == b'\n' || byte == b'\t' || (0x20..=0x7e).contains(&byte) {
            if !write(core::slice::from_ref(&byte)) {
                return false;
            }
        } else if !write(&[b'\\', b'x', hex(byte >> 4), hex(byte & 15)]) {
            return false;
        }
    }
    true
}

#[cfg(not(test))]
unsafe extern "C" {
    fn fwrite(bytes: *const u8, size: usize, count: usize, stream: *mut core::ffi::c_void)
    -> usize;
    safe fn witchy_console_charge(len: usize);
    safe fn exit(status: core::ffi::c_int) -> !;
}

// Set by the launcher before entry. Console writes cannot suspend or reenter.
#[cfg(not(test))]
static mut STREAM: *mut core::ffi::c_void = core::ptr::null_mut();
#[cfg(not(test))]
static mut PLAIN: bool = false;
#[cfg(not(test))]
static mut LINE_START: bool = true;

#[cfg(not(test))]
unsafe extern "C" {
    fn setvbuf(
        stream: *mut core::ffi::c_void,
        buffer: *mut u8,
        mode: core::ffi::c_int,
        size: usize,
    ) -> core::ffi::c_int;
    fn fputs(text: *const core::ffi::c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
}

// BUFSIZ from Darwin stdio.h and Linux glibc stdio.h, respectively.
#[cfg(all(not(test), target_os = "macos"))]
const BUFFER_SIZE: usize = 1024;
#[cfg(all(not(test), target_os = "linux"))]
const BUFFER_SIZE: usize = 8192;

#[cfg(not(test))]
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_launch_console(
    stream: *mut core::ffi::c_void,
    stderr: *mut core::ffi::c_void,
    output: bool,
    streaming: bool,
    plain: bool,
) -> bool {
    const LINE_BUFFERED: core::ffi::c_int = 1;
    if plain && !streaming {
        unsafe {
            fputs(
                c"--plain-output requires --stream-output\n".as_ptr(),
                stderr,
            )
        };
        return false;
    }
    if streaming
        && (!output
            || unsafe { setvbuf(stream, core::ptr::null_mut(), LINE_BUFFERED, BUFFER_SIZE) } != 0)
    {
        unsafe {
            fputs(
                c"--stream-output requires a working --output grant\n".as_ptr(),
                stderr,
            )
        };
        return false;
    }
    #[cfg(all(not(test), target_os = "linux"))]
    if output && !streaming && !unsafe { witchy_buffer_console(stream, stderr) } {
        return false;
    }
    unsafe {
        STREAM = stream;
        PLAIN = plain;
        LINE_START = true;
    }
    true
}

/// Install the Linux buffer before seccomp so flush needs only write(1).
#[cfg(all(not(test), target_os = "linux"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_buffer_console(
    stream: *mut core::ffi::c_void,
    stderr: *mut core::ffi::c_void,
) -> bool {
    static mut BUFFER: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];
    const FULLY_BUFFERED: core::ffi::c_int = 0;
    if unsafe {
        setvbuf(
            stream,
            (&raw mut BUFFER).cast(),
            FULLY_BUFFERED,
            BUFFER_SIZE,
        )
    } == 0
    {
        return true;
    }
    unsafe {
        fputs(
            c"native sandbox could not initialize stdout\n".as_ptr(),
            stderr,
        )
    };
    false
}

fn decimal(mut value: u64, buffer: &mut [u8; 21]) -> &[u8] {
    let mut digits = 1;
    if let Some((newline, positions)) = buffer.split_last_mut() {
        *newline = b'\n';
        for position in positions.iter_mut().rev() {
            *position = b'0' + (value % 10) as u8;
            digits += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
    }
    buffer
        .get(buffer.len().saturating_sub(digits)..)
        .unwrap_or(&[])
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn witchy_print(console: u64, value: u64, mode: u64) {
    if console != 1 {
        exit(2)
    }
    let mut line = [0; 21];
    let word = value.to_le_bytes();
    let byte = [value as u8];
    let bytes = match mode {
        0 if value <= 255 => byte.as_slice(),
        0 => {
            witchy_console_charge(1);
            exit(2)
        },
        1 => word.as_slice(),
        2 => decimal(value, &mut line),
        _ => exit(2),
    };
    witchy_console_charge(bytes.len());
    let stream = unsafe { STREAM };
    if stream.is_null() {
        exit(2)
    }
    let write =
        |chunk: &[u8]| (unsafe { fwrite(chunk.as_ptr(), 1, chunk.len(), stream) }) == chunk.len();
    let ok = if unsafe { PLAIN } {
        render(bytes, unsafe { &mut *(&raw mut LINE_START) }, write)
    } else {
        write(bytes)
    };
    if !ok {
        exit(2)
    }
}

#[cfg(test)]
#[test]
fn decimal_covers_full_word() {
    let mut buffer = [0; 21];
    assert_eq!(decimal(u64::MAX, &mut buffer), b"18446744073709551615\n");
    assert_eq!(decimal(0, &mut buffer), b"0\n");
}

#[cfg(test)]
#[test]
fn rendering_is_independent_of_write_boundaries() {
    fn run(bytes: &[u8], split: usize) -> (Vec<u8>, bool) {
        let mut out = Vec::new();
        let mut start = true;
        for chunk in [&bytes[..split], &bytes[split..]] {
            assert!(render(chunk, &mut start, |part| {
                out.extend_from_slice(part);
                true
            }));
        }
        (out, start)
    }
    let bytes = b"A\x1b\n\n\xff\t";
    let expected = (b"| A\\x1b\n| \n| \\xff\t".to_vec(), false);
    for split in 0..=bytes.len() {
        assert_eq!(run(bytes, split), expected);
    }
}
