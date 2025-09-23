use std::fs::File;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use indicatif::ProgressBar;

use crate::util::retry_operation;

use super::WorkerCommonCtx;
use crate::transfer::session::ensure_worker_session;
use crate::transfer::{EntryKind, FileEntry, WORKER_BUF_SIZE};

pub(crate) struct UploadWorkersCtx {
    pub(crate) common: WorkerCommonCtx,
    pub(crate) rx: Receiver<FileEntry>,
    pub(crate) expanded_remote_base: String,
    pub(crate) conn_token_rx: Receiver<()>,
    pub(crate) conn_token_tx: Sender<()>,
}

pub(crate) fn run_upload_workers(ctx: UploadWorkersCtx) {
    let UploadWorkersCtx { common, rx, expanded_remote_base, conn_token_rx, conn_token_tx } = ctx;
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
        let handle = std::thread::spawn(move || {
            let mut worker_pb: Option<ProgressBar> = None;
            let mut buf = vec![0u8; WORKER_BUF_SIZE];
            let worker_start = Instant::now();
            let mut worker_bytes: u64 = 0;
            let mut maybe_sess: Option<ssh2::Session> = None;
            let mut maybe_sftp: Option<ssh2::Sftp> = None;
            let mut has_token = false;
            let mut session_rebuilds: u32 = 0;
            let mut sftp_rebuilds: u32 = 0;
            while let Ok(entry) = rx.recv() {
                let FileEntry { rel, size, kind, local_full, .. } = entry;
                let remote_full = if expanded_remote_base.ends_with('/') || target_is_dir_final {
                    std::path::Path::new(&expanded_remote_base).join(&rel)
                } else {
                    std::path::Path::new(&expanded_remote_base).to_path_buf()
                };
                let remote_str = remote_full.to_string_lossy().replace('\\', "/");

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
                            let _ = failure_tx.send(format!(
                                "认证失败: {} (远端)",
                                server.alias.as_deref().unwrap_or("<unknown>")
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

                    let sess = maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
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
                                return Err(anyhow::anyhow!(format!("sftp create failed: {}", e)));
                            }
                        }
                    }
                    let sftp = maybe_sftp.as_ref().ok_or_else(|| anyhow::anyhow!("no sftp"))?;

                    if kind == EntryKind::Dir {
                        let rpath = std::path::Path::new(&remote_str);
                        match sftp.stat(rpath) {
                            Ok(st) => {
                                if st.is_file() {
                                    let _ = failure_tx.send(format!(
                                        "远端已有同名文件（期望目录）: {}",
                                        remote_str
                                    ));
                                }
                            }
                            Err(_) => {
                                if let Some(parent) = rpath.parent() {
                                    if sftp.stat(parent).is_ok() {
                                        if let Err(e) = sftp.mkdir(rpath, 0o755) {
                                            let _ = failure_tx.send(format!(
                                                "创建远端目录失败: {} — {}",
                                                remote_str, e
                                            ));
                                        }
                                    } else {
                                        let _ = failure_tx.send(format!(
                                            "目录父目录不存在: {}",
                                            parent.to_string_lossy()
                                        ));
                                    }
                                }
                            }
                        }
                        return Ok(());
                    }

                    if let Some(parent) = std::path::Path::new(&remote_str).parent()
                        && sftp.stat(parent).is_err()
                    {
                        if let Some(pp) = parent.parent() {
                            if sftp.stat(pp).is_ok() {
                                let _ = sftp.mkdir(parent, 0o755);
                            } else {
                                return Err(anyhow::anyhow!(format!(
                                    "父目录不存在: {} (远端)",
                                    parent.to_string_lossy()
                                )));
                            }
                        } else {
                            return Err(anyhow::anyhow!(format!(
                                "无效父目录: {} (远端)",
                                parent.to_string_lossy()
                            )));
                        }
                    }

                    if let Some(old) = worker_pb.take() {
                        old.finish_and_clear();
                    }
                    let file_pb = mp.add(ProgressBar::new(size.unwrap_or(0)));
                    file_pb.set_style(file_style.clone());
                    file_pb.set_message(rel.clone());
                    worker_pb = Some(file_pb.clone());

                    let local_full = if let Some(ref lf) = local_full {
                        std::path::PathBuf::from(lf)
                    } else {
                        std::path::PathBuf::from(&rel)
                    };
                    let mut local_file = File::open(&local_full).map_err(|e| {
                        anyhow::anyhow!(format!("本地打开失败: {} — {}", local_full.display(), e))
                    })?;
                    let mut remote_f =
                        sftp.create(std::path::Path::new(&remote_str)).map_err(|e| {
                            anyhow::anyhow!(format!("远端创建文件失败: {} — {}", remote_str, e))
                        })?;
                    // Throttled progress updates
                    let mut pending: u64 = 0;
                    let mut last_flush = Instant::now();
                    loop {
                        match local_file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                remote_f.write_all(&buf[..n]).map_err(|e| {
                                    anyhow::anyhow!(format!("远端写入失败: {} — {}", remote_str, e))
                                })?;
                                worker_bytes += n as u64;
                                pending += n as u64;
                                if pending >= 64 * 1024
                                    || last_flush.elapsed() >= Duration::from_millis(50)
                                {
                                    if let Some(ref p) = worker_pb {
                                        p.inc(pending);
                                    }
                                    pb.inc(pending);
                                    pending = 0;
                                    last_flush = Instant::now();
                                }
                            }
                            Err(e) => {
                                return Err(anyhow::anyhow!(format!(
                                    "本地读取失败: {} — {}",
                                    local_full.display(),
                                    e
                                )));
                            }
                        }
                    }
                    // Flush remaining pending progress
                    if pending > 0 {
                        if let Some(ref p) = worker_pb {
                            p.inc(pending);
                        }
                        pb.inc(pending);
                    }
                    if let Some(fpb) = worker_pb.take() {
                        fpb.finish_and_clear();
                    }
                    Ok(())
                });

                if let Err(e) = transfer_res {
                    tracing::debug!(
                        "[ts][upload] transfer failed for {}: {}; reset SFTP for next try",
                        rel,
                        e
                    );
                    let _ = failure_tx.send(format!("上传失败: {}", remote_str));
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
        });
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
}
