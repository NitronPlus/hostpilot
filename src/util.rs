use anyhow::Result;
use chrono::Utc;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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

/// Write failures to a file with a UTC timestamped header (append mode).
pub fn write_failures(path: Option<PathBuf>, failures: &[String]) {
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Open in append mode so we don't clobber previous runs
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
            // Write a simple header with UTC timestamp for this run
            let header =
                format!("Transfer failures (UTC {}):\n", Utc::now().format("%Y%m%dT%H%M%SZ"));
            let _ = writeln!(f, "{}", header);
            for line in failures {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
}

/// Write structured failures as JSON Lines alongside the plain-text failures.
/// For backward compatibility this function will also keep the existing plain-text file
/// (written by `write_failures`). The structured file will be named by appending
/// `.jsonl` to the provided path (if any).
pub fn write_failures_structured(path: Option<PathBuf>, failures: &[crate::TransferError]) {
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Text output kept by callers via write_failures; here we write a .jsonl alongside.
        let mut jsonl_path = p.clone();
        // Append `.jsonl` extension (e.g., failures.txt -> failures.txt.jsonl). This keeps it
        // non-destructive and clearly associated with the original file.
        let new_name = format!(
            "{}.jsonl",
            jsonl_path.file_name().and_then(|s| s.to_str()).unwrap_or("failures")
        );
        jsonl_path.set_file_name(new_name);

        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&jsonl_path) {
            for err in failures {
                // Build a simple structured object: variant + human message + optional fields
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
    }
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
    Err(last_err.unwrap_or_else(|| {
        crate::TransferError::OperationFailed("operation failed".to_string()).into()
    }))
}
