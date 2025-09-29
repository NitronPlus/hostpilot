use std::fs::File;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use crossbeam_channel::Receiver;
use indicatif::ProgressBar;

use super::{
    Throttler, WorkerCommonCtx, WorkerMetrics, finish_and_release_pb, maybe_create_file_pb,
    try_acquire_pb_slot,
};
use crate::transfer::helpers::display_path;
use crate::transfer::multi_channel::{
    AcquireError as MultiChannelAcquireError, MultiChannelSftpManager,
};
use crate::transfer::session::ensure_worker_session;
use crate::transfer::{EntryKind, FileEntry};
// classifier-aware retry helper is used via crate::util::retry_operation_with_classifier

pub(crate) struct DownloadWorkersCtx {
    pub(crate) common: WorkerCommonCtx,
    pub(crate) file_rx: Receiver<FileEntry>,
    pub(crate) target: String,
    pub(crate) bytes_transferred: Arc<AtomicU64>,
    pub(crate) verbose: bool,
    pub(crate) metrics_tx: crossbeam_channel::Sender<WorkerMetrics>,
    // 最多仅允许 8 个可见文件进度条（通过槽位令牌实现）；不影响传输并发
    pub(crate) pb_slot_rx: crossbeam_channel::Receiver<()>,
    pub(crate) pb_slot_tx: crossbeam_channel::Sender<()>,
}

pub(crate) fn run_download_workers(ctx: DownloadWorkersCtx) -> Vec<std::thread::JoinHandle<()>> {
    let DownloadWorkersCtx {
        common,
        file_rx,
        target,
        bytes_transferred,
        verbose,
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
        sftp_channels_per_worker,
        buf_size,
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
        let pb_slot_rx = pb_slot_rx.clone();
        let pb_slot_tx = pb_slot_tx.clone();
        let handle = std::thread::spawn(move || {
            let mut worker_pb: Option<ProgressBar> = None;
            let mut buf = vec![0u8; buf_size];
            let mut maybe_session: Option<ssh2::Session> = None;
            let mut sftp_manager: Option<MultiChannelSftpManager> = None;
            let mut session_rebuilds: u32 = 0;
            let mut sftp_rebuilds: u32 = 0;
            let worker_start = Instant::now();
            let mut worker_bytes: u64 = 0;
            let mut has_pb_slot = false;
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
                            let _ =
                                failure_tx.send(crate::TransferError::LocalTargetParentMissing(
                                    parent.display().to_string(),
                                ));
                            if let Some(fpb) = worker_pb.take() {
                                fpb.finish_and_clear();
                            }
                            continue;
                        }
                    } else {
                        let _ = failure_tx.send(crate::TransferError::LocalTargetParentMissing(
                            parent.display().to_string(),
                        ));
                        if let Some(fpb) = worker_pb.take() {
                            fpb.finish_and_clear();
                        }
                        continue;
                    }
                }

                if entry.kind == EntryKind::Dir {
                    if local_target.exists() {
                        if !local_target.is_dir() {
                            let _ = failure_tx.send(crate::TransferError::LocalTargetMustBeDir(
                                local_target.display().to_string(),
                            ));
                        }
                    } else if let Some(parent) = local_target.parent() {
                        if parent.exists() && parent.is_dir() {
                            if let Err(e) = std::fs::create_dir(&local_target) {
                                let _ =
                                    failure_tx.send(crate::TransferError::CreateLocalDirFailed(
                                        local_target.display().to_string(),
                                        e.to_string(),
                                    ));
                            }
                        } else {
                            let _ =
                                failure_tx.send(crate::TransferError::LocalTargetParentMissing(
                                    local_target.display().to_string(),
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
                try_acquire_pb_slot(&pb_slot_rx, &mut has_pb_slot);
                worker_pb = maybe_create_file_pb(
                    &mp,
                    &file_style,
                    entry.size.unwrap_or(0),
                    &rel,
                    has_pb_slot,
                );

                // Pre-transfer: ensure session and SFTP manager, separate retry phase
                let pre_ctx = format!("download pre-transfer worker={} file={}", worker_id, rel);
                if let Err(e) = crate::util::retry_operation_with_ctx(
                    max_retries,
                    || -> anyhow::Result<()> {
                        if sftp_manager.is_none() {
                            if maybe_session.is_none() {
                                match ensure_worker_session(&mut maybe_session, &server, &addr) {
                                    Ok(()) => {
                                        session_rebuilds += 1;
                                        tracing::debug!(
                                            "[ts][download] worker_id={} created session",
                                            worker_id
                                        );
                                    }
                                    Err(_e) => {
                                        return Err(crate::TransferError::WorkerNoSession(
                                            server
                                                .alias
                                                .clone()
                                                .unwrap_or_else(|| "<unknown>".to_string()),
                                        )
                                        .into());
                                    }
                                }
                            }
                            if sftp_manager.is_none() {
                                if let Some(session) = maybe_session.take() {
                                    sftp_manager = Some(MultiChannelSftpManager::new(
                                        session,
                                        sftp_channels_per_worker,
                                    ));
                                } else {
                                    return Err(crate::TransferError::WorkerNoSession(
                                        server
                                            .alias
                                            .clone()
                                            .unwrap_or_else(|| "<unknown>".to_string()),
                                    )
                                    .into());
                                }
                            }
                        }
                        Ok(())
                    },
                    crate::util::RetryPhase::PreTransfer,
                    &pre_ctx,
                ) {
                    tracing::debug!("[ts][download] pre-transfer failed for {}: {}", rel, e);
                    let _ = failure_tx.send(crate::TransferError::WorkerIo(format!(
                        "pre-transfer failed: {} — {}",
                        remote_full, e
                    )));
                    // reset state for next file
                    sftp_manager = None;
                    maybe_session = None;
                    finish_and_release_pb(&mut worker_pb, Some(&pb_slot_tx), &mut has_pb_slot);
                    continue;
                }

                // Streaming transfer with DuringTransfer policy
                let transfer_res = crate::util::retry_operation_with_ctx(
                    max_retries,
                    || -> anyhow::Result<()> {
                        let channel_result = {
                            let manager_ref =
                                sftp_manager.as_mut().ok_or_else(|| -> anyhow::Error {
                                    crate::TransferError::WorkerNoSftp(
                                        server
                                            .alias
                                            .clone()
                                            .unwrap_or_else(|| "<unknown>".to_string()),
                                    )
                                    .into()
                                })?;

                            manager_ref.acquire()
                        };

                        let channel_guard = match channel_result {
                            Ok(guard) => guard,
                            Err(MultiChannelAcquireError::NoCapacity) => {
                                return Err(crate::TransferError::WorkerNoSftp(
                                    server.alias.clone().unwrap_or_else(|| "<unknown>".to_string()),
                                )
                                .into());
                            }
                            Err(MultiChannelAcquireError::Create(err)) => {
                                return Err(crate::TransferError::SftpCreateFailed(format!(
                                    "{}",
                                    err
                                ))
                                .into());
                            }
                        };

                        if channel_guard.was_fresh() {
                            sftp_rebuilds += 1;
                            tracing::debug!(
                                "[ts][download] worker_id={} created SFTP channel",
                                worker_id
                            );
                        }

                        let sftp_adapter = channel_guard.adapter();
                        let sftp: &dyn crate::transfer::sftp_like::SftpLike = sftp_adapter;
                        let remote_path = std::path::Path::new(&remote_full);
                        let mut remote_f =
                            sftp.open_read(remote_path).map_err(|e| -> anyhow::Error {
                                crate::TransferError::WorkerIo(format!("remote open failed: {}", e))
                                    .into()
                            })?;
                        let parent =
                            local_target.parent().unwrap_or_else(|| std::path::Path::new("."));
                        let tmp_name = format!("{}.hp.part.{}", file_name, std::process::id());
                        let tmp_path = parent.join(tmp_name);
                        let mut local_f =
                            File::create(&tmp_path).map_err(|e| -> anyhow::Error {
                                crate::TransferError::WorkerIo(format!(
                                    "local create failed: {}",
                                    e
                                ))
                                .into()
                            })?;
                        // Throttled progress updates (shared helper)
                        let mut throttler = Throttler::new();
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
                                        return Err(crate::TransferError::WorkerIo(format!(
                                            "local write failed: {}",
                                            e
                                        ))
                                        .into());
                                    }
                                    worker_bytes += n as u64;
                                    throttler.tick(
                                        n as u64,
                                        worker_pb.as_ref(),
                                        &total_pb,
                                        Some(&bytes_transferred),
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "[ts][download] remote read error for {}: {:?}",
                                        display_path(remote_path),
                                        e
                                    );
                                    let _ = std::fs::remove_file(&tmp_path);
                                    return Err(crate::TransferError::WorkerIo(format!(
                                        "remote read failed: {}",
                                        e
                                    ))
                                    .into());
                                }
                            }
                        }
                        // Flush remaining progress
                        throttler.flush(worker_pb.as_ref(), &total_pb, Some(&bytes_transferred));
                        if let Err(e) = local_f.sync_all() {
                            tracing::debug!(
                                "[ts][download] sync error for {}: {:?}",
                                tmp_path.display(),
                                e
                            );
                            let _ = std::fs::remove_file(&tmp_path);
                            return Err(crate::TransferError::WorkerIo(format!(
                                "local sync failed: {}",
                                e
                            ))
                            .into());
                        }
                        drop(local_f);
                        // Use helper to perform atomic rename with platform-aware retries
                        match atomic_rename_with_retries(&tmp_path, &local_target) {
                            Ok(()) => {}
                            Err(e) => {
                                tracing::debug!(
                                    "[ts][download] rename temp {} -> {} failed: {:?}",
                                    tmp_path.display(),
                                    local_target.display(),
                                    e
                                );
                                let _ = std::fs::remove_file(&tmp_path);
                                return Err(crate::TransferError::WorkerIo(format!(
                                    "rename failed: {}",
                                    e
                                ))
                                .into());
                            }
                        }
                        Ok(())
                    },
                    crate::util::RetryPhase::DuringTransfer,
                    &format!("download stream worker={} file={}", worker_id, rel),
                );

                if let Err(e) = transfer_res {
                    tracing::debug!(
                        "[ts][download] transfer failed for {}: {}; reset SFTP for next try",
                        rel,
                        e
                    );
                    let _ = failure_tx.send(crate::TransferError::WorkerIo(format!(
                        "download failed: {} — {}",
                        remote_full, e
                    )));
                    // Drop SFTP to force recreation on next attempt/file
                    if let Some(mgr) = sftp_manager.as_mut() {
                        mgr.reset();
                    }
                }

                finish_and_release_pb(&mut worker_pb, Some(&pb_slot_tx), &mut has_pb_slot);
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

// Test helper: copy from a reader into a tmp file, on any read/write error remove the tmp
// and return the io::Error. Mirrors the worker's behavior on remote read or local write failure.
#[cfg(test)]
pub(crate) fn copy_stream_with_cleanup<R: std::io::Read>(
    mut reader: R,
    tmp_path: &std::path::Path,
    buf_size: usize,
) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut local_f = std::fs::File::create(tmp_path)?;
    let mut buf = vec![0u8; buf_size];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = local_f.write_all(&buf[..n]) {
                    let _ = std::fs::remove_file(tmp_path);
                    return Err(e);
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(tmp_path);
                return Err(e);
            }
        }
    }
    // flush/sync
    if let Err(e) = local_f.sync_all() {
        let _ = std::fs::remove_file(tmp_path);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use crate::transfer::workers::mock_io::PartialReader;

    // using PartialReader from mock_io

    fn make_tmp_dir() -> std::path::PathBuf {
        let mut base = std::env::temp_dir();
        let uniq = format!(
            "hp_dl_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        base.push(uniq);
        std::fs::create_dir(&base).expect("create tmp dir");
        base
    }

    #[test]
    fn partial_read_removes_tmp_on_error() {
        let dir = make_tmp_dir();
        let tmp = dir.join("partial.tmp");
        let data = b"hello world";
        // fail after 1 successful read (so there will be partial data)
        let mut r = PartialReader::new(data, 1);
        let res = copy_stream_with_cleanup(&mut r, &tmp, 4);
        assert!(res.is_err());
        // tmp should have been removed by cleanup
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_read_removes_tmp_on_error_via_mock_io() {
        let dir = make_tmp_dir();
        let tmp = dir.join("partial2.tmp");
        let data = b"hello world";
        let mut r = PartialReader::new(data, 1);
        let res = copy_stream_with_cleanup(&mut r, &tmp, 4);
        assert!(res.is_err());
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Attempt to atomically rename `tmp_path` to `local_target`, retrying a few times
/// if the target already exists or is temporarily permission-denied (Windows semantics).
///
/// Returns Ok(()) on success, otherwise returns the final io::Error from rename.
pub(crate) fn atomic_rename_with_retries(
    tmp_path: &std::path::Path,
    local_target: &std::path::Path,
) -> Result<(), std::io::Error> {
    use std::time::Duration;
    let mut attempts = 0;
    loop {
        match std::fs::rename(tmp_path, local_target) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let kind = e.kind();
                if attempts < 2
                    && (kind == std::io::ErrorKind::AlreadyExists
                        || kind == std::io::ErrorKind::PermissionDenied)
                {
                    // try deleting target and retry
                    let _ = std::fs::remove_file(local_target);
                    std::thread::sleep(Duration::from_millis(50));
                    attempts += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, read_to_string};
    use std::io::Write;

    fn make_tmp_dir() -> std::path::PathBuf {
        let mut base = std::env::temp_dir();
        let uniq = format!(
            "hostpilot_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        base.push(uniq);
        std::fs::create_dir(&base).expect("create temp dir");
        base
    }

    #[test]
    fn atomic_rename_no_target() {
        let dir = make_tmp_dir();
        let tmp = dir.join("a.tmp");
        let target = dir.join("a.txt");
        let mut f = File::create(&tmp).expect("create tmp");
        writeln!(f, "hello tmp").expect("write tmp");
        drop(f);
        assert!(!target.exists());
        atomic_rename_with_retries(&tmp, &target).expect("rename should succeed");
        assert!(target.exists());
        let content = read_to_string(&target).expect("read target");
        assert!(content.contains("hello tmp"));
        // cleanup
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn atomic_rename_overwrite_existing() {
        let dir = make_tmp_dir();
        let tmp = dir.join("b.tmp");
        let target = dir.join("b.txt");
        // create existing target
        let mut t = File::create(&target).expect("create target");
        writeln!(t, "old").expect("write old");
        drop(t);
        // create tmp
        let mut f = File::create(&tmp).expect("create tmp");
        writeln!(f, "new content").expect("write tmp");
        drop(f);
        // call rename helper
        atomic_rename_with_retries(&tmp, &target).expect("rename should succeed");
        assert!(target.exists());
        let content = read_to_string(&target).expect("read target");
        assert!(content.contains("new content"));
        // ensure tmp removed
        assert!(!tmp.exists());
        // cleanup
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir(&dir);
    }
}
