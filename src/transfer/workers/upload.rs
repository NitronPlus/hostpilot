use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
// PathBuf not required at top-level here; reference via std::path::Path when needed.
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use indicatif::ProgressBar;

// use classifier-aware retry helper from util; explicit import not required here

use super::{
    Throttler, WorkerCommonCtx, WorkerMetrics, finish_and_release_pb, maybe_create_file_pb,
    try_acquire_pb_slot,
};
use crate::MkdirError;
use crate::transfer::helpers::display_path;
use crate::transfer::session::ensure_worker_session;
use crate::transfer::sftp_like::SftpLike;
use crate::transfer::{EntryKind, FileEntry};

// ...existing code...

// RAII guard for connection token. Holds a Sender and returns the token on Drop
// or when release() is called. Defined at module scope so tests can access it.
struct ConnTokenGuard {
    tx: Option<Sender<()>>,
}

impl ConnTokenGuard {
    fn new(tx: Sender<()>) -> Self {
        Self { tx: Some(tx) }
    }

    fn release(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for ConnTokenGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

fn ensure_remote_dir_all_generic(
    sftp: &dyn SftpLike,
    dir_path: &std::path::Path,
) -> Result<(), MkdirError> {
    let mut accum = std::path::PathBuf::new();
    for comp in dir_path.components() {
        use std::path::Component;
        match comp {
            Component::RootDir => {
                accum.push(std::path::Path::new("/"));
            }
            Component::Prefix(_) => {
                // Windows 前缀在远端无意义，跳过
            }
            Component::CurDir => {}
            Component::ParentDir => {}
            Component::Normal(seg) => {
                accum.push(seg);
            }
        }
        let p = accum.as_path();
        if p.as_os_str().is_empty() {
            continue;
        }
        match sftp.stat_is_file(p) {
            Ok(is_file) => {
                if is_file {
                    return Err(MkdirError::ExistsAsFile(p.to_path_buf()));
                }
                // exists and is directory -> continue
            }
            Err(_) => {
                if let Err(e) = sftp.mkdir(p, 0o755) {
                    // Maybe concurrent create; re-check
                    match sftp.stat_is_file(p) {
                        Ok(is_file2) => {
                            if is_file2 {
                                return Err(MkdirError::ExistsAsFile(p.to_path_buf()));
                            }
                            // became directory -> success
                        }
                        Err(_) => {
                            return Err(MkdirError::SftpError(p.to_path_buf(), e.to_string()));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// Note: ensure_remote_dir_all wrapper removed; prefer calling ensure_remote_dir_all_generic
// with a boxed/borrowed SftpLike adapter.

// Helper to reset session and sftp state on transfer-level failures.
// Generic so tests can use stand-in types instead of real `ssh2` objects.
fn reset_session_and_sftp<S, F>(maybe_sess: &mut Option<S>, maybe_sftp: &mut Option<F>) {
    *maybe_sftp = None;
    *maybe_sess = None;
}

// Decide whether an error should trigger a full session reset (SSH session + SFTP).
// Strategy:
// - For explicit TransferError variants that indicate session/handshake/build problems,
//   return true.
// - For TransferError::WorkerIo, inspect the message for connection-related keywords.
// - Fallback: inspect the error string for the same connection keywords.
fn should_reset_session(err: &anyhow::Error) -> bool {
    // connection-level keywords (lowercase) to search for in messages
    const KEYWORDS: [&str; 6] = [
        "connection reset",
        "broken pipe",
        "connection aborted",
        "connection refused",
        "not connected",
        "eof",
    ];

    if let Some(te) = err.downcast_ref::<crate::TransferError>() {
        use crate::TransferError::*;
        match te {
            // clear session for session/build/creation failures
            SshSessionCreateFailed(_)
            | SshHandshakeFailed(_)
            | WorkerBuildSessionFailed(_)
            | SftpCreateFailed(_)
            | WorkerNoSftp(_)
            | WorkerNoSession(_) => return true,
            // auth failures are non-retriable by recreating session
            SshAuthFailed(_) => return false,
            // WorkerIo: inspect message
            WorkerIo(msg) => {
                let m = msg.to_lowercase();
                for kw in &KEYWORDS {
                    if m.contains(kw) {
                        return true;
                    }
                }
                return false;
            }
            _ => return false,
        }
    }

    // Fallback: string-match the anyhow error message
    let s = err.to_string().to_lowercase();
    for kw in &KEYWORDS {
        if s.contains(kw) {
            return true;
        }
    }
    false
}

// 兼容并更健壮的外部 API 名称（供其它模块调用）
pub(crate) fn sftp_mkdir_p(
    sftp: &dyn SftpLike,
    dir_path: &std::path::Path,
) -> Result<(), MkdirError> {
    // Retry + backoff wrapper around ensure_remote_dir_all to tolerate
    // transient SFTP errors and concurrent mkdir races.
    let mut attempt = 0u32;
    let max_attempts = 3u32;
    loop {
        match ensure_remote_dir_all_generic(sftp, dir_path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                // compute backoff using shared helper (attempt is 1-based)
                let base = crate::util::get_backoff_ms().max(50);
                let wait = crate::util::compute_backoff_ms(base, attempt as u64);
                std::thread::sleep(Duration::from_millis(wait));
                // retry
            }
        }
    }
}

pub(crate) struct UploadWorkersCtx {
    pub(crate) common: WorkerCommonCtx,
    pub(crate) rx: Receiver<FileEntry>,
    pub(crate) expanded_remote_base: String,
    pub(crate) conn_token_rx: Receiver<()>,
    pub(crate) conn_token_tx: Sender<()>,
    pub(crate) metrics_tx: Sender<WorkerMetrics>,
    // 最多仅允许 8 个可见文件进度条（通过槽位令牌实现）；不影响传输并发
    pub(crate) pb_slot_rx: Receiver<()>,
    pub(crate) pb_slot_tx: Sender<()>,
}

pub(crate) fn run_upload_workers(ctx: UploadWorkersCtx) {
    let UploadWorkersCtx {
        common,
        rx,
        expanded_remote_base,
        conn_token_rx,
        conn_token_tx,
        metrics_tx,
        pb_slot_rx,
        pb_slot_tx,
    } = ctx;
    let WorkerCommonCtx {
        workers,
        mp,
        total_pb,
        file_style,
        server,
        addr,
        max_retries,
        target_is_dir_final,
        failure_tx,
        buf_size,
    } = common;
    let mut handles = Vec::new();
    for worker_id in 0..workers {
        let rx = rx.clone();
        let pb = total_pb.clone();
        let mp = mp.clone();
        let file_style = file_style.clone();
        let server = server.clone();
        let expanded_remote_base = expanded_remote_base.clone();
        let failure_tx = failure_tx.clone();
        let conn_token_rx = conn_token_rx.clone();
        let conn_token_tx = conn_token_tx.clone();
        let addr = addr.clone();
        let metrics_tx_thread = metrics_tx.clone();
        let pb_slot_rx = pb_slot_rx.clone();
        let pb_slot_tx = pb_slot_tx.clone();
        let handle = std::thread::spawn(move || {
            let mut worker_pb: Option<ProgressBar> = None;
            // small stack buffer is fine for small sizes; use Vec only when buf_size > 8192
            // use a heap buffer sized to buf_size for simplicity and safety
            let mut buf = vec![0u8; buf_size];
            // per-worker cache of remote directories that have been created in this run
            let mut created_dirs: HashSet<String> = HashSet::new();
            let worker_start = Instant::now();
            let mut worker_bytes: u64 = 0;
            let mut maybe_sess: Option<ssh2::Session> = None;
            let mut maybe_sftp: Option<Box<dyn SftpLike>> = None;
            let mut token_guard: Option<ConnTokenGuard> = None;
            let mut session_rebuilds: u32 = 0;
            let mut sftp_rebuilds: u32 = 0;
            // Count consecutive connection-level errors; reset session only after threshold
            let mut connection_error_streak: u8 = 0;
            let mut has_pb_slot = false;
            while let Ok(entry) = rx.recv() {
                let FileEntry { rel, size, kind, local_full, .. } = entry;
                let remote_path_str = if expanded_remote_base.ends_with('/') || target_is_dir_final
                {
                    let base = expanded_remote_base.trim_end_matches('/');
                    // Ensure rel uses forward slashes when appended to remote base
                    let rel_unix = crate::transfer::helpers::normalize_path(&rel, true);
                    format!("{}/{}", base, rel_unix)
                } else {
                    expanded_remote_base.clone()
                };
                let remote_path = std::path::Path::new(&remote_path_str);

                let retry_ctx = format!("upload stream worker={} file={}", worker_id, rel);
                let transfer_res = crate::util::retry_operation_with_ctx(
                    max_retries,
                    || -> anyhow::Result<()> {
                        if maybe_sess.is_none() {
                            if token_guard.is_none() {
                                let _ = conn_token_rx.recv();
                                token_guard = Some(ConnTokenGuard::new(conn_token_tx.clone()));
                            }
                            if let Err(e) = ensure_worker_session(&mut maybe_sess, &server, &addr) {
                                tracing::debug!(
                                    "[ts][upload] worker_id={} ensure session failed: {:?}",
                                    worker_id,
                                    e
                                );
                                let _ = failure_tx.send(crate::TransferError::WorkerNoSession(
                                    server.alias.as_deref().unwrap_or("<unknown>").to_string(),
                                ));
                                if let Some(mut g) = token_guard.take() {
                                    g.release();
                                }
                                return Ok(());
                            } else if token_guard.is_some() {
                                // Handshake succeeded: release token immediately (limit only handshake concurrency)
                                if let Some(mut g) = token_guard.take() {
                                    g.release();
                                }
                                session_rebuilds += 1;
                                tracing::debug!(
                                    "[ts][upload] worker_id={} created session",
                                    worker_id
                                );
                            }
                        }

                        let sess = maybe_sess.as_mut().ok_or_else(|| -> anyhow::Error {
                            crate::TransferError::WorkerNoSession(
                                server.alias.clone().unwrap_or_else(|| "<unknown>".to_string()),
                            )
                            .into()
                        })?;
                        if maybe_sftp.is_none() {
                            match sess.sftp() {
                                Ok(s) => {
                                    maybe_sftp =
                                        Some(Box::new(crate::transfer::sftp_like::Ssh2Adapter(s)));
                                    sftp_rebuilds += 1;
                                    tracing::debug!(
                                        "[ts][upload] worker_id={} created SFTP",
                                        worker_id
                                    );
                                }
                                Err(e) => {
                                    return Err(crate::TransferError::SftpCreateFailed(format!(
                                        "{}",
                                        e
                                    ))
                                    .into());
                                }
                            }
                        }
                        let sftp_box = maybe_sftp.as_ref().ok_or_else(|| -> anyhow::Error {
                            crate::TransferError::WorkerNoSftp(
                                server.alias.clone().unwrap_or_else(|| "<unknown>".to_string()),
                            )
                            .into()
                        })?;
                        let sftp: &dyn SftpLike = sftp_box.as_ref();

                        if kind == EntryKind::Dir {
                            let rpath = remote_path;
                            let rstr = rpath.to_string_lossy().to_string();
                            if !created_dirs.contains(&rstr) {
                                match sftp_mkdir_p(sftp, rpath) {
                                    Ok(()) => {
                                        created_dirs.insert(rstr);
                                    }
                                    Err(e) => {
                                        let _ = failure_tx.send(
                                            crate::TransferError::CreateRemoteDirFailed(
                                                rstr.clone(),
                                                e.to_string(),
                                            ),
                                        );
                                    }
                                }
                            }
                            return Ok(());
                        }

                        // 文件：先确保父目录链存在
                        if let Some(parent) = remote_path.parent() {
                            let pstr = parent.to_string_lossy().to_string();
                            if !created_dirs.contains(&pstr) {
                                match sftp_mkdir_p(sftp, parent) {
                                    Ok(()) => {
                                        created_dirs.insert(pstr);
                                    }
                                    Err(e) => {
                                        return Err(crate::TransferError::CreateRemoteDirFailed(
                                            pstr.clone(),
                                            e.to_string(),
                                        )
                                        .into());
                                    }
                                }
                            }
                        }

                        if let Some(old) = worker_pb.take() {
                            old.finish_and_clear();
                        }
                        // 获取可见进度条槽位并按需创建文件级进度条
                        try_acquire_pb_slot(&pb_slot_rx, &mut has_pb_slot);
                        worker_pb = maybe_create_file_pb(
                            &mp,
                            &file_style,
                            size.unwrap_or(0),
                            &rel,
                            has_pb_slot,
                        );

                        let local_full = if let Some(ref lf) = local_full {
                            std::path::PathBuf::from(lf)
                        } else {
                            std::path::PathBuf::from(&rel)
                        };
                        let mut local_file =
                            File::open(&local_full).map_err(|e| -> anyhow::Error {
                                crate::TransferError::WorkerIo(format!(
                                    "本地打开失败: {} — {}",
                                    local_full.display(),
                                    e
                                ))
                                .into()
                            })?;
                        let mut remote_f =
                            sftp.create_write(remote_path).map_err(|e| -> anyhow::Error {
                                crate::TransferError::WorkerIo(format!(
                                    "远端创建文件失败: {} — {}",
                                    display_path(remote_path),
                                    e
                                ))
                                .into()
                            })?;
                        // Throttled progress updates
                        let mut throttler = Throttler::new();
                        loop {
                            match local_file.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    remote_f.write_all(&buf[..n]).map_err(
                                        |e| -> anyhow::Error {
                                            crate::TransferError::WorkerIo(format!(
                                                "远端写入失败: {} — {}",
                                                display_path(remote_path),
                                                e
                                            ))
                                            .into()
                                        },
                                    )?;
                                    worker_bytes += n as u64;
                                    throttler.tick(n as u64, worker_pb.as_ref(), &pb, None);
                                }
                                Err(e) => {
                                    return Err(crate::TransferError::WorkerIo(format!(
                                        "本地读取失败: {} — {}",
                                        local_full.display(),
                                        e
                                    ))
                                    .into());
                                }
                            }
                        }
                        // Flush remaining pending progress
                        throttler.flush(worker_pb.as_ref(), &pb, None);
                        finish_and_release_pb(&mut worker_pb, Some(&pb_slot_tx), &mut has_pb_slot);
                        Ok(())
                    },
                    crate::util::RetryPhase::DuringTransfer,
                    &retry_ctx,
                );

                if let Err(e) = transfer_res {
                    tracing::debug!(
                        "[ts][upload] transfer failed for {}: {}; reset SFTP for next try",
                        rel,
                        e
                    );
                    let _ = failure_tx.send(crate::TransferError::WorkerIo(format!(
                        "上传失败: {} — {}",
                        display_path(remote_path),
                        e
                    )));
                    // Conditionally reset session+SFTP based on error type/content.
                    // Only perform a full reset after two consecutive connection-level errors
                    if should_reset_session(&e) {
                        connection_error_streak = connection_error_streak.saturating_add(1);
                        if connection_error_streak >= 2 {
                            reset_session_and_sftp(&mut maybe_sess, &mut maybe_sftp);
                            connection_error_streak = 0;
                        }
                    } else {
                        // non-connection error -> clear streak
                        connection_error_streak = 0;
                    }
                }

                // Ensure token is never held beyond handshake scope
                if let Some(mut g) = token_guard.take() {
                    g.release();
                }
            }
            let elapsed = worker_start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let mb = worker_bytes as f64 / 1024.0 / 1024.0;
                tracing::info!(
                    "[ts][worker] upload avg_MBps={:.2} session_rebuilds={} sftp_rebuilds={}",
                    mb / elapsed,
                    session_rebuilds,
                    sftp_rebuilds
                );
            }
            let _ = metrics_tx_thread.send(WorkerMetrics {
                bytes: worker_bytes,
                session_rebuilds,
                sftp_rebuilds,
            });
        });
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
}

// Optional integration test: run only when env var HP_RUN_SFTP_TESTS=1
#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // Simple in-memory mock SFTP to exercise mkdir-p logic.
    struct MockSftp {
        // set of paths that are directories
        dirs: std::sync::Mutex<std::collections::HashSet<String>>,
        // set of paths that are files
        files: std::sync::Mutex<std::collections::HashSet<String>>,
        // optionally fail mkdir once for a given path
        fail_mkdir_once: std::sync::Mutex<std::collections::HashSet<String>>,
        // optionally make mkdir always fail for a given path (permission denied simulation)
        fail_mkdir_always: std::sync::Mutex<std::collections::HashSet<String>>,
    }

    impl MockSftp {
        fn new() -> Self {
            let mut dirs = std::collections::HashSet::new();
            dirs.insert("/".to_string());
            Self {
                dirs: std::sync::Mutex::new(dirs),
                files: std::sync::Mutex::new(std::collections::HashSet::new()),
                fail_mkdir_once: std::sync::Mutex::new(std::collections::HashSet::new()),
                fail_mkdir_always: std::sync::Mutex::new(std::collections::HashSet::new()),
            }
        }

        fn add_file(&self, p: &str) {
            let mut f = self.files.lock().unwrap();
            f.insert(p.to_string());
        }

        fn set_fail_mkdir_once(&self, p: &str) {
            let mut s = self.fail_mkdir_once.lock().unwrap();
            s.insert(p.to_string());
        }

        fn set_fail_mkdir_always(&self, p: &str) {
            let mut s = self.fail_mkdir_always.lock().unwrap();
            s.insert(p.to_string());
        }
    }

    impl SftpLike for MockSftp {
        fn stat_is_file(&self, p: &std::path::Path) -> Result<bool, String> {
            let mut s = p.to_string_lossy().to_string();
            s = crate::transfer::helpers::normalize_path(&s, true);
            if self.files.lock().unwrap().contains(&s) {
                return Ok(true);
            }
            if self.dirs.lock().unwrap().contains(&s) {
                return Ok(false);
            }
            Err("noent".to_string())
        }

        fn mkdir(&self, p: &std::path::Path, _mode: i32) -> Result<(), String> {
            let mut s = p.to_string_lossy().to_string();
            s = crate::transfer::helpers::normalize_path(&s, true);
            let always = self.fail_mkdir_always.lock().unwrap();
            if always.contains(&s) {
                return Err("simulated permission denied".to_string());
            }
            let mut once = self.fail_mkdir_once.lock().unwrap();
            if once.remove(&s) {
                // simulate race: other process created the dir after our mkdir failed
                self.dirs.lock().unwrap().insert(s.clone());
                return Err("simulated mkdir failure".to_string());
            }
            self.dirs.lock().unwrap().insert(s);
            Ok(())
        }

        fn open_read(&self, p: &std::path::Path) -> Result<Box<dyn std::io::Read + Send>, String> {
            let mut s = p.to_string_lossy().to_string();
            s = crate::transfer::helpers::normalize_path(&s, true);
            if self.files.lock().unwrap().contains(&s) {
                // Return an empty reader as a placeholder for tests that only open
                Ok(Box::new(std::io::Cursor::new(Vec::new())))
            } else {
                Err("noent".to_string())
            }
        }

        fn create_write(
            &self,
            _p: &std::path::Path,
        ) -> Result<Box<dyn std::io::Write + Send>, String> {
            // Return an in-memory writer for tests
            Ok(Box::new(std::io::Cursor::new(Vec::new())))
        }
    }

    #[test]
    fn conn_token_guard_drop_and_release() {
        use crossbeam_channel::bounded;
        // create a short-lived channel to observe token return
        let (tx, rx) = bounded::<()>(1);
        {
            // create guard and drop it to send token
            let g = ConnTokenGuard::new(tx.clone());
            drop(g);
        }
        // we should be able to recv the returned token
        assert!(rx.try_recv().is_ok());

        // test release() doesn't panic and is idempotent
        let (tx2, rx2) = bounded::<()>(1);
        let mut g2 = ConnTokenGuard::new(tx2.clone());
        g2.release();
        // release() already sent token
        assert!(rx2.try_recv().is_ok());
        // subsequent drop should be no-op
        drop(g2);
    }

    #[test]
    fn ensure_remote_dir_all_generic_creates_dirs() {
        let mock = MockSftp::new();
        // ensure /a/b/c gets created
        let p = std::path::Path::new("/a/b/c");
        let res = ensure_remote_dir_all_generic(&mock, p);
        assert!(res.is_ok());
        assert!(mock.dirs.lock().unwrap().contains(&"/a".to_string()));
        assert!(mock.dirs.lock().unwrap().contains(&"/a/b".to_string()));
        assert!(mock.dirs.lock().unwrap().contains(&"/a/b/c".to_string()));
    }

    #[test]
    fn ensure_remote_dir_all_generic_file_conflict() {
        let mock = MockSftp::new();
        // create a file at /a/b
        mock.add_file("/a/b");
        let p = std::path::Path::new("/a/b/c");
        let res = ensure_remote_dir_all_generic(&mock, p);
        assert!(matches!(res, Err(MkdirError::ExistsAsFile(_))));
    }

    #[test]
    fn ensure_remote_dir_all_generic_mkdir_race_then_ok() {
        let mock = MockSftp::new();
        // simulate mkdir failing once for /a then subsequent stat shows dir
        mock.set_fail_mkdir_once("/a");
        // make stat later return directory after failed mkdir simulation
        // Achieve by scheduling add_dir after first mkdir call (the mock removes fail flag)
        let p = std::path::Path::new("/a/b");
        let res = ensure_remote_dir_all_generic(&mock, p);
        assert!(res.is_ok());
        assert!(mock.dirs.lock().unwrap().contains(&"/a".to_string()));
        assert!(mock.dirs.lock().unwrap().contains(&"/a/b".to_string()));
    }

    #[test]
    fn optional_sftp_mkdir_p_integration() {
        if env::var("HP_RUN_SFTP_TESTS").unwrap_or_default() != "1" {
            eprintln!("Skipping sftp integration test (set HP_RUN_SFTP_TESTS=1 to enable)");
            return;
        }
        // Expect the following env vars for a test alias 'hdev':
        // HP_TEST_HDEV_HOST, HP_TEST_HDEV_USER, HP_TEST_HDEV_KEY (path to private key)
        let host = env::var("HP_TEST_HDEV_HOST").expect("HP_TEST_HDEV_HOST required");
        let user = env::var("HP_TEST_HDEV_USER").expect("HP_TEST_HDEV_USER required");
        let key = env::var("HP_TEST_HDEV_KEY").expect("HP_TEST_HDEV_KEY required");
        // Remote path to use
        let remote = env::var("HP_TEST_REMOTE_PATH").unwrap_or("~/.hp_bench".to_string());

        // Try to connect and run sftp mkdir_p — this mirrors ensure_worker_session usage but is simplified
        let tcp = std::net::TcpStream::connect((host.as_str(), 22)).expect("connect");
        let mut sess = ssh2::Session::new().unwrap();
        sess.set_tcp_stream(tcp);
        sess.handshake().expect("handshake");
        sess.userauth_pubkey_file(&user, None, std::path::Path::new(&key), None).expect("auth");
        let sftp = sess.sftp().expect("sftp");
        let rpath = if let Some(stripped) = remote.strip_prefix("~/") {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("HOME or USERPROFILE required for test");
            let mut pb = std::path::PathBuf::from(home);
            pb.push(stripped);
            pb.to_string_lossy().into_owned()
        } else if remote == "~" {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("HOME or USERPROFILE required for test")
                .to_string_lossy()
                .into_owned()
        } else {
            remote.clone()
        };
        let p = std::path::Path::new(&rpath);
        let adapter = crate::transfer::sftp_like::Ssh2Adapter(sftp);
        match sftp_mkdir_p(&adapter, p) {
            Ok(()) => println!("sftp_mkdir_p succeeded for {}", rpath),
            Err(e) => panic!("sftp_mkdir_p failed: {}", e),
        }
    }

    #[test]
    fn reset_session_and_sftp_clears_options() {
        // Use simple placeholder types to ensure helper compiles and clears slots
        let mut sess: Option<u32> = Some(42);
        let mut sftp: Option<&str> = Some("s");
        reset_session_and_sftp(&mut sess, &mut sftp);
        assert!(sess.is_none());
        assert!(sftp.is_none());
    }

    #[test]
    fn should_reset_on_session_errors() {
        let e = anyhow::Error::from(crate::TransferError::SshHandshakeFailed("x".to_string()));
        assert!(should_reset_session(&e));
        let e2 = anyhow::Error::from(crate::TransferError::WorkerNoSftp("a".to_string()));
        assert!(should_reset_session(&e2));
    }

    #[test]
    fn should_not_reset_on_auth_or_non_conn_errors() {
        let e = anyhow::Error::from(crate::TransferError::SshAuthFailed("x".to_string()));
        assert!(!should_reset_session(&e));
        let e2 = anyhow::Error::msg("some random io error");
        assert!(!should_reset_session(&e2));
    }

    #[test]
    fn should_reset_on_workerio_connection_phrases() {
        let e = anyhow::Error::from(crate::TransferError::WorkerIo(
            "connection reset by peer".to_string(),
        ));
        assert!(should_reset_session(&e));
        let e2 = anyhow::Error::msg("broken pipe");
        assert!(should_reset_session(&e2));
    }

    #[test]
    fn ensure_remote_dir_permission_denied() {
        let mock = MockSftp::new();
        // make mkdir always fail for /a to simulate permission denial
        mock.set_fail_mkdir_always("/a");
        let p = std::path::Path::new("/a/b");
        let res = ensure_remote_dir_all_generic(&mock, p);
        assert!(matches!(res, Err(MkdirError::SftpError(_, _))));
    }

    #[test]
    fn ensure_remote_dir_concurrent_race() {
        use std::sync::Arc;
        use std::thread;
        let mock = Arc::new(MockSftp::new());
        let p = std::path::Path::new("/concurrent/x/y");
        // spawn threads that concurrently ensure the same dir
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = mock.clone();
            handles.push(thread::spawn(move || ensure_remote_dir_all_generic(&*m, p)));
        }
        for h in handles {
            let r = h.join().expect("thread join");
            assert!(r.is_ok());
        }
        // final state should contain created directories
        assert!(mock.dirs.lock().unwrap().contains(&"/concurrent".to_string()));
        assert!(mock.dirs.lock().unwrap().contains(&"/concurrent/x".to_string()));
        assert!(mock.dirs.lock().unwrap().contains(&"/concurrent/x/y".to_string()));
    }

    #[test]
    fn mocksftp_root_idempotent() {
        let mock = MockSftp::new();
        // root already present; calling mkdir on root should be fine (no-op)
        let p = std::path::Path::new("/");
        assert!(mock.mkdir(p, 0o755).is_ok());
        assert!(mock.dirs.lock().unwrap().contains(&"/".to_string()));
    }

    #[test]
    fn mock_sftp_repeated_mkdir_is_ok() {
        let mock = MockSftp::new();
        let p = std::path::Path::new("/x/y");
        // first create
        assert!(ensure_remote_dir_all_generic(&mock, p).is_ok());
        // second create should be idempotent
        assert!(ensure_remote_dir_all_generic(&mock, p).is_ok());
    }

    #[test]
    fn upload_remote_write_failure_propagates_workerio() {
        use crate::transfer::workers::mock_io::FailingWriter;

        // TestSftp will implement SftpLike and return a FailingWriter for create_write
        struct TestSftp;

        impl SftpLike for TestSftp {
            fn stat_is_file(&self, _p: &std::path::Path) -> Result<bool, String> {
                Err("noent".to_string())
            }
            fn mkdir(&self, _p: &std::path::Path, _mode: i32) -> Result<(), String> {
                Ok(())
            }
            fn open_read(
                &self,
                _p: &std::path::Path,
            ) -> Result<Box<dyn std::io::Read + Send>, String> {
                Err("noread".to_string())
            }
            fn create_write(
                &self,
                _p: &std::path::Path,
            ) -> Result<Box<dyn std::io::Write + Send>, String> {
                Ok(Box::new(FailingWriter::new(0))) // fail immediately on first write
            }
        }

        // Simulate the upload write loop reading from an in-memory local cursor and writing to remote
        let local_data = b"hello world";
        let _local_cursor = std::io::Cursor::new(local_data.to_vec());
        let _buf: [u8; 8] = [0u8; 8];
        let s = TestSftp;
        let mut remote = s.create_write(std::path::Path::new("/tmp/x.txt")).expect("create write");
        // perform a single write attempt and ensure it errors
        let res = remote.write(&[1, 2, 3]);
        assert!(res.is_err());
    }

    #[test]
    fn transient_write_failure_then_retry_ok() {
        use crate::transfer::workers::mock_io::{FailingWriter, InMemoryWriter};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct TestSftp {
            calls: AtomicUsize,
        }

        impl TestSftp {
            fn new() -> Self {
                Self { calls: AtomicUsize::new(0) }
            }
        }

        impl SftpLike for TestSftp {
            fn stat_is_file(&self, _p: &std::path::Path) -> Result<bool, String> {
                Err("noent".to_string())
            }

            fn mkdir(&self, _p: &std::path::Path, _mode: i32) -> Result<(), String> {
                Ok(())
            }

            fn open_read(
                &self,
                _p: &std::path::Path,
            ) -> Result<Box<dyn std::io::Read + Send>, String> {
                Err("noread".to_string())
            }

            fn create_write(
                &self,
                _p: &std::path::Path,
            ) -> Result<Box<dyn std::io::Write + Send>, String> {
                // First create_write returns a writer that fails immediately on write
                // Subsequent create_write calls return a normal in-memory writer
                let prev = self.calls.fetch_add(1, Ordering::SeqCst);
                if prev == 0 {
                    Ok(Box::new(FailingWriter::new(0)))
                } else {
                    Ok(Box::new(InMemoryWriter::new()))
                }
            }
        }

        // Use the project's retry helper to simulate the worker transfer retry loop
        let s = TestSftp::new();
        let local_data = b"hello world".to_vec();

        let res = crate::util::retry_operation_with_ctx(
            3,
            || -> anyhow::Result<()> {
                let mut cursor = std::io::Cursor::new(local_data.clone());
                let mut buf = [0u8; 4];
                let mut remote =
                    s.create_write(std::path::Path::new("/tmp/x.txt")).map_err(|e| {
                        crate::TransferError::WorkerIo(format!("create_write failed: {}", e))
                    })?;
                loop {
                    match cursor.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            // use write_all so a FailingWriter::new(0) will cause an error
                            remote.write_all(&buf[..n]).map_err(|e| {
                                crate::TransferError::WorkerIo(format!(
                                    "remote write failed: {}",
                                    e
                                ))
                            })?;
                        }
                        Err(e) => {
                            return Err(crate::TransferError::WorkerIo(format!(
                                "local read failed: {}",
                                e
                            ))
                            .into());
                        }
                    }
                }
                Ok(())
            },
            crate::util::RetryPhase::DuringTransfer,
            "transient-test",
        );

        assert!(res.is_ok());
    }
}
