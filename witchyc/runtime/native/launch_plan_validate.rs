//! Validation of the compiler's embedded launch-plan format.
//!
//! This layer performs no I/O and does not bind or open capability sources.
//! The platform launcher must still perform those operations securely.

const MAX_BYTES: usize = 64 * 1024;
const MAX_RECORDS: usize = 512;
const MAX_FLAGS: usize = 32;
const MAX_FIELDS: usize = 8;

fn fields(line: &[u8]) -> Option<([&[u8]; MAX_FIELDS], usize)> {
    let mut result = [&[][..]; MAX_FIELDS];
    let mut count = 0;
    for field in line.split(|byte| *byte == b'\t') {
        if count == MAX_FIELDS
            || field.is_empty()
            || field.iter().any(|&byte| byte < 0x20 || byte == 0x7f)
        {
            return None;
        }
        result[count] = field;
        count += 1;
    }
    Some((result, count))
}

fn record_width(tag: &[u8]) -> Option<usize> {
    Some(match tag {
        b"flag" => 6,
        b"limit" | b"args" | b"store" | b"object" | b"identity" => 3,
        b"socket" | b"env" | b"grant" | b"net" | b"listen" | b"exec-env" => 2,
        b"output" | b"allocator" | b"clock" | b"rand" => 1,
        b"dir" => 7,
        b"file" | b"input" | b"secret" => 5,
        b"exec" => 4,
        b"write-file" => 6,
        _ => return None,
    })
}

/// Validate framing, record shapes, and declared argument uniqueness.
/// Values remain borrowed from `plan`; no untrusted value is logged.
pub(super) fn valid(plan: &[u8]) -> bool {
    if plan.len() > MAX_BYTES || !plan.ends_with(b"\n") {
        return false;
    }
    let mut lines = plan[..plan.len() - 1].split(|byte| *byte == b'\n');
    let Some((header, 3)) = lines.next().and_then(fields) else {
        return false;
    };
    if header[0] != b"witchy-launch-plan"
        || header[1] != b"1"
        || header[2].len() != 64
        || !header[2]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return false;
    }
    let mut names = [&[][..]; MAX_FLAGS];
    let mut flag_count = 0;
    let mut record_count = 0;
    let mut ended = false;
    for line in lines {
        if ended {
            return false;
        }
        let Some((f, n)) = fields(line) else {
            return false;
        };
        if n == 1 && f[0] == b"end" {
            ended = true;
            continue;
        }
        if record_width(f[0]) != Some(n) || record_count == MAX_RECORDS {
            return false;
        }
        record_count += 1;
        if f[0] != b"flag" {
            continue;
        }
        if flag_count == MAX_FLAGS
            || names[..flag_count].contains(&f[1])
            || ![b"path".as_slice(), b"fd", b"object"].contains(&f[2])
            || ![b"0".as_slice(), b"1"].contains(&f[3])
        {
            return false;
        }
        let optional = f[4] != b"-";
        if optional != (f[5] != b"-")
            || (optional
                && (f[2] != b"path" || ![b"literal".as_slice(), b"home", b"cwd"].contains(&f[4])))
        {
            return false;
        }
        names[flag_count] = f[1];
        flag_count += 1;
    }
    ended
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// A line that splits has between one and eight fields, none empty and none
    /// with a control byte, and the fields with their tabs are the whole line.
    #[kani::proof]
    #[kani::unwind(19)]
    fn a_split_line_is_its_fields() {
        // Seventeen bytes can hold nine fields, one more than a line may have.
        let bytes: [u8; 17] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let line = &bytes[..length];
        if let Some((found, count)) = fields(line) {
            assert!(1 <= count && count <= MAX_FIELDS);
            kani::cover!(count == MAX_FIELDS);
            let mut total = count - 1;
            let mut index = 0;
            while index < count {
                let field = found[index];
                assert!(!field.is_empty());
                let at: usize = kani::any();
                kani::assume(at < field.len());
                assert!(field[at] >= 0x20 && field[at] != 0x7f && field[at] != b'\t');
                total += field.len();
                index += 1;
            }
            assert_eq!(total, length);
        }
    }

    /// Every known record is at least its tag and no wider than a line can be.
    #[kani::proof]
    #[kani::unwind(12)]
    fn a_record_width_fits_a_line() {
        let bytes: [u8; 10] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Some(width) = record_width(&bytes[..length]) {
            assert!(1 <= width && width <= MAX_FIELDS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid;
    const HEAD: &str =
        "witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";

    #[test]
    fn accepts_declared_records_and_defaults() {
        let text = format!(
            "{HEAD}flag\t--path\tpath\t0\thome\tdocuments\noutput\ndir\ta\tb\tc\td\te\tf\nend\n"
        );
        assert!(valid(text.as_bytes()));
    }

    #[test]
    fn rejects_ambiguous_or_unrecognized_plans() {
        for body in [
            "end\noutput\n",
            "output\n",
            "unknown\nend\n",
            "flag\tx\tfd\t0\thome\tfoo\nend\n",
            "flag\tx\tpath\t0\t-\t-\nflag\tx\tpath\t0\t-\t-\nend\n",
            "flag\tx\tpath\t0\t-\t-\0\nend\n",
        ] {
            assert!(!valid(format!("{HEAD}{body}").as_bytes()), "{body:?}");
        }
    }
}
