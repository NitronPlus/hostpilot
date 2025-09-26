use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hostpilot::TransferError;
use hostpilot::util::{RetryPhase, init_retry_jsonl_dir, retry_operation_with_ctx, set_backoff_ms};

#[test]
fn retry_attempts_jsonl_written_non_verbose() {
    // create a unique temporary directory under system temp
    let mut tmp = std::env::temp_dir();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    tmp.push(format!("hp_test_retry_{}", nanos));
    std::fs::create_dir_all(&tmp).expect("create tmp dir");

    // register the retry jsonl dir early (simulates main initializing the dir in non-verbose runs)
    init_retry_jsonl_dir(tmp.clone());
    let path = tmp.join("retry_attempts.jsonl");
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }

    // make backoff tiny to keep test fast
    set_backoff_ms(1);

    // op will fail twice with a retriable error, then succeed
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();

    let res = retry_operation_with_ctx(
        4,
        || -> anyhow::Result<()> {
            let n = calls2.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(TransferError::WorkerIo("simulated transient".to_string()).into())
            } else {
                Ok(())
            }
        },
        RetryPhase::DuringTransfer,
        "test-retry",
    );

    assert!(res.is_ok(), "operation should eventually succeed");

    // ensure retry_attempts.jsonl was created and contains attempt records
    assert!(path.exists(), "retry_attempts.jsonl should exist at {:?}", path);
    let contents = std::fs::read_to_string(&path).expect("read attempts file");
    assert!(contents.contains("\"attempt\""), "attempt field missing in attempts file");

    // cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&tmp);
}
