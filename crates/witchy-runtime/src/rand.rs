//! The `Rand` capability's randomness source, shared by both backends.
//!
//! Production: each `rand_u64` draw comes from the OS CSPRNG (host-side `getrandom`
//! natively, the browser's `crypto.getRandomValues` in the wasm shim) — non-deterministic.
//!
//! Tests / parity: when `WITCHY_RAND_SEED` is set, both the interpreter and the compiled
//! host draw from this same splitmix64 sequence instead, so a program using `rand_u64`
//! stays parity-identical across backends and is reproducible. This module is pure (no
//! `getrandom`), so it compiles on every target; the CSPRNG path lives in each native
//! backend.

/// The deterministic seed from `WITCHY_RAND_SEED`, or `None` for the OS CSPRNG.
pub fn seed_from_env() -> Option<u64> {
    std::env::var("WITCHY_RAND_SEED").ok().and_then(|s| s.parse::<u64>().ok())
}

/// One splitmix64 step: advance `state` in place and return the next value. Identical on
/// every backend, so a seeded run is deterministic and parity-stable.
pub fn seeded_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_sequence_is_deterministic_per_seed() {
        let draw = |seed: u64| -> Vec<u64> {
            let mut s = seed;
            (0..6).map(|_| seeded_next(&mut s)).collect()
        };
        // Same seed → identical sequence (the parity/reproducibility guarantee).
        assert_eq!(draw(42), draw(42));
        // Different seed → different sequence (it actually mixes).
        assert_ne!(draw(42), draw(7));
        // And it advances (consecutive draws differ).
        let seq = draw(42);
        assert_ne!(seq[0], seq[1]);
    }
}
