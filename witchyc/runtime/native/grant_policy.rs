//! Platform-independent checks for the launcher's required grants.

use core::ffi::CStr;

const ARGS: u64 = 64;

const RULES: [(u64, &CStr); 10] = [
    (1, c"Console<Write> requires --output\n"),
    (2, c"Allocator requires --allocator\n"),
    (4, c"Net requires --net=IP:PORT\n"),
    (8, c"Console<Read> requires --input\n"),
    (16, c"Clock requires --clock\n"),
    (32, c"Rand requires --rand\n"),
    (
        ARGS,
        c"Args requires --args=COUNT:BYTES covering all --arg values\n",
    ),
    (128, c"Env requires --env=NAME[=VALUE]\n"),
    (16384, c"Grants requires --grant=Type.field=VALUE\n"),
    (262144, c"Exec requires --exec=DIR:name,name\n"),
];

/// Return the first missing grant in diagnostic order. `args_fit` describes
/// whether the bound argument values fit the program's declared limits.
pub(super) fn refusal(required: u64, supplied: u64, args_fit: bool) -> Option<&'static CStr> {
    let missing = required & !supplied;
    RULES.iter().find_map(|&(grant, message)| {
        (missing & grant != 0 || (grant == ARGS && required & ARGS != 0 && !args_fit))
            .then_some(message)
    })
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(12)]
fn missing_known_grants_are_refused() {
    let required: u64 = kani::any();
    let supplied: u64 = kani::any();
    let args_fit: bool = kani::any();
    let known = 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 16384 | 262144;
    let missing = required & !supplied & known;
    let args_out_of_bounds = required & ARGS != 0 && !args_fit;
    assert_eq!(
        refusal(required, supplied, args_fit).is_some(),
        missing != 0 || args_out_of_bounds
    );
}

#[cfg(test)]
#[test]
fn required_grants_and_argument_limits_are_checked_in_order() {
    for &(grant, message) in &RULES {
        assert_eq!(refusal(grant, 0, true), Some(message));
        assert_eq!(refusal(grant, grant, true), None);
    }
    assert_eq!(refusal(ARGS, ARGS, false), Some(RULES[6].1));
    assert_eq!(refusal(0, 0, false), None);
    assert_eq!(refusal(ARGS | 2, 0, false), Some(RULES[1].1));
}
