use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
// PathBuf not required at top-level here; reference via std::path::Path when needed.
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use indicatif::ProgressBar;

use crate::util::retry_operation;

use super::{
    Throttler, WorkerCommonCtx, WorkerMetrics, finish_and_release_pb, maybe_create_file_pb,
    try_acquire_pb_slot,
};
use crate::MkdirError;
use crate::transfer::helpers::display_path;
use crate::transfer::session::ensure_worker_session;
use crate::transfer::{EntryKind, FileEntry};

// ...existing code...

/// 递归确保远端目录存在（mkdir -p 语义）。
/// 对于每一级：存在且为目录 -> 跳过；存在且为文件 -> 报错；不存在 -> mkdir，若失败则复查 stat 再决定是否报错。
fn ensure_remote_dir_all(sftp: &ssh2::Sftp, dir_path: &std::path::Path) -> Result<(), MkdirError> {
    // 正常化：移除尾部分隔符
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
        match sftp.stat(p) {
            Ok(st) => {
                if st.is_file() {
                    return Err(MkdirError::ExistsAsFile(p.to_path_buf()));
                }
                // 目录已存在，继续下一层
            }
            Err(_) => {
                if let Err(e) = sftp.mkdir(p, 0o755) {
                    // 可能是并发下已被其他 worker 创建；复查一次
                    if let Ok(st2) = sftp.stat(p) {
                        if st2.is_file() {
                            return Err(MkdirError::ExistsAsFile(p.to_path_buf()));
                        }
                        // 已变为目录，当作成功
                    } else {
                        return Err(MkdirError::SftpError(p.to_path_buf(), format!("{}", e)));
                    }
                }
            }
        }
    }
    Ok(())
}

// 兼容并更健壮的外部 API 名称（供其它模块调用）
pub(crate) fn sftp_mkdir_p(
    sftp: &ssh2::Sftp,
    dir_path: &std::path::Path,
) -> Result<(), MkdirError> {
    // Retry + backoff wrapper around ensure_remote_dir_all to tolerate
    // transient SFTP errors and concurrent mkdir races.
    let mut attempt = 0u32;
    let max_attempts = 3u32;
    loop {
        match ensure_remote_dir_all(sftp, dir_path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                // small exponential backoff with jitter
                let backoff_ms = 50u64 * (1u64 << (attempt - 1));
                let jitter = (backoff_ms / 4).min(100);
                let now_nanos = std::time::Instant::now().elapsed().as_nanos();
                let sleep_ms = backoff_ms + (now_nanos as u64 % (jitter + 1));
                std::thread::sleep(Duration::from_millis(sleep_ms));
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
            let mut buf = vec![0u8; buf_size];
            // per-worker cache of remote directories that have been created in this run
            let mut created_dirs: HashSet<String> = HashSet::new();
            let worker_start = Instant::now();
            let mut worker_bytes: u64 = 0;
            let mut maybe_sess: Option<ssh2::Session> = None;
            let mut maybe_sftp: Option<ssh2::Sftp> = None;
            let mut has_token = false;
            let mut session_rebuilds: u32 = 0;
            let mut sftp_rebuilds: u32 = 0;
            let mut has_pb_slot = false;
            while let Ok(entry) = rx.recv() {
                let FileEntry { rel, size, kind, local_full, .. } = entry;
                let remote_path_str = if expanded_remote_base.ends_with('/') || target_is_dir_final
                {
                    let base = expanded_remote_base.trim_end_matches('/');
                    // Ensure rel uses forward slashes when appended to remote base
                    let rel_unix = rel.replace('\\', "/");
                    format!("{}/{}", base, rel_unix)
                } else {
                    expanded_remote_base.clone()
                };
                let remote_path = std::path::Path::new(&remote_path_str);

                let transfer_res = retry_operation(max_retries, || -> anyhow::Result<()> {
                    if maybe_sess.is_none() {
                        if !has_token {
                            let _ = conn_token_rx.recv();
                            has_token = true;
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
                            if has_token {
                                let _ = conn_token_tx.send(());
                                has_token = false;
                            }
                            return Ok(());
                        } else if has_token {
                            // Handshake succeeded: release token immediately (limit only handshake concurrency)
                            let _ = conn_token_tx.send(());
                            has_token = false;
                            session_rebuilds += 1;
                            tracing::debug!("[ts][upload] worker_id={} created session", worker_id);
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
                                maybe_sftp = Some(s);
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
                    let sftp = maybe_sftp.as_ref().ok_or_else(|| -> anyhow::Error {
                        crate::TransferError::WorkerNoSftp(
                            server.alias.clone().unwrap_or_else(|| "<unknown>".to_string()),
                        )
                        .into()
                    })?;

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
                    let mut local_file = File::open(&local_full).map_err(|e| -> anyhow::Error {
                        crate::TransferError::WorkerIo(format!(
                            "本地打开失败: {} — {}",
                            local_full.display(),
                            e
                        ))
                        .into()
                    })?;
                    let mut remote_f = sftp.create(remote_path).map_err(|e| -> anyhow::Error {
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
                                remote_f.write_all(&buf[..n]).map_err(|e| -> anyhow::Error {
                                    crate::TransferError::WorkerIo(format!(
                                        "远端写入失败: {} — {}",
                                        display_path(remote_path),
                                        e
                                    ))
                                    .into()
                                })?;
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
                });

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
                    // Drop SFTP to force recreation on next attempt/file
                    maybe_sftp = None;
                }

                // Ensure token is never held beyond handshake scope
                if has_token {
                    let _ = conn_token_tx.send(());
                    has_token = false;
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
        match sftp_mkdir_p(&sftp, p) {
            Ok(()) => println!("sftp_mkdir_p succeeded for {}", rpath),
            Err(e) => panic!("sftp_mkdir_p failed: {}", e),
        }
    }
}
