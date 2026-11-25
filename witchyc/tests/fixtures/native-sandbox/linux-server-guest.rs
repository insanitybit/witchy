#![no_std]

#[unsafe(no_mangle)]
static witchy_entry_grants: u64 = 131072;
#[unsafe(no_mangle)]
static witchy_launch_plan: [u8; 1] = [0];

// An empty secret-slot table, as emitted for guests without fixed secrets.
#[unsafe(no_mangle)]
static witchy_secret_slots: u8 = 0;

unsafe extern "C" {
    fn witchy_native_net(op: u64, handle: u64, operand: u64, count: u64) -> u64;
    fn witchy_public_digest(mode: u64, input: u64, length: u64, output: u64, capacity: u64) -> u64;
    fn _exit(status: i32) -> !;
}

#[unsafe(no_mangle)]
extern "C" fn witchy_entry(_: u64) {
    unsafe {
        // Linux without a crypto provider must return unavailable, not fail to link.
        let mut digest = [0u8; 48];
        let status = witchy_public_digest(384, 0, 0, digest.as_mut_ptr() as u64, 48);
        if status != 0 && status != 4 {
            _exit(12);
        }
        let byte = witchy_native_net(5, 131072, 0, 0);
        if byte > 255
            || witchy_native_net(4, 131072, byte, 0) != 0
            || witchy_native_net(6, 131072, 0, 0) != 0
        {
            _exit(11);
        }
    }
}
