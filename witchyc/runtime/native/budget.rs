//! Shared and per-connection accounting for the native host.

pub fn parse_limit(text: &[u8]) -> Option<u64> {
    if text == b"unlimited" {
        return Some(u64::MAX);
    }
    let mut limit = 0u64;
    if text.is_empty() {
        return None;
    }
    for &byte in text {
        if !byte.is_ascii_digit() {
            return None;
        }
        limit = limit.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
        if limit > 1 << 40 {
            return None;
        }
    }
    Some(limit)
}

pub fn debit(total: u64, connection: u64, bytes: u64) -> Option<(u64, u64)> {
    if bytes > total.min(connection) {
        return None;
    }
    Some((debit_limit(total, bytes), debit_limit(connection, bytes)))
}

fn debit_limit(limit: u64, bytes: u64) -> u64 {
    if limit == u64::MAX {
        return limit;
    }
    // debit checks both limits before computing either result.
    limit.wrapping_sub(bytes)
}

#[cfg(kani)]
mod verification {
    #[kani::proof]
    fn debit_matches_policy() {
        let total: u64 = kani::any();
        let connection: u64 = kani::any();
        let bytes: u64 = kani::any();
        let result = super::debit(total, connection, bytes);
        if total < bytes || connection < bytes {
            assert_eq!(result, None);
        } else {
            let expected = |limit| {
                if limit == u64::MAX {
                    limit
                } else {
                    limit - bytes
                }
            };
            assert_eq!(result, Some((expected(total), expected(connection))));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_limit_syntax_and_boundaries() {
        for (text, expected) in [
            ("unlimited", Some(u64::MAX)),
            ("0", Some(0)),
            ("0001", Some(1)),
            ("1099511627776", Some(1 << 40)),
            ("1099511627777", None),
            ("18446744073709551616", None),
            ("", None),
            ("+1", None),
            ("-1", None),
            ("1 ", None),
            (" 1", None),
            ("Unlimited", None),
        ] {
            assert_eq!(super::parse_limit(text.as_bytes()), expected, "{text:?}");
        }
    }

    #[test]
    fn debits_match_reference_model_across_a_sequence() {
        for initial_total in [0, 7, 1000, u64::MAX] {
            for initial_connection in [0, 3, 50, u64::MAX] {
                let mut actual = (initial_total, initial_connection);
                let cap = |value| (value != u64::MAX).then_some(u128::from(value));
                let mut expected = (cap(initial_total), cap(initial_connection));
                let expose = |value: Option<u128>| value.map_or(u64::MAX, |n| n as u64);
                for bytes in (0..128).map(|i| (i * 71) % 23).chain([u64::MAX]) {
                    let debit = |limit: Option<u128>| match limit {
                        None => Some(None),
                        Some(limit) => limit.checked_sub(u128::from(bytes)).map(Some),
                    };
                    let next = debit(expected.0).zip(debit(expected.1));
                    let result = super::debit(actual.0, actual.1, bytes);
                    assert_eq!(result, next.map(|(a, b)| (expose(a), expose(b))));
                    if let Some((left, right)) = next {
                        expected = (left, right);
                        actual = (expose(left), expose(right));
                    }
                }
            }
        }
    }
}
