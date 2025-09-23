pub(super) mod download;
pub(super) mod upload;

use crossbeam_channel::Sender;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;

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
}

#[derive(Clone, Default, Debug)]
pub(super) struct WorkerMetrics {
    pub(super) bytes: u64,
    pub(super) session_rebuilds: u32,
    pub(super) sftp_rebuilds: u32,
}
