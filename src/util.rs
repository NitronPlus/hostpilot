use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
static RETRY_JSONL_DIR: OnceLock<PathBuf> = OnceLock::new();
use std::time::Duration;
use std::time::SystemTime;

/// Try to enable ANSI escape sequence support on Windows consoles.
/// Returns true if enabling succeeded (or platform likely already supports ANSI), false otherwise.
#[cfg(windows)]
pub fn try_enable_ansi_on_windows() -> bool {
    enable_ansi_support::enable_ansi_support().is_ok()
}

// On non-Windows platforms the crate is not required and ANSI support is typically available
// by default in terminals; provide a no-op fallback to avoid referencing the optional crate.
#[cfg(not(windows))]
pub fn try_enable_ansi_on_windows() -> bool {
    false
}

/// Convert a byte count into a human readable string using IEC units (KiB/MiB/GiB).
pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GiB", b / GB)
    } else if b >= MB {
        format!("{:.2} MiB", b / MB)
    } else if b >= KB {
        format!("{:.2} KiB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Initialize a MultiProgress and a total ProgressBar plus a header spinner ProgressBar.
/// The header bar is used to display a single-line startup summary above the total progress.
pub fn init_progress_and_mp(
    verbose: bool,
    total: u64,
    total_style: &ProgressStyle,
) -> (Arc<MultiProgress>, ProgressBar, ProgressBar) {
    let mp = Arc::new(if verbose {
        MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
    } else {
        MultiProgress::new()
    });
    // header bar used to show startup info above the total progress. Use {msg}
    let header = mp.add(ProgressBar::new_spinner());
    header.set_style(ProgressStyle::with_template("{msg}").expect("valid header template"));
    let total_pb = mp.add(ProgressBar::new(total));
    total_pb.set_style(total_style.clone());
    // attempt to enable ANSI on Windows (best-effort)
    let _ = try_enable_ansi_on_windows();
    (mp, total_pb, header)
}

/// Populate and set the startup header message above the total progress bar.
/// Fields are: Action, Worker, Backoff, Buf — each aligned and separated by 4 spaces.
pub fn set_startup_header(
    header: &ProgressBar,
    action: &str,
    worker_count: usize,
    backoff_ms: u64,
    buf_size: usize,
) {
    let buf_hr = human_bytes(buf_size as u64);
    let action_field = format!("{:<10}", format!("Action:{}", action));
    let conc_field = format!("{:<12}", format!("Worker:{}", worker_count));
    let backoff_field = format!("{:<12}", format!("Backoff:{}ms", backoff_ms));
    let buffer_field = format!("{:<12}", format!("Buf:{}", buf_hr));
    let mut header_msg_plain =
        format!("{}    {}    {}    {}", action_field, conc_field, backoff_field, buffer_field);
    if try_enable_ansi_on_windows() {
        let action_col = action_field.green();
        let conc_col = conc_field.cyan();
        let back_col = backoff_field.yellow();
        let buf_col = buffer_field.magenta();
        header_msg_plain = format!("{}    {}    {}    {}", action_col, conc_col, back_col, buf_col);
    }
    header.set_message(header_msg_plain);
}

/// Print a concise summary line for completed transfer and optionally write failures to disk.
pub fn print_summary(
    total_bytes: u64,
    elapsed_secs: f64,
    files: u64,
    session_rebuilds: u64,
    sftp_rebuilds: u64,
) {
    if elapsed_secs > 0.0 {
        let mb = total_bytes as f64 / 1024.0 / 1024.0;
        println!(
            "平均速率: {:.2} MB/s (传输 {} 字节, 耗时 {:.2} 秒, {} 文件) | 会话重建: {} | SFTP重建: {}",
            mb / elapsed_secs,
            total_bytes,
            elapsed_secs,
            files,
            session_rebuilds,
            sftp_rebuilds
        );
    } else {
        println!("平均速率: 0.00 MB/s (0 文件)");
    }
}

// Write failures to a file with a UTC timestamped header (append mode).
// removed plain-text & structured failure writers; use JSONL-only writer

/// Write failures in both plain-text and JSON Lines form using a single call.
/// This is a convenience wrapper used by higher-level helpers to produce both
/// human and machine-readable failure outputs alongside each other.
/// Write failures as JSON Lines only (append). This replaces the older
/// combined writer that also wrote plain-text; JSONL is easier for CI to
/// parse and therefore preferred.
///
/// Returns the final JSONL file path if writing occurred (Some), otherwise None.
pub fn write_failures_jsonl(
    path: Option<PathBuf>,
    failures: &[crate::TransferError],
) -> Option<PathBuf> {
    // Always write failures JSONL into the canonical logs directory located at
    // the config storage directory (where config.json lives) under `logs/`.
    // Do not accept external paths for the log location. If a filename was
    // provided via `path`, use its basename; otherwise generate a default
    // timestamped name.
    // Compute logs_dir from ~/.{pkgname}/ (ops::ensure_hostpilot_dir)
    let logs_dir =
        match dirs::home_dir().and_then(|home_dir| {
            match crate::ops::ensure_hostpilot_dir(&home_dir) {
                Ok(p) => Some(p.join("logs")),
                Err(_) => None,
            }
        }) {
            Some(d) => d,
            None => {
                let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                home.join(".".to_string() + env!("CARGO_PKG_NAME")).join("logs")
            }
        };
    let _ = std::fs::create_dir_all(&logs_dir);

    // Determine filename (basename) to use
    let filename = if let Some(p) = path {
        p.file_name().and_then(|s| s.to_str()).map(|s| s.to_string())
    } else {
        None
    };
    let jsonl_file_name = if let Some(name) = filename {
        let p = std::path::Path::new(&name);
        if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
            if ext.eq_ignore_ascii_case("jsonl") {
                name
            } else if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                format!("{}.jsonl", stem)
            } else {
                format!("{}.jsonl", name)
            }
        } else {
            format!("{}.jsonl", name)
        }
    } else {
        // Use a fixed filename for failures JSONL to allow simple append-only
        // behavior and easy discovery in CI/scripts. We intentionally avoid
        // timestamped names so callers can always look for a stable path
        // `<logs_dir>/failures.jsonl`.
        "failures.jsonl".to_string()
    };

    let jsonl_path = logs_dir.join(jsonl_file_name);

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&jsonl_path) {
        for err in failures {
            // Reuse existing structured mapping
            let obj = match err {
                crate::TransferError::InvalidDirection => {
                    serde_json::json!({"variant":"InvalidDirection","message":err.to_string()})
                }
                crate::TransferError::UnsupportedGlobUsage(s) => {
                    serde_json::json!({"variant":"UnsupportedGlobUsage","pattern":s,"message":err.to_string()})
                }
                crate::TransferError::AliasNotFound(a) => {
                    serde_json::json!({"variant":"AliasNotFound","alias":a,"message":err.to_string()})
                }
                crate::TransferError::RemoteTargetMustBeDir(p) => {
                    serde_json::json!({"variant":"RemoteTargetMustBeDir","path":p,"message":err.to_string()})
                }
                crate::TransferError::RemoteTargetParentMissing(p) => {
                    serde_json::json!({"variant":"RemoteTargetParentMissing","path":p,"message":err.to_string()})
                }
                crate::TransferError::CreateRemoteDirFailed(p, m) => {
                    serde_json::json!({"variant":"CreateRemoteDirFailed","path":p,"error":m,"message":err.to_string()})
                }
                crate::TransferError::LocalTargetMustBeDir(p) => {
                    serde_json::json!({"variant":"LocalTargetMustBeDir","path":p,"message":err.to_string()})
                }
                crate::TransferError::LocalTargetParentMissing(p) => {
                    serde_json::json!({"variant":"LocalTargetParentMissing","path":p,"message":err.to_string()})
                }
                crate::TransferError::CreateLocalDirFailed(p, m) => {
                    serde_json::json!({"variant":"CreateLocalDirFailed","path":p,"error":m,"message":err.to_string()})
                }
                crate::TransferError::GlobNoMatches(p) => {
                    serde_json::json!({"variant":"GlobNoMatches","pattern":p,"message":err.to_string()})
                }
                crate::TransferError::WorkerNoSession(a) => {
                    serde_json::json!({"variant":"WorkerNoSession","alias":a,"message":err.to_string()})
                }
                crate::TransferError::WorkerNoSftp(a) => {
                    serde_json::json!({"variant":"WorkerNoSftp","alias":a,"message":err.to_string()})
                }
                crate::TransferError::SftpCreateFailed(m) => {
                    serde_json::json!({"variant":"SftpCreateFailed","error":m,"message":err.to_string()})
                }
                crate::TransferError::SshNoAddress(a) => {
                    serde_json::json!({"variant":"SshNoAddress","addr":a,"message":err.to_string()})
                }
                crate::TransferError::SshSessionCreateFailed(a) => {
                    serde_json::json!({"variant":"SshSessionCreateFailed","addr":a,"message":err.to_string()})
                }
                crate::TransferError::SshHandshakeFailed(a) => {
                    serde_json::json!({"variant":"SshHandshakeFailed","addr":a,"message":err.to_string()})
                }
                crate::TransferError::SshAuthFailed(a) => {
                    serde_json::json!({"variant":"SshAuthFailed","addr":a,"message":err.to_string()})
                }
                crate::TransferError::WorkerBuildSessionFailed(a) => {
                    serde_json::json!({"variant":"WorkerBuildSessionFailed","addr":a,"message":err.to_string()})
                }
                crate::TransferError::MissingLocalSource(s) => {
                    serde_json::json!({"variant":"MissingLocalSource","message":err.to_string(),"detail":s})
                }
                crate::TransferError::DownloadMultipleRemoteSources(s) => {
                    serde_json::json!({"variant":"DownloadMultipleRemoteSources","message":err.to_string(),"detail":s})
                }
                crate::TransferError::OperationFailed(s) => {
                    serde_json::json!({"variant":"OperationFailed","message":s})
                }
                crate::TransferError::WorkerIo(s) => {
                    serde_json::json!({"variant":"WorkerIo","message":s})
                }
            };
            if let Ok(line) = serde_json::to_string(&obj) {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
    // record directory for retry attempts logging (one-time set)
    let _ = RETRY_JSONL_DIR.set(logs_dir.clone());
    Some(jsonl_path)
}

/// Initialize the retry attempts JSONL directory for early writes from retry logic.
///
/// This should be called once during startup (after creating the canonical logs
/// directory). It's a convenience to ensure `retry_operation_with_ctx` can append
/// retry attempts to `retry_attempts.jsonl` in the same directory as failures.
pub fn init_retry_jsonl_dir(dir: PathBuf) {
    let _ = RETRY_JSONL_DIR.set(dir);
}

// Default backoff base in milliseconds. Can be adjusted at runtime via `set_backoff_ms`.
static BACKOFF_BASE_MS: AtomicU64 = AtomicU64::new(100);

/// Set the base backoff in milliseconds used by `retry_operation` between attempts.
pub fn set_backoff_ms(ms: u64) {
    BACKOFF_BASE_MS.store(ms, Ordering::SeqCst);
}

/// Get the current base backoff in milliseconds used by `retry_operation`.
pub fn get_backoff_ms() -> u64 {
    BACKOFF_BASE_MS.load(Ordering::SeqCst)
}

/// Compute an exponential backoff delay (in milliseconds) for the given base and attempt number.
///
/// - `base_ms` is the configured base backoff in ms.
/// - `attempt_no` is a human-friendly attempt (1-based).
///
/// Uses exponential growth: base * 2^(attempt_no-1), saturating on overflow, and
/// applies a small time-derived jitter up to min(backoff/4, 100ms).
pub fn compute_backoff_ms(base_ms: u64, attempt_no: u64) -> u64 {
    let attempt_no = if attempt_no == 0 { 1 } else { attempt_no };
    // exponential: base * (2^(attempt_no-1)) but saturate on overflow
    let backoff: u64 = {
        let shift = attempt_no.saturating_sub(1);
        if shift >= 64 {
            u64::MAX
        } else {
            let mult = (1u128) << (shift as u32);
            let val = (base_ms as u128).saturating_mul(mult);
            if val > u64::MAX as u128 { u64::MAX } else { val as u64 }
        }
    };
    // small jitter: up to min(backoff/4, 100)
    let jitter_max = std::cmp::min(backoff / 4, 100);
    if jitter_max > 0
        && let Ok(dur) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
    {
        let nanos = dur.as_nanos() as u64;
        let jitter = nanos % (jitter_max + 1);
        return backoff.saturating_add(jitter);
    }
    backoff
}

/// Generic retry helper used by workers and tests.
/// `op` should return an anyhow::Result; helper will retry transient failures up to max_retries.
/// Retry phase indicating where the error occurred.
#[derive(Debug, Clone, Copy)]
pub enum RetryPhase {
    PreTransfer,
    DuringTransfer,
}

fn is_error_retriable_for_phase(e: &anyhow::Error, phase: RetryPhase) -> bool {
    if let Some(te) = e.downcast_ref::<crate::TransferError>() {
        match phase {
            RetryPhase::PreTransfer => te.is_retriable_pre_transfer(),
            RetryPhase::DuringTransfer => te.is_retriable_during_transfer(),
        }
    } else {
        // For non-TransferError (generic anyhow errors) be permissive during
        // active streaming transfers: these are often transient IO/network
        // errors produced by lower-level libraries. Keep PreTransfer
        // conservative to avoid retrying validation/auth errors.
        matches!(phase, RetryPhase::DuringTransfer)
    }
}

/// Retry helper with contextual logging. Emits per-attempt logs including phase, attempt index,
/// backoff duration, and a compact error summary. Uses the same phase-aware classifier as
/// `retry_operation`.
pub fn retry_operation_with_ctx<F, T>(
    max_retries: usize,
    mut op: F,
    phase: RetryPhase,
    ctx: &str,
) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..max_retries {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let should_retry = is_error_retriable_for_phase(&e, phase);
                let attempt_no = attempt + 1; // human-friendly
                if should_retry && attempt_no < max_retries {
                    let base = BACKOFF_BASE_MS.load(Ordering::SeqCst);
                    let wait = compute_backoff_ms(base, attempt_no as u64);
                    // try to write an attempt record to retry_attempts.jsonl in the
                    // same directory as failures JSONL if available. Non-fatal on error.
                    if let Some(dir) = RETRY_JSONL_DIR.get() {
                        let mut path = dir.clone();
                        path.push("retry_attempts.jsonl");
                        if let Ok(mut af) = OpenOptions::new().create(true).append(true).open(&path)
                        {
                            let attempt_obj = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "phase": format!("{:?}", phase),
                                "ctx": ctx,
                                "attempt": attempt_no,
                                "max_retries": max_retries,
                                "backoff_ms": wait,
                                "error": format!("{}", e)
                            });
                            if let Ok(line) = serde_json::to_string(&attempt_obj) {
                                let _ = writeln!(af, "{}", line);
                            }
                        }
                    }
                    tracing::warn!(
                        phase = ?phase,
                        ctx = %ctx,
                        attempt = attempt_no,
                        max_retries = max_retries,
                        backoff_ms = wait,
                        error = %e,
                        "retrying"
                    );
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(wait));
                    continue;
                } else {
                    if should_retry {
                        tracing::warn!(
                            phase = ?phase,
                            ctx = %ctx,
                            attempt = attempt_no,
                            max_retries = max_retries,
                            error = %e,
                            "giving up"
                        );
                    } else {
                        tracing::warn!(
                            phase = ?phase,
                            ctx = %ctx,
                            attempt = attempt_no,
                            max_retries = max_retries,
                            error = %e,
                            non_retriable = true,
                            "non-retriable"
                        );
                    }
                    last_err = Some(e);
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        crate::TransferError::OperationFailed("operation failed".to_string()).into()
    }))
}
