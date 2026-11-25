//! Deadline sent by an Exec capability to the broker.

pub(super) const DEFAULT_MS: u64 = 2000;

pub(super) fn command_ms(requested: u64, launch_remaining: Option<u64>) -> u64 {
    let command = if requested == 0 { DEFAULT_MS } else { requested };
    command.min(launch_remaining.unwrap_or(u64::MAX))
}

#[cfg(test)]
#[test]
fn defaults_to_two_seconds_and_uses_the_earliest_deadline() {
    assert_eq!(command_ms(0, None), 2000);
    assert_eq!(command_ms(0, Some(2500)), 2000);
    assert_eq!(command_ms(0, Some(700)), 700);
    assert_eq!(command_ms(900, None), 900);
    assert_eq!(command_ms(900, Some(700)), 700);
    assert_eq!(command_ms(900, Some(0)), 0);
}
