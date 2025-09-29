pub(super) mod download;
#[cfg(test)]
pub(super) mod mock_io;
pub(super) mod pipeline;
pub(super) mod upload;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) struct WorkerCommonCtx {
    pub(super) workers: usize,
    pub(super) mp: Arc<MultiProgress>,
    pub(super) total_pb: ProgressBar,
    pub(super) file_style: ProgressStyle,
    pub(super) server: crate::server::Server,
    pub(super) addr: String,
    pub(super) max_retries: usize,
    pub(super) target_is_dir_final: bool,
    pub(super) failure_tx: Sender<crate::TransferError>,
    pub(super) buf_size: usize,
}

#[derive(Clone, Default, Debug)]
pub(super) struct WorkerMetrics {
    pub(super) bytes: u64,
    pub(super) session_rebuilds: u32,
    pub(super) sftp_rebuilds: u32,
}

// Helpers shared by upload/download workers

pub(super) fn try_acquire_pb_slot(slot_rx: &Receiver<()>, has_slot: &mut bool) {
    if !*has_slot && slot_rx.try_recv().is_ok() {
        *has_slot = true;
    }
}

pub(super) fn maybe_create_file_pb(
    mp: &Arc<MultiProgress>,
    style: &ProgressStyle,
    size: u64,
    rel: &str,
    has_slot: bool,
) -> Option<ProgressBar> {
    if has_slot {
        let pb = mp.add(ProgressBar::new(size));
        pb.set_style(style.clone());
        pb.set_message(rel.to_string());
        Some(pb)
    } else {
        None
    }
}

pub(super) fn prepare_file_progress(
    worker_pb: &mut Option<ProgressBar>,
    mp: &Arc<MultiProgress>,
    style: &ProgressStyle,
    pb_slot_rx: &Receiver<()>,
    has_slot: &mut bool,
    file_size: u64,
    rel: &str,
) {
    if let Some(pb) = worker_pb.take() {
        pb.finish_and_clear();
    }
    try_acquire_pb_slot(pb_slot_rx, has_slot);
    *worker_pb = maybe_create_file_pb(mp, style, file_size, rel, *has_slot);
}

pub(super) fn finish_and_release_pb(
    worker_pb: &mut Option<ProgressBar>,
    slot_tx: Option<&Sender<()>>,
    has_slot: &mut bool,
) {
    if let Some(pb) = worker_pb.take() {
        pb.finish_and_clear();
    }
    if let (Some(tx), true) = (slot_tx, *has_slot) {
        let _ = tx.send(());
        *has_slot = false;
    }
}

pub(super) struct Throttler {
    pending: u64,
    last_flush: Instant,
}

impl Throttler {
    pub(super) fn new() -> Self {
        Self { pending: 0, last_flush: Instant::now() }
    }

    #[inline]
    pub(super) fn tick(
        &mut self,
        n: u64,
        worker_pb: Option<&ProgressBar>,
        total_pb: &ProgressBar,
        bytes_transferred: Option<&AtomicU64>,
    ) {
        self.pending += n;
        if self.pending >= 64 * 1024 || self.last_flush.elapsed() >= Duration::from_millis(50) {
            if let Some(pb) = worker_pb {
                pb.inc(self.pending);
            }
            total_pb.inc(self.pending);
            if let Some(bytes) = bytes_transferred {
                bytes.fetch_add(self.pending, Ordering::SeqCst);
            }
            self.pending = 0;
            self.last_flush = Instant::now();
        }
    }

    #[inline]
    pub(super) fn flush(
        &mut self,
        worker_pb: Option<&ProgressBar>,
        total_pb: &ProgressBar,
        bytes_transferred: Option<&AtomicU64>,
    ) {
        if self.pending > 0 {
            if let Some(pb) = worker_pb {
                pb.inc(self.pending);
            }
            total_pb.inc(self.pending);
            if let Some(bytes) = bytes_transferred {
                bytes.fetch_add(self.pending, Ordering::SeqCst);
            }
            self.pending = 0;
            self.last_flush = Instant::now();
        }
    }
}

/// 工作线程运行时的通道和资源管理
pub(super) struct WorkerRuntimeHandles {
    pub(super) failure_tx: Sender<crate::TransferError>,
    pub(super) failure_rx: crossbeam_channel::Receiver<crate::TransferError>,
    pub(super) metrics_tx: Sender<WorkerMetrics>,
    pub(super) metrics_rx: crossbeam_channel::Receiver<WorkerMetrics>,
    pub(super) pb_slot_tx: Sender<()>,
    pub(super) pb_slot_rx: Receiver<()>,
}

/// 设置工作线程运行时通道和进度槽位
pub(super) fn setup_worker_runtime(workers: usize) -> WorkerRuntimeHandles {
    let (failure_tx, failure_rx) = unbounded::<crate::TransferError>();
    let (metrics_tx, metrics_rx) = bounded::<WorkerMetrics>(workers);

    // 限制可见单文件进度条最大为 8（不影响实际并发）
    let (pb_slot_tx, pb_slot_rx) = bounded::<()>(8);
    for _ in 0..8 {
        let _ = pb_slot_tx.send(());
    }

    WorkerRuntimeHandles { failure_tx, failure_rx, metrics_tx, metrics_rx, pb_slot_tx, pb_slot_rx }
}

pub(super) fn finalize_worker_metrics(
    label: &str,
    worker_bytes: u64,
    worker_start: Instant,
    session_rebuilds: u32,
    sftp_rebuilds: u32,
    metrics_tx: &Sender<WorkerMetrics>,
) {
    let elapsed = worker_start.elapsed().as_secs_f64();
    if elapsed > 0.0 {
        let mb = worker_bytes as f64 / 1024.0 / 1024.0;
        tracing::info!(
            "[ts][worker] {} avg_MBps={:.2} session_rebuilds={} sftp_rebuilds={}",
            label,
            mb / elapsed,
            session_rebuilds,
            sftp_rebuilds
        );
    } else {
        tracing::info!(
            "[ts][worker] {} session_rebuilds={} sftp_rebuilds={}",
            label,
            session_rebuilds,
            sftp_rebuilds
        );
    }

    let _ = metrics_tx.send(WorkerMetrics { bytes: worker_bytes, session_rebuilds, sftp_rebuilds });
}

pub(super) fn report_failure_and_finish_pb(
    failure_tx: &Sender<crate::TransferError>,
    error: crate::TransferError,
    worker_pb: &mut Option<ProgressBar>,
    pb_slot_tx: Option<&Sender<()>>,
    has_pb_slot: &mut bool,
) {
    let _ = failure_tx.send(error);
    finish_and_release_pb(worker_pb, pb_slot_tx, has_pb_slot);
}
