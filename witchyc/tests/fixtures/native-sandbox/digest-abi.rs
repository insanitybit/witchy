//! Offline checks of guest spans at the native digest boundary.
#[cfg(not(witchy_native_aws_lc))]
#[path = "../../../runtime/native/digest.rs"]
mod digest;

unsafe extern "C" {
    fn witchy_public_digest(mode: u64, data: u64, length: u64, output: u64, capacity: u64) -> u64;
}

#[cfg(any(target_os = "macos", witchy_native_aws_lc))]
fn check_maximum_input() {
    // Independent SHA-2 results for 64 MiB of zero bytes.
    let vectors = [
        (
            256,
            "3b6a07d0d404fab4e23b6d34bc6696a6a312dd92821332385e5af7c01c421351",
        ),
        (
            384,
            "ca08389ab41a62c869fb6d774dc44a5478a4af022ad1a6c9cdee5e45236b427f37c0d352fa4784ef8df16def91540625",
        ),
        (
            512,
            "450766d07ea8acdba4e42a47e3de22ddb35678d62ae5446832b6e3e51780ab92f365ab982152d4d63be9954770997a5438b4fb7f4db5927b9973e82dd1ce0346",
        ),
    ];
    let input = vec![0u8; 64 * 1024 * 1024];
    for (mode, expected) in vectors {
        let mut output = [0xA5u8; 65];
        let capacity = mode / 8;
        assert_eq!(
            unsafe {
                witchy_public_digest(
                    mode,
                    input.as_ptr().expose_provenance() as u64,
                    input.len() as u64,
                    output.as_mut_ptr().expose_provenance() as u64,
                    capacity,
                )
            },
            0
        );
        let end = capacity as usize;
        let actual: String = output[..end]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected);
        assert!(output[end..].iter().all(|&byte| byte == 0xA5));
    }
}

#[cfg(any(target_os = "macos", witchy_native_aws_lc))]
fn main() {
    check_maximum_input();
    for mode in [256, 384, 512] {
        let capacity = mode / 8;
        let mut expected = [0u8; 64];
        let mut overlapping = [0xA5u8; 80];
        overlapping[..3].copy_from_slice(b"abc");
        assert_eq!(
            unsafe {
                witchy_public_digest(
                    mode,
                    b"abc".as_ptr().expose_provenance() as u64,
                    3,
                    expected.as_mut_ptr().expose_provenance() as u64,
                    capacity,
                )
            },
            0
        );

        let base = overlapping.as_mut_ptr();
        assert_eq!(
            unsafe {
                witchy_public_digest(
                    mode,
                    base.expose_provenance() as u64,
                    3,
                    base.expose_provenance() as u64,
                    capacity,
                )
            },
            0
        );
        let end = capacity as usize;
        assert_eq!(overlapping[..end], expected[..end]);
        assert!(overlapping[end..].iter().all(|&byte| byte == 0xA5));

        expected.fill(0xA5);
        let output = expected.as_mut_ptr().expose_provenance() as u64;
        assert_eq!(
            unsafe { witchy_public_digest(mode, 0, 0, output, capacity) },
            0
        );
        assert!(expected[end..].iter().all(|&byte| byte == 0xA5));
        assert_eq!(
            unsafe { witchy_public_digest(mode, 0, 1, 0, capacity - 1) },
            2
        );
        assert_eq!(
            unsafe { witchy_public_digest(mode, 0, 64 * 1024 * 1024 + 1, 0, capacity) },
            3
        );
    }
}

#[cfg(all(target_os = "linux", not(witchy_native_aws_lc)))]
fn main() {
    for mode in [256, 384, 512] {
        let capacity = mode / 8;
        assert_eq!(
            unsafe { witchy_public_digest(mode, 0, 1, 0, capacity - 1) },
            2
        );
        assert_eq!(
            unsafe { witchy_public_digest(mode, 0, 64 * 1024 * 1024 + 1, 0, capacity) },
            3
        );
        // The provider-free path returns before accessing either address.
        assert_eq!(unsafe { witchy_public_digest(mode, 0, 1, 0, capacity) }, 4);
    }
}
