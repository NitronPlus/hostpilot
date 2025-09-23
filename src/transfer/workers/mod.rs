pub(super) mod download;
pub(super) mod upload;

use crossbeam_channel::{Receiver, Sender};
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
    pub(super) failure_tx: Sender<String>,
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
