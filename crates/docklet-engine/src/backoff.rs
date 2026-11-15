//! The pause before a stream that ended is opened again.

use std::time::Duration;

/// Waits that double from one second up to fifteen: 1, 2, 4, 8, 15, 15, ...
///
/// There is no wait of zero, so a loop that sleeps for `next_delay` between
/// its attempts cannot spin, whatever makes the attempts fail.
#[derive(Debug, Default)]
pub struct Backoff {
    attempts: u32,
}

impl Backoff {
    pub const SHORTEST: Duration = Duration::from_secs(1);
    pub const LONGEST: Duration = Duration::from_secs(15);

    /// The wait before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        let doubled = Self::SHORTEST.saturating_mul(1 << self.attempts.min(4));
        self.attempts = self.attempts.saturating_add(1);
        doubled.min(Self::LONGEST)
    }

    /// Starts over with the shortest wait, after an attempt that worked.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Sleeps for `next_delay`.
    pub async fn wait(&mut self) {
        tokio::time::sleep(self.next_delay()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(backoff: &mut Backoff, count: usize) -> Vec<u64> {
        (0..count).map(|_| backoff.next_delay().as_secs()).collect()
    }

    #[test]
    fn doubles_up_to_the_longest_wait() {
        let mut backoff = Backoff::default();
        assert_eq!(seconds(&mut backoff, 7), [1, 2, 4, 8, 15, 15, 15]);
    }

    #[test]
    fn never_waits_less_than_a_second() {
        let mut backoff = Backoff::default();
        for _ in 0..1000 {
            assert!(backoff.next_delay() >= Backoff::SHORTEST);
        }
        // Not even when the counter is as high as it gets.
        backoff.attempts = u32::MAX;
        assert_eq!(backoff.next_delay(), Backoff::LONGEST);
    }

    #[test]
    fn starts_over_after_a_reset() {
        let mut backoff = Backoff::default();
        seconds(&mut backoff, 5);
        backoff.reset();
        assert_eq!(seconds(&mut backoff, 2), [1, 2]);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_sleeps_for_the_delay() {
        let mut backoff = Backoff::default();
        let started = tokio::time::Instant::now();
        backoff.wait().await;
        backoff.wait().await;
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }
}
