use std::fs::File;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use indicatif::ProgressBar;

use super::{WorkerCommonCtx, WorkerMetrics};
use crate::transfer::helpers::display_path;
use crate::transfer::session::ensure_worker_session;
use crate::transfer::{EntryKind, FileEntry, WORKER_BUF_SIZE};
use crate::util::retry_operation;

pub(crate) struct DownloadWorkersCtx {
    pub(crate) common: WorkerCommonCtx,
    pub(crate) file_rx: Receiver<FileEntry>,
    pub(crate) target: String,
    pub(crate) bytes_transferred: Arc<AtomicU64>,
    pub(crate) verbose: bool,
    pub(crate) metrics_tx: crossbeam_channel::Sender<WorkerMetrics>,
}

pub(crate) fn run_download_workers(ctx: DownloadWorkersCtx) -> Vec<std::thread::JoinHandle<()>> {
    let DownloadWorkersCtx { common, file_rx, target, bytes_transferred, verbose, metrics_tx } =
        ctx;
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
    } = common;
    let mut handles = Vec::new();
    for worker_id in 0..workers {
        let file_rx = file_rx.clone();
        let mp = mp.clone();
        let total_pb = total_pb.clone();
        let file_style = file_style.clone();
        let server = server.clone();
        let target = target.clone();
        let failure_tx = failure_tx.clone();
        let bytes_transferred = bytes_transferred.clone();
        let addr = addr.clone();
        let metrics_tx_thread = metrics_tx.clone();
        let handle = std::thread::spawn(move || {
            let mut worker_pb: Option<ProgressBar> = None;
            let mut buf = vec![0u8; WORKER_BUF_SIZE];
            let mut maybe_sess: Option<ssh2::Session> = None;
            let mut maybe_sftp: Option<ssh2::Sftp> = None;
            let mut session_rebuilds: u32 = 0;
            let mut sftp_rebuilds: u32 = 0;
            let worker_start = Instant::now();
            let mut worker_bytes: u64 = 0;
            while let Ok(entry) = file_rx.recv() {
                tracing::debug!(
                    "[ts][download] worker_id={} received entry {}",
                    worker_id,
                    entry.rel
                );
                let remote_full = entry.remote_full;
                let rel = entry.rel;
                let file_name = std::path::Path::new(&rel)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(rel.as_str());
                let local_target = if target_is_dir_final {
                    std::path::Path::new(&target).join(&rel)
                } else {
                    std::path::Path::new(&target).to_path_buf()
                };
                if let Some(parent) = local_target.parent()
                    && !parent.exists()
                {
                    if let Some(pp) = parent.parent() {
                        if pp.exists() && pp.is_dir() {
                            let _ = std::fs::create_dir(parent);
                        } else {
                            let _ = failure_tx.send(format!(
                                "无法创建父目录（缺少上级）: {} (本地)",
                                parent.display()
                            ));
                            if let Some(fpb) = worker_pb.take() {
                                fpb.finish_and_clear();
                            }
                            continue;
                        }
                    } else {
                        let _ =
                            failure_tx.send(format!("无法创建父目录: {} (本地)", parent.display()));
                        if let Some(fpb) = worker_pb.take() {
                            fpb.finish_and_clear();
                        }
                        continue;
                    }
                }

                if entry.kind == EntryKind::Dir {
                    if local_target.exists() {
                        if !local_target.is_dir() {
                            let _ = failure_tx.send(format!(
                                "期望是目录但存在同名文件: {} (本地)",
                                local_target.display()
                            ));
                        }
                    } else if let Some(parent) = local_target.parent() {
                        if parent.exists() && parent.is_dir() {
                            if let Err(e) = std::fs::create_dir(&local_target) {
                                let _ = failure_tx.send(format!(
                                    "创建目录失败: {} (本地) — {}",
                                    local_target.display(),
                                    e
                                ));
                            }
                        } else {
                            let _ = failure_tx.send(format!(
                                "目录的父目录不存在: {} (本地)",
                                local_target.display()
                            ));
                        }
                    }
                    if let Some(fpb) = worker_pb.take() {
                        fpb.finish_and_clear();
                    }
                    continue;
                }
                if let Some(old) = worker_pb.take() {
                    old.finish_and_clear();
                }
                let file_size = entry.size.unwrap_or(0);
                let file_pb = mp.add(ProgressBar::new(file_size));
                file_pb.set_style(file_style.clone());
                file_pb.set_message(rel.clone());
                worker_pb = Some(file_pb.clone());

                let transfer_res = retry_operation(max_retries, || -> anyhow::Result<()> {
                    if maybe_sess.is_none() {
                        match ensure_worker_session(&mut maybe_sess, &server, &addr) {
                            Ok(()) => {
                                session_rebuilds += 1;
                                tracing::debug!(
                                    "[ts][download] worker_id={} created session",
                                    worker_id
                                );
                            }
                            Err(e) => {
                                return Err(anyhow::anyhow!(format!(
                                    "failed to build session: {}",
                                    e
                                )));
                            }
                        }
                    }
                    let sess = maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
                    if maybe_sftp.is_none() {
                        match sess.sftp() {
                            Ok(s) => {
                                tracing::debug!(
                                    "[ts][download] worker_id={} created SFTP",
                                    worker_id
                                );
                                sftp_rebuilds += 1;
                                maybe_sftp = Some(s);
                            }
                            Err(e) => {
                                return Err(anyhow::anyhow!(format!("sftp create failed: {}", e)));
                            }
                        }
                    }
                    let sftp = maybe_sftp.as_ref().ok_or_else(|| anyhow::anyhow!("no sftp"))?;
                    let remote_path = std::path::Path::new(&remote_full);
                    let mut remote_f = sftp
                        .open(remote_path)
                        .map_err(|e| anyhow::anyhow!("remote open failed: {}", e))?;
                    let parent = local_target.parent().unwrap_or_else(|| std::path::Path::new("."));
                    let tmp_name = format!("{}.hp.part.{}", file_name, std::process::id());
                    let tmp_path = parent.join(tmp_name);
                    let mut local_f = File::create(&tmp_path)
                        .map_err(|e| anyhow::anyhow!("local create failed: {}", e))?;
                    // Throttled progress updates
                    let mut pending: u64 = 0;
                    let mut last_flush = Instant::now();
                    loop {
                        match remote_f.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if let Err(e) = local_f.write_all(&buf[..n]) {
                                    tracing::debug!(
                                        "[ts][download] write error for {}: {:?}",
                                        tmp_path.display(),
                                        e
                                    );
                                    let _ = std::fs::remove_file(&tmp_path);
                                    return Err(anyhow::anyhow!("local write failed: {}", e));
                                }
                                pending += n as u64;
                                worker_bytes += n as u64;
                                if pending >= 64 * 1024
                                    || last_flush.elapsed() >= Duration::from_millis(50)
                                {
                                    if let Some(ref p) = worker_pb {
                                        p.inc(pending);
                                    }
                                    total_pb.inc(pending);
                                    bytes_transferred.fetch_add(pending, Ordering::SeqCst);
                                    pending = 0;
                                    last_flush = Instant::now();
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "[ts][download] remote read error for {}: {:?}",
                                    display_path(remote_path),
                                    e
                                );
                                let _ = std::fs::remove_file(&tmp_path);
                                return Err(anyhow::anyhow!("remote read failed: {}", e));
                            }
                        }
                    }
                    // Flush remaining progress
                    if pending > 0 {
                        if let Some(ref p) = worker_pb {
                            p.inc(pending);
                        }
                        total_pb.inc(pending);
                        bytes_transferred.fetch_add(pending, Ordering::SeqCst);
                    }
                    if let Err(e) = local_f.sync_all() {
                        tracing::debug!(
                            "[ts][download] sync error for {}: {:?}",
                            tmp_path.display(),
                            e
                        );
                        let _ = std::fs::remove_file(&tmp_path);
                        return Err(anyhow::anyhow!("local sync failed: {}", e));
                    }
                    drop(local_f);
                    let mut attempts = 0;
                    loop {
                        match std::fs::rename(&tmp_path, &local_target) {
                            Ok(()) => break,
                            Err(e) => {
                                // Windows 上如果目标已存在或被占用，尝试删除后重试一次
                                let kind = e.kind();
                                if attempts < 2
                                    && (kind == std::io::ErrorKind::AlreadyExists
                                        || kind == std::io::ErrorKind::PermissionDenied)
                                {
                                    let _ = std::fs::remove_file(&local_target);
                                    std::thread::sleep(Duration::from_millis(50));
                                    attempts += 1;
                                    continue;
                                }
                                tracing::debug!(
                                    "[ts][download] rename temp {} -> {} failed: {:?}",
                                    tmp_path.display(),
                                    local_target.display(),
                                    e
                                );
                                let _ = std::fs::remove_file(&tmp_path);
                                return Err(anyhow::anyhow!("rename failed: {}", e));
                            }
                        }
                    }
                    Ok(())
                });

                if let Err(e) = transfer_res {
                    tracing::debug!(
                        "[ts][download] transfer failed for {}: {}; reset SFTP for next try",
                        rel,
                        e
                    );
                    let _ = failure_tx.send(format!("download failed: {}", remote_full));
                    // Drop SFTP to force recreation on next attempt/file
                    maybe_sftp = None;
                }

                if let Some(fpb) = worker_pb.take() {
                    fpb.finish_and_clear();
                }
                if verbose {
                    tracing::debug!("[ts][download] finished {}", file_name);
                }
            }
            let elapsed = worker_start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let mb = worker_bytes as f64 / 1024.0 / 1024.0;
                tracing::info!("[ts][worker] download avg_MBps={:.2}", mb / elapsed);
            }
            let _ = metrics_tx_thread.send(WorkerMetrics {
                bytes: worker_bytes,
                session_rebuilds,
                sftp_rebuilds,
            });
        });
        handles.push(handle);
    }
    handles
}
