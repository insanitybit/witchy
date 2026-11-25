//! Shared classification of native launcher arguments.

pub(super) fn classify(arg: &[u8]) -> u32 {
    match arg {
        b"--output" => 1,
        b"--stream-output" => 2,
        b"--plain-output" => 3,
        b"--unmetered" => 7,
        b"--allocator" => 8,
        b"--no-links" => 9,
        b"--instance-per-connection" => 16,
        b"--data-only" => 22,
        b"--dir-rights=read,write" => 24,
        b"--dir-rights=write" => 25,
        b"--rand" => 31,
        b"--clock" => 32,
        b"--input-stdin" => 34,
        _ if arg.starts_with(b"--limit-name=") => 4,
        _ if arg.starts_with(b"--fuel=") => 5,
        _ if arg.starts_with(b"--fuel-per-accept=") => 6,
        _ if arg.starts_with(b"--secret=") => 10,
        _ if arg.starts_with(b"--secret-seal=") => 11,
        _ if arg.starts_with(b"--secret-slot=") => 12,
        _ if arg.starts_with(b"--grant=") => 13,
        _ if arg.starts_with(b"--write-file=") => 14,
        _ if arg.starts_with(b"--write-dir=") => 15,
        _ if arg.starts_with(b"--connections=") => 17,
        _ if arg.starts_with(b"--waiting-connections=") => 18,
        _ if arg.starts_with(b"--connection-deadline-ms=") => 19,
        _ if arg.starts_with(b"--net-bytes-per-connection=") => 20,
        _ if arg.starts_with(b"--output-bytes-per-connection=") => 21,
        _ if arg.starts_with(b"--dir-id=") => 23,
        _ if arg.starts_with(b"--dir=") => 26,
        _ if arg.starts_with(b"--file=") => 27,
        _ if arg.starts_with(b"--env=") => 28,
        _ if arg.starts_with(b"--args=") => 29,
        _ if arg.starts_with(b"--arg=") => 30,
        b"--input" => 33,
        _ if arg.starts_with(b"--input=") => 44,
        _ if arg.starts_with(b"--deadline-ms=") => 35,
        _ if arg.starts_with(b"--output-bytes=") => 36,
        _ if arg.starts_with(b"--input-bytes=") => 37,
        _ if arg.starts_with(b"--input-id=") => 38,
        _ if arg.starts_with(b"--fs-entries=") => 39,
        _ if arg.starts_with(b"--fs-bytes=") => 40,
        _ if arg.starts_with(b"--net-bytes=") => 41,
        _ if arg.starts_with(b"--net=") => 42,
        _ if arg.starts_with(b"--listen=") => 43,
        _ if arg.starts_with(b"--exec-env=") => 45,
        _ if arg.starts_with(b"--exec=") => 46,
        _ if arg.starts_with(b"--exec-processes=") => 47,
        _ if arg.starts_with(b"--exec-bytes=") => 48,
        _ if arg.starts_with(b"--fs-bytes-per-connection=") => 49,
        _ if arg.starts_with(b"--fs-entries-per-connection=") => 50,
        _ => 0,
    }
}

#[cfg(test)]
#[test]
fn classifies_options_without_accepting_prefix_lookalikes() {
    for (arg, expected) in [
        (b"--output".as_slice(), 1),
        (b"--allocator", 8),
        (b"--grant=App.value=42", 13),
        (b"--args=2:32", 29),
        (b"--net=127.0.0.1:80", 42),
        (b"--output-extra", 0),
        (b"--grant", 0),
        (b"--net", 0),
    ] {
        assert_eq!(classify(arg), expected);
    }
}
