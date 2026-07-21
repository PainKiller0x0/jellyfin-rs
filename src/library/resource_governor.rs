use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static FOREGROUND_BUSY_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

const FOREGROUND_GRACE: Duration = Duration::from_millis(1_500);
const SLOW_REQUEST_GRACE: Duration = Duration::from_secs(5);
const BACKGROUND_RECHECK: Duration = Duration::from_millis(40);

pub fn mark_foreground_activity() {
    extend_busy_window(FOREGROUND_GRACE);
}

pub fn report_request_latency(elapsed: Duration) {
    if elapsed >= Duration::from_millis(100) {
        extend_busy_window(SLOW_REQUEST_GRACE);
    }
}

pub fn foreground_is_busy() -> bool {
    now_millis() < FOREGROUND_BUSY_UNTIL_MS.load(Ordering::Relaxed)
}

pub async fn wait_for_background_worker(worker_index: usize, foreground_workers: usize) {
    while worker_index >= foreground_workers && foreground_is_busy() {
        tokio::time::sleep(BACKGROUND_RECHECK).await;
    }
}

pub async fn cooperative_scan_yield() {
    if foreground_is_busy() {
        tokio::time::sleep(Duration::from_millis(2)).await;
    } else {
        tokio::task::yield_now().await;
    }
}

fn extend_busy_window(duration: Duration) {
    let deadline = now_millis().saturating_add(duration.as_millis() as u64);
    let mut current = FOREGROUND_BUSY_UNTIL_MS.load(Ordering::Relaxed);
    while current < deadline {
        match FOREGROUND_BUSY_UNTIL_MS.compare_exchange_weak(
            current,
            deadline,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_activity_opens_busy_window() {
        FOREGROUND_BUSY_UNTIL_MS.store(0, Ordering::Relaxed);
        assert!(!foreground_is_busy());
        mark_foreground_activity();
        assert!(foreground_is_busy());
    }

    #[test]
    fn slow_request_extends_existing_window() {
        FOREGROUND_BUSY_UNTIL_MS.store(0, Ordering::Relaxed);
        report_request_latency(Duration::from_millis(100));
        let remaining = FOREGROUND_BUSY_UNTIL_MS
            .load(Ordering::Relaxed)
            .saturating_sub(now_millis());
        assert!(remaining >= 4_000);
    }
}
