use crate::models::endpoint::RetryPolicy;
use rand::Rng;

/// Backoff delay in milliseconds for a 1-based `attempt`, with ±25% jitter,
/// clamped to `max_delay_ms`.
///
/// Shared by the retry path (where `attempt` stays under `max_attempts`) and the
/// long-running poll path (where it can reach `max_polls`, up to 100_000). The
/// growth term is therefore saturating and the exponent capped — without that,
/// `2^attempt` overflows and panics. Since the result is clamped to
/// `max_delay_ms` anyway, the cap is never observable.
pub fn compute_backoff_ms(
    shape: &str,
    initial_delay_ms: i64,
    max_delay_ms: i64,
    attempt: i64,
) -> i64 {
    let delay = match shape {
        "fixed" => initial_delay_ms,
        "linear" => initial_delay_ms.saturating_mul(attempt),
        // "exponential", and anything unrecognised
        _ => {
            // 1 << 62 already exceeds any sane max_delay_ms.
            let exp = (attempt - 1).clamp(0, 62) as u32;
            initial_delay_ms.saturating_mul(1_i64 << exp)
        }
    };

    // Add ±25% jitter
    let jitter_range = delay / 4;
    let jitter = if jitter_range > 0 {
        rand::thread_rng().gen_range(-jitter_range..=jitter_range)
    } else {
        0
    };

    delay.saturating_add(jitter).clamp(0, max_delay_ms.max(0))
}

pub fn compute_backoff(policy: &RetryPolicy, attempt: i64) -> i64 {
    compute_backoff_ms(
        &policy.backoff,
        policy.initial_delay_ms,
        policy.max_delay_ms,
        attempt,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Jitter is ±25%, so every case asserts a band rather than a point.
    #[test]
    fn shapes_scale_as_documented() {
        let cases = [
            ("fixed", 9, 750..=1250),        // flat
            ("linear", 4, 3000..=5000),      // 1000 * 4
            ("exponential", 3, 3000..=5000), // 1000 * 2^2
            ("nonsense", 3, 3000..=5000),    // unknown → exponential
        ];
        for (shape, attempt, expected) in cases {
            let d = compute_backoff_ms(shape, 1000, 60_000, attempt);
            assert!(expected.contains(&d), "{shape} attempt {attempt} gave {d}");
        }
        assert_eq!(
            compute_backoff_ms("exponential", 1000, 5_000, 30),
            5_000,
            "clamp"
        );
    }

    /// `max_polls` permits 100_000 polls; the previous `2_i64.pow(attempt - 1)`
    /// overflowed and panicked past 63. Keep the math saturating.
    #[test]
    fn long_sequence_saturates_instead_of_panicking() {
        assert_eq!(
            compute_backoff_ms("exponential", 1000, 60_000, 100_000),
            60_000
        );
        assert_eq!(compute_backoff_ms("linear", 1000, 60_000, i64::MAX), 60_000);
    }
}
