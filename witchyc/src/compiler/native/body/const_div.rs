//! Full-width invariant unsigned division via a scaled reciprocal.
use super::{Binary, Body, InstBuilder, Value};

struct Reciprocal {
    multiplier: u64,
    shift: u32,
    correction: bool,
}

impl Reciprocal {
    fn for_divisor(divisor: u64) -> Self {
        debug_assert!(divisor != 0 && !divisor.is_power_of_two());
        let shift = divisor.ilog2();
        let scale = 1u128 << (64 + shift);
        let divisor = u128::from(divisor);
        let quotient = scale / divisor;
        let remainder = scale % divisor;

        // Use the rounded-up reciprocal if its error fits this scale.
        // Otherwise use one extra bit, reconstructing its high bit in the
        // emitted quotient with q + (dividend - q) / 2 before shifting.
        let correction = divisor - remainder >= 1u128 << shift;
        let multiplier = if correction {
            quotient * 2 + (remainder * 2) / divisor
        } else {
            quotient
        };
        Self {
            multiplier: (multiplier + 1) as u64,
            shift,
            correction,
        }
    }
}

impl Body<'_, '_> {
    pub(super) fn unsigned_constant_division(
        &mut self,
        op: Binary,
        dividend: Value,
        divisor: u64,
    ) -> Value {
        if divisor.is_power_of_two() {
            return if matches!(op, Binary::Div) {
                self.fb
                    .ins()
                    .ushr_imm_s(dividend, i64::from(divisor.trailing_zeros()))
            } else {
                self.fb.ins().band_imm_s(dividend, (divisor - 1) as i64)
            };
        }
        let block = self.fb.current_block().expect("division in a native block");
        let key = (block, dividend, divisor);
        let quotient = if let Some(&quotient) = self.quotients.get(&key) {
            quotient
        } else {
            let reciprocal = Reciprocal::for_divisor(divisor);
            let multiplier = self.constant(reciprocal.multiplier);
            let mut quotient = self.fb.ins().umulhi(dividend, multiplier);
            if reciprocal.correction {
                let difference = self.fb.ins().isub(dividend, quotient);
                let half = self.fb.ins().ushr_imm_s(difference, 1);
                quotient = self.fb.ins().iadd(quotient, half);
            }
            let quotient = self
                .fb
                .ins()
                .ushr_imm_s(quotient, i64::from(reciprocal.shift));
            self.quotients.insert(key, quotient);
            quotient
        };
        if matches!(op, Binary::Div) {
            quotient
        } else {
            let product = self.fb.ins().imul_imm_s(quotient, divisor as i64);
            self.fb.ins().isub(dividend, product)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reciprocal;

    #[test]
    fn reciprocal_matches_unsigned_division_at_boundaries_and_across_widths() {
        let divisors = (3u64..1000)
            .chain((2..64).flat_map(|shift| {
                let base = 1u64 << shift;
                [base - 1, base + 1]
            }))
            .chain([u64::MAX - 1, u64::MAX]);
        let mut state = 123456789u64;
        for divisor in divisors.filter(|d| !d.is_power_of_two()) {
            let reciprocal = Reciprocal::for_divisor(divisor);
            let boundary = (u64::MAX / divisor) * divisor;
            for dividend in [
                0,
                1,
                divisor - 1,
                divisor,
                divisor.saturating_add(1),
                boundary - 1,
                boundary,
                boundary.saturating_add(1),
                u64::MAX,
            ]
            .into_iter()
            .chain((0..32).map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            })) {
                let mut quotient =
                    ((u128::from(dividend) * u128::from(reciprocal.multiplier)) >> 64) as u64;
                if reciprocal.correction {
                    quotient += (dividend - quotient) >> 1;
                }
                assert_eq!(
                    quotient >> reciprocal.shift,
                    dividend / divisor,
                    "{dividend} / {divisor}"
                );
            }
        }
    }
}
