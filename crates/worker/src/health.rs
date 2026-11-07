use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

pub fn stale_after(poll_interval: Duration, floor: Duration) -> Duration {
    std::cmp::max(floor, poll_interval * 10)
}

pub fn poller_ok(last_tick_ago: Duration, stale_after: Duration, available_permits: usize) -> bool {
    last_tick_ago <= stale_after || available_permits == 0
}

pub struct WorkerHealth {
    started: Instant,
    last_tick_ms: AtomicU64,
    semaphore: Arc<Semaphore>,
    stale_after: Duration,
}

impl WorkerHealth {
    pub fn new(max_concurrent: usize, poll_interval: Duration, stale_floor: Duration) -> Self {
        Self {
            started: Instant::now(),
            last_tick_ms: AtomicU64::new(0),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            stale_after: stale_after(poll_interval, stale_floor),
        }
    }

    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn tick(&self) {
        self.last_tick_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn last_tick_ago(&self) -> Duration {
        Duration::from_millis(
            self.now_ms()
                .saturating_sub(self.last_tick_ms.load(Ordering::Relaxed)),
        )
    }

    pub fn poller_ok(&self) -> bool {
        poller_ok(
            self.last_tick_ago(),
            self.stale_after,
            self.semaphore.available_permits(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESH: Duration = Duration::from_millis(50);
    const THRESHOLD: Duration = Duration::from_secs(5);

    #[test]
    fn stale_after_is_ten_poll_intervals() {
        assert_eq!(
            stale_after(Duration::from_millis(2000), Duration::from_secs(5)),
            Duration::from_secs(20)
        );
    }

    #[test]
    fn stale_after_uses_the_configured_floor() {
        // The 200ms poll interval would otherwise yield a 2s threshold.
        assert_eq!(
            stale_after(Duration::from_millis(200), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn poller_ok_when_tick_is_fresh() {
        assert!(poller_ok(FRESH, THRESHOLD, 50));
    }

    #[test]
    fn poller_not_ok_when_tick_is_stale() {
        assert!(!poller_ok(Duration::from_secs(90), THRESHOLD, 50));
    }

    #[test]
    fn poller_ok_when_stale_but_at_capacity() {
        // Saturated, not wedged: every permit is held by an in-flight dispatch.
        assert!(poller_ok(Duration::from_secs(90), THRESHOLD, 0));
    }

    #[test]
    fn new_health_starts_fresh() {
        let health = WorkerHealth::new(1, Duration::from_millis(200), Duration::from_secs(5));
        assert!(health.last_tick_ago() < Duration::from_secs(5));
        assert!(health.poller_ok());
    }

    #[tokio::test]
    async fn health_is_ok_when_stale_but_every_permit_is_held() {
        let health = WorkerHealth::new(2, Duration::from_millis(1), Duration::from_secs(5));
        // The floor is 5s, so force staleness rather than sleeping it out.
        health.last_tick_ms.store(0, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(10));

        let _held = health.semaphore().acquire_many_owned(2).await.unwrap();
        assert_eq!(health.semaphore().available_permits(), 0);
        assert!(health.poller_ok());
    }

    #[test]
    fn tick_refreshes_last_tick() {
        let health = WorkerHealth::new(1, Duration::from_millis(200), Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
        let before = health.last_tick_ago();
        health.tick();
        assert!(health.last_tick_ago() < before);
    }
}
