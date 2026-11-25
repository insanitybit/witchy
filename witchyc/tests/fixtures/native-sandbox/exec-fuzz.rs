//! Exercise the actual Exec decoder with deterministic malformed requests.
use std::ffi::{c_char, c_int};

unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn witchy_exec_granted();
    fn witchy_exec_program(device: u64, inode: u64) -> bool;
    fn witchy_exec_setup(
        directory: c_int,
        names: *const *const c_char,
        count: u32,
        budget: u64,
    ) -> bool;
    fn witchy_exec_request(
        channel: c_int,
        operation: u8,
        bytes: *mut u8,
        count: usize,
        capacity: usize,
        reply: *mut usize,
    ) -> bool;
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn fill(bytes: &mut [u8], state: &mut u64) {
    let mut blocks = bytes.chunks_exact_mut(8);
    for block in &mut blocks {
        block.copy_from_slice(&next(state).to_le_bytes());
    }
    let tail = blocks.into_remainder();
    if !tail.is_empty() {
        tail.copy_from_slice(&next(state).to_le_bytes()[..tail.len()]);
    }
}

fn main() {
    unsafe { witchy_exec_granted() };
    assert!(unsafe { witchy_exec_program(1, 1) });
    #[cfg(target_os = "macos")]
    const DIRECTORY: c_int = 0x100000;
    #[cfg(target_os = "linux")]
    const DIRECTORY: c_int = 0o200000;
    let directory = unsafe { open(c".".as_ptr(), DIRECTORY) };
    let names = [c"absent-program".as_ptr()];
    assert!(directory >= 0 && unsafe { witchy_exec_setup(directory, names.as_ptr(), 1, 4096) });
    assert!(!unsafe { witchy_exec_program(1, 1) });
    // Keep input bytes and request parameters on separate deterministic streams.
    let mut parameters = 0x9e3779b97f4a7c15u64;
    let mut payload = 0xd1b54a32d192ed03u64;
    let mut random = || next(&mut parameters);
    let mut bytes = [0u8; 16384];
    let mut operations = [0; 5];
    let mut length_choices = [0; 4];
    let mut undersized_buffers = 0;
    let (mut answered, mut refused, mut decoded) = (0, 0, 0);
    for _ in 0..60_000 {
        let operation_index = (random() % 5) as usize;
        operations[operation_index] += 1;
        let operation = 26 + operation_index as u8;
        let valid_run = operation == 28 && random() % 4 == 0;
        let count = if valid_run {
            11
        } else {
            let length_choice = (random() % 4) as usize;
            length_choices[length_choice] += 1;
            match length_choice {
                0 => random() as usize % 16,
                1 => 10,
                2 => random() as usize % (bytes.len() + 1),
                _ => bytes.len(),
            }
        };
        fill(&mut bytes[..count], &mut payload);
        if valid_run {
            bytes[..11].fill(0);
            bytes[0] = 1; // No program is granted at this index.
        } else if operation == 28 && random() % 2 == 0 {
            bytes[..5].copy_from_slice(&[(random() % 3) as u8, 0, 0, 0, (random() % 20) as u8]);
        }
        let capacity = if valid_run || random() % 2 == 0 {
            bytes.len()
        } else {
            random() as usize % 12
        };
        undersized_buffers += usize::from(capacity < count);
        let mut reply = 0;
        let ok = unsafe {
            witchy_exec_request(
                -1,
                operation,
                bytes.as_mut_ptr(),
                count,
                capacity,
                &mut reply,
            )
        };
        assert!(reply <= capacity);
        if ok && operation == 28 {
            decoded += 1;
            assert!(bytes[0] <= 6 && bytes[3..7] == [0; 4]);
        }
        if ok {
            answered += 1;
        } else {
            refused += 1;
        }
    }
    assert!(operations.iter().all(|&count| count > 0));
    assert!(length_choices.iter().all(|&count| count > 0));
    assert!(undersized_buffers > 0);
    println!("ok {answered} {refused} {decoded}");
}
