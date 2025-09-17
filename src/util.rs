use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// Default backoff base in milliseconds. Can be adjusted at runtime via `set_backoff_ms`.
static BACKOFF_BASE_MS: AtomicU64 = AtomicU64::new(100);

/// Set the base backoff in milliseconds used by `retry_operation` between attempts.
pub fn set_backoff_ms(ms: u64) {
    BACKOFF_BASE_MS.store(ms, Ordering::SeqCst);
}

/// Generic retry helper used by workers and tests.
/// `op` should return an anyhow::Result; helper will retry transient failures up to max_retries.
pub fn retry_operation<F, T>(max_retries: usize, mut op: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..max_retries {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < max_retries {
                    let base = BACKOFF_BASE_MS.load(Ordering::SeqCst);
                    let wait = base.saturating_mul(attempt as u64 + 1);
                    std::thread::sleep(Duration::from_millis(wait));
                    continue;
                } else {
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("operation failed")))
}
