// transfer module: file transfer orchestration and helpers
mod enumeration;
mod helpers;
mod session;
mod workers;
use crate::config::Config;
use crate::server::ServerCollection;
use anyhow::{Context, Result};
pub use helpers::wildcard_match;

use self::enumeration::{enumerate_local_sources, enumerate_remote_and_push};
use self::helpers::{is_disallowed_glob, is_remote_spec};
use self::session::{connect_session, expand_remote_tilde};
use self::workers::download::{DownloadWorkersCtx, run_download_workers};
use self::workers::upload::{UploadWorkersCtx, run_upload_workers};
use self::workers::{WorkerCommonCtx, WorkerMetrics};
use crossbeam_channel::{bounded, unbounded};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::Write;
// VecDeque no longer used here after enumeration split
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Buffer size is configurable via CLI (--buf-mib); default is 1 MiB wired in main.

// Entry passed from producer to workers for processing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    File,
    Dir,
}

#[derive(Clone)]
struct FileEntry {
    remote_full: String,
    rel: String,
    size: Option<u64>,
    kind: EntryKind,
    local_full: Option<String>,
}

/// Arguments for `handle_ts` grouped to avoid too-many-arguments lint.
#[derive(Clone)]
pub struct HandleTsArgs {
    pub sources: Vec<String>,
    pub target: String,
    pub verbose: bool,
    pub concurrency: usize,
    pub output_failures: Option<std::path::PathBuf>,
    pub max_retries: usize,
    pub buf_size: usize,
}

// helper and session functions moved into submodules

// remote target pre-checks and single-level mkdir; returns whether target is dir
fn prepare_remote_target(sftp: &ssh2::Sftp, base: &str, ends_slash: bool) -> anyhow::Result<bool> {
    let target_exists_meta = sftp.stat(std::path::Path::new(base));
    let target_is_dir_final = match (ends_slash, target_exists_meta) {
        (true, Ok(st)) => {
            if st.is_file() {
                return Err(anyhow::anyhow!(format!("目标必须存在且为目录: {} (远端)", base)));
            }
            true
        }
        (true, Err(_)) => {
            return Err(anyhow::anyhow!(format!("目标必须存在且为目录: {} (远端)", base)));
        }
        (false, Ok(st)) => !st.is_file(),
        (false, Err(_)) => {
            // create target dir (one level) if parent exists
            let tpath = std::path::Path::new(base);
            if let Some(parent) = tpath.parent() {
                if sftp.stat(parent).is_ok() {
                    sftp.mkdir(tpath, 0o755).map_err(|e| {
                        anyhow::anyhow!(format!("创建远端目录失败: {} — {}", base, e))
                    })?;
                    true
                } else {
                    return Err(anyhow::anyhow!(format!(
                        "目标父目录不存在: {} (远端)，不会自动创建多级目录",
                        parent.to_string_lossy()
                    )));
                }
            } else {
                return Err(anyhow::anyhow!(format!("无效远端路径: {}", base)));
            }
        }
    };
    Ok(target_is_dir_final)
}

// local target pre-checks and single-level mkdir; returns whether target is dir
fn prepare_local_target(tpath: &std::path::Path, ends_slash: bool) -> anyhow::Result<bool> {
    if ends_slash {
        if !(tpath.exists() && tpath.is_dir()) {
            return Err(anyhow::anyhow!(format!(
                "目标必须存在且为目录: {} (本地)，建议先创建或移除尾部/",
                tpath.display()
            )));
        }
        return Ok(true);
    }
    if tpath.exists() {
        return Ok(tpath.is_dir());
    }
    // create target dir (single level). If target is a single-segment path (no parent),
    // treat parent as current directory and allow creation.
    match tpath.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            if parent.exists() && parent.is_dir() {
                std::fs::create_dir(tpath).map_err(|e| {
                    anyhow::anyhow!(format!("创建目标目录失败: {} (本地) — {}", tpath.display(), e))
                })?;
                Ok(true)
            } else {
                let pdisp = if parent.as_os_str().is_empty() {
                    ".".to_string()
                } else {
                    parent.display().to_string()
                };
                Err(anyhow::anyhow!(format!(
                    "目标父目录不存在: {} (本地)，不会自动创建多级目录，请手动创建父目录",
                    pdisp
                )))
            }
        }
        // No parent (single-segment like "dist") or empty parent -> use current directory
        _ => {
            std::fs::create_dir(tpath).map_err(|e| {
                anyhow::anyhow!(format!("创建目标目录失败: {} (本地) — {}", tpath.display(), e))
            })?;
            Ok(true)
        }
    }
}

// enumeration split into submodule

// ensure a worker has an SSH session established and authenticated
// ensure_worker_session moved into session module

// Helper: calculate upload workers bounded by max and total entries
fn calc_upload_workers(
    concurrency: usize,
    max_allowed_workers: usize,
    total_entries: usize,
) -> usize {
    let mut workers = if concurrency == 0 { 1 } else { concurrency };
    workers = std::cmp::min(workers, max_allowed_workers);
    workers = std::cmp::min(workers, std::cmp::max(1, total_entries));
    workers
}

// Helper: calculate download workers bounded by max
fn calc_download_workers(concurrency: usize, max_allowed_workers: usize) -> usize {
    let mut workers = if concurrency == 0 { 8usize } else { concurrency };
    workers = std::cmp::min(workers, max_allowed_workers);
    workers
}

// 启动上传工作线程并阻塞直至全部完成。
// 行为说明:
// - 从 `ctx.rx` 消费待传条目（文件/目录）。目录在远端作存在性与必要的单级 mkdir 处理，文件则执行读写传输。
// - 使用 `retry_operation(max_retries)` 包裹单文件传输，减少瞬时错误的影响。
// - 通过 `conn_token_rx/tx` 作为令牌桶限制并发建连次数，避免过多 SSH 同时握手。
// - 使用 `MultiProgress` 与每文件 `ProgressBar` 更新总进度与单文件进度。
// - 对失败条目通过 `failure_tx` 上报，函数最后会 join 所有线程。
// run_upload_workers moved to workers module

// 启动下载工作线程并返回其 JoinHandle 列表，由调用者负责 join。
// 行为说明:
// - 从 `ctx.file_rx` 消费远端条目；目录按需在本地创建，文件采用“写入临时文件 -> fsync -> 原子重命名”的方式落盘。
// - 传输过程受 `retry_operation(max_retries)` 保护，降低临时网络/IO 问题的失败率。
// - 进度更新通过 `MultiProgress` 与 `total_pb` 展示；累计字节写入 `bytes_transferred`。
// - 错误信息通过 `failure_tx` 上报。
// run_download_workers moved to workers module

/// 传输子命令主入口：根据源/目标判定方向，完成上传或下载。
///
/// 概览:
/// - 方向判定：源或目标必须且只有一端是远端（`alias:/path`）。
/// - glob 规则：仅允许最后一段使用 `*`/`?`，禁止 `**` 递归；无匹配时报错。
/// - 目录语义：目录是否带 `/` 会影响枚举与目标路径拼接；目标是否为目录会在前置检查中确定。
/// - 进度与并发：使用 `MultiProgress` 展示总体/单文件进度，工人线程并发受限且单文件传输带重试。
/// - 失败输出：可选将失败清单追加写入文件（参数 `output_failures`）。
pub fn handle_ts(config: &Config, args: HandleTsArgs) -> Result<()> {
    let HandleTsArgs {
        sources,
        target,
        verbose,
        concurrency,
        output_failures,
        max_retries,
        buf_size,
    } = args;
    // Early validations enforcing repository transfer rules (R1-R10)
    // R1: Exactly one side must be remote (target or first source)
    let target_is_remote = is_remote_spec(&target);
    let source0_is_remote = sources.first().map(|s| is_remote_spec(s)).unwrap_or(false);
    if target_is_remote == source0_is_remote {
        return Err(anyhow::anyhow!("命令必须且只有一端为远端（alias:/path），请检查源和目标"));
    }

    // R3: allow dir/*.txt but forbid recursive ** and wildcard in non-last segments
    for s in sources.iter() {
        if is_disallowed_glob(s) {
            return Err(anyhow::anyhow!(format!("不支持的通配符用法（仅允许最后一段）：{}", s)));
        }
    }
    // 确定传输方向 — Determine transfer direction
    // target/source detection already performed above

    // 规范化本地 '.' 目标 — Normalize local '.' target
    let target = if !target_is_remote {
        if target == "." || target == "./" {
            let cwd = std::env::current_dir().with_context(|| "无法获取当前工作目录")?;
            cwd.to_string_lossy().to_string()
        } else {
            target
        }
    } else {
        target
    };

    // 通用进度条样式 — Common progress style
    let total_style = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
    )
    .with_context(|| "无效的进度条模板")?
    .progress_chars("=> ");

    // Shared per-file progress style to avoid rebuilding template per file
    let file_style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})",
    )
    .with_context(|| "无效的进度条模板")?
    .progress_chars("=> ");

    // 将 worker 限制在合理范围 — Bound workers to sensible limits
    let max_allowed_workers = 16usize;

    // Helper: initialize MultiProgress and total ProgressBar
    fn init_progress_and_mp(
        verbose: bool,
        total: u64,
        total_style: &ProgressStyle,
    ) -> (Arc<MultiProgress>, ProgressBar) {
        let mp = Arc::new(if verbose {
            MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
        } else {
            MultiProgress::new()
        });
        let total_pb = mp.add(ProgressBar::new(total));
        total_pb.set_style(total_style.clone());
        (mp, total_pb)
    }

    enum TransferKind {
        Upload {
            server: crate::server::Server,
            addr: String,
            expanded_remote_base: String,
            entries: Vec<FileEntry>,
            total_size: u64,
        },
        Download {
            server: crate::server::Server,
            addr: String,
            remote_root: String,
        },
        Unknown,
    }

    // Build a TransferKind instance by performing the minimal parsing/auth required
    let transfer_kind = if target_is_remote {
        // Prepare upload-side instance. We'll parse alias and enumerate sources now so
        // common setup can be moved into the Upload variant.
        if sources.is_empty() {
            return Err(anyhow::anyhow!("ts 上传需要至少一个本地源"));
        }
        // parse alias:path
        let (alias, remote_path) = crate::parse::parse_alias_and_path(&target)?;
        let collection = ServerCollection::read_from_storage(&config.server_file_path)?;
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };
        let addr = format!("{}:{}", server.address, server.port);
        let sess = connect_session(server)?;
        let expanded_remote_base = expand_remote_tilde(&sess, &remote_path)?;
        // enumerate local sources
        let (entries, total_size) = enumerate_local_sources(&sources)?;
        TransferKind::Upload {
            server: server.clone(),
            addr,
            expanded_remote_base,
            entries,
            total_size,
        }
    } else if source0_is_remote {
        // Prepare download-side instance
        if sources.len() != 1 {
            return Err(anyhow::anyhow!("ts 下载仅支持单个远端源"));
        }
        let (alias, remote_path) = crate::parse::parse_alias_and_path(&sources[0])?;
        let collection = ServerCollection::read_from_storage(&config.server_file_path)?;
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };
        let addr = format!("{}:{}", server.address, server.port);
        let sess = match connect_session(server) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("SSH 认证失败: {}", e);
                return Err(e);
            }
        };
        let remote_root = expand_remote_tilde(&sess, &remote_path)?;
        TransferKind::Download { server: server.clone(), addr, remote_root }
    } else {
        TransferKind::Unknown
    };

    match transfer_kind {
        TransferKind::Upload { server, addr, expanded_remote_base, mut entries, total_size } => {
            // R2 flags per source and target
            let tgt_ends_slash = expanded_remote_base.ends_with('/');

            // sftp for probing/creating remote dirs
            let sess = connect_session(&server)?;
            let sftp = sess.sftp().with_context(|| format!("创建 SFTP 会话失败: {}", addr))?;

            // 预判目标目录策略（R5–R7）
            let target_is_dir_final =
                prepare_remote_target(&sftp, &expanded_remote_base, tgt_ends_slash)?;

            // 多源/单源一致性（R8）
            let total_entries = entries.len();
            if !target_is_dir_final && total_entries > 1 {
                return Err(anyhow::anyhow!("目标为文件路径，但存在多源/多条目；请将目标设为目录"));
            }
            if !target_is_dir_final && total_entries == 1 {
                // 唯一条目必须为文件
                if entries[0].kind != EntryKind::File {
                    return Err(anyhow::anyhow!(
                        "目标为文件路径，但源是目录；请将目标设为目录或在源后添加 '/' 复制内容"
                    ));
                }
            }

            // 进度与工作线程
            let (mp, total_pb) = init_progress_and_mp(verbose, total_size, &total_style);
            let workers = calc_upload_workers(concurrency, max_allowed_workers, total_entries);
            let cap = {
                let base = std::cmp::max(4, workers * 4);
                std::cmp::min(base, std::cmp::max(1, total_entries))
            };
            let (tx, rx) = bounded::<FileEntry>(cap);
            let (failure_tx, failure_rx) = unbounded::<String>();
            let (metrics_tx, metrics_rx) = bounded::<WorkerMetrics>(workers);
            // 限制上传侧可见单文件进度条最大为 8（不影响实际并发）
            let (pb_slot_tx, pb_slot_rx) = bounded::<()>(8);
            for _ in 0..8 {
                let _ = pb_slot_tx.send(());
            }
            // connection token bucket
            let (conn_token_tx, conn_token_rx) = bounded::<()>(workers);
            for _ in 0..workers {
                let _ = conn_token_tx.send(());
            }

            for e in entries.drain(..) {
                // Blocking send to apply backpressure on producer
                let _ = tx.send(e);
            }
            drop(tx);
            let start = Instant::now();

            run_upload_workers(UploadWorkersCtx {
                common: WorkerCommonCtx {
                    workers,
                    mp: mp.clone(),
                    total_pb: total_pb.clone(),
                    file_style: file_style.clone(),
                    server: server.clone(),
                    addr: addr.clone(),
                    max_retries,
                    target_is_dir_final,
                    failure_tx: failure_tx.clone(),
                    buf_size,
                },
                rx,
                expanded_remote_base: expanded_remote_base.clone(),
                conn_token_rx: conn_token_rx.clone(),
                conn_token_tx: conn_token_tx.clone(),
                metrics_tx: metrics_tx.clone(),
                pb_slot_rx: pb_slot_rx.clone(),
                pb_slot_tx: pb_slot_tx.clone(),
            });
            drop(failure_tx);
            drop(metrics_tx);
            let failures_vec: Vec<String> = failure_rx.into_iter().collect();
            let mut agg = WorkerMetrics::default();
            for m in metrics_rx.into_iter() {
                agg.bytes += m.bytes;
                agg.session_rebuilds += m.session_rebuilds;
                agg.sftp_rebuilds += m.sftp_rebuilds;
            }

            total_pb.finish_with_message("上传完成");
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let mb = total_size as f64 / 1024.0 / 1024.0;
                println!(
                    "平均速率: {:.2} MB/s (传输 {} 字节, 耗时 {:.2} 秒) | 会话重建: {} | SFTP重建: {}",
                    mb / elapsed,
                    total_size,
                    elapsed,
                    agg.session_rebuilds,
                    agg.sftp_rebuilds
                );
            } else {
                println!("平均速率: 0.00 MB/s");
            }

            if !failures_vec.is_empty() {
                eprintln!("传输失败文件列表:");
                for f in failures_vec.iter() {
                    eprintln!(" - {}", f);
                }
                write_failures(output_failures.clone(), &failures_vec);
            }

            Ok(())
        }
        TransferKind::Download { server, addr, remote_root } => {
            // 下载：远端 -> 本地 — Download remote -> local
            // Use the pre-parsed/expanded fields from TransferKind to avoid
            // repeating parsing/lookup.
            // Flags per R2
            let src_has_glob = remote_root.chars().any(|c| c == '*' || c == '?');
            let explicit_dir_suffix = remote_root.ends_with('/');
            let tgt_ends_slash = target.ends_with('/');

            // Pre-check and normalize local target per R5–R7
            let tpath = std::path::Path::new(&target);
            let target_is_dir_final: bool = prepare_local_target(tpath, tgt_ends_slash)?;

            // Additional multi-entry constraint (R8): if target is a file path, forbid glob or recursive
            if !target_is_dir_final && (src_has_glob || explicit_dir_suffix) {
                return Err(anyhow::anyhow!(
                    "目标为文件路径，但源为 glob 或目录递归；请将目标设为目录或去除 glob/尾部/"
                ));
            }

            // 枚举远端文件（支持目录、通配或单文件） — Streamed producer (support dir, glob, or single)
            // Need an authenticated session for enumeration
            let sess = match connect_session(&server) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("SSH 认证失败: {}", e);
                    return Err(e);
                }
            };
            let sftp = sess.sftp().with_context(|| format!("创建 SFTP 会话失败: {}", addr))?;
            let producer_workers = if concurrency == 0 { 8usize } else { concurrency };
            let cap = std::cmp::max(4, producer_workers * 4);
            let (file_tx, file_rx) = bounded::<FileEntry>(cap);
            let bytes_transferred = Arc::new(AtomicU64::new(0));
            let files_discovered = Arc::new(AtomicU64::new(0));
            let estimated_total_bytes = Arc::new(AtomicU64::new(0));
            let enumeration_done = Arc::new(AtomicBool::new(false));
            // Prepare progress and spawn worker threads BEFORE enumeration so
            // the producer won't block when the bounded channel is full.
            let start = Instant::now();
            let mp = Arc::new(if verbose {
                MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
            } else {
                MultiProgress::new()
            });
            let initial_total = estimated_total_bytes.load(Ordering::SeqCst);
            let total_pb = mp.add(ProgressBar::new(initial_total));
            total_pb.set_style(total_style.clone());
            if initial_total == 0 {
                // unknown total — show spinner so user sees activity
                total_pb.enable_steady_tick(Duration::from_millis(100));
            }

            let workers = calc_download_workers(concurrency, max_allowed_workers);

            let (failure_tx, failure_rx) = unbounded::<String>();

            let (metrics_tx, metrics_rx) = bounded::<WorkerMetrics>(workers);
            // 限制下载侧可见单文件进度条最大为 8（不影响实际并发）
            let (pb_slot_tx, pb_slot_rx) = bounded::<()>(8);
            for _ in 0..8 {
                let _ = pb_slot_tx.send(());
            }
            let handles = run_download_workers(DownloadWorkersCtx {
                common: WorkerCommonCtx {
                    workers,
                    mp: mp.clone(),
                    total_pb: total_pb.clone(),
                    file_style: file_style.clone(),
                    server: server.clone(),
                    addr: addr.clone(),
                    max_retries,
                    target_is_dir_final,
                    failure_tx: failure_tx.clone(),
                    buf_size,
                },
                file_rx: file_rx.clone(),
                target: target.clone(),
                bytes_transferred: bytes_transferred.clone(),
                verbose,
                metrics_tx: metrics_tx.clone(),
                pb_slot_rx: pb_slot_rx.clone(),
                pb_slot_tx: pb_slot_tx.clone(),
            });
            // Enumerate in current thread (streaming) to avoid cloning SFTP/session
            // local helper to push an entry
            let file_tx_clone = file_tx.clone();
            let files_discovered_ref = files_discovered.clone();
            let estimated_total_bytes_ref = estimated_total_bytes.clone();
            let total_pb_clone = total_pb.clone();
            let push = |full: String, rel: String, size: Option<u64>, kind: EntryKind| {
                let entry = FileEntry { remote_full: full, rel, size, kind, local_full: None };
                // Blocking send with bounded queue applies natural backpressure
                let _ = file_tx_clone.send(entry);
                files_discovered_ref.fetch_add(1, Ordering::SeqCst);
                if let Some(s) = size {
                    estimated_total_bytes_ref.fetch_add(s, Ordering::SeqCst);
                    let new_total = estimated_total_bytes_ref.load(Ordering::SeqCst);
                    total_pb_clone.set_length(new_total);
                    if new_total > 0 {
                        total_pb_clone.disable_steady_tick();
                    }
                }
            };

            // 复用提炼后的远端枚举推送逻辑
            enumerate_remote_and_push(
                &sftp,
                &remote_root,
                explicit_dir_suffix,
                src_has_glob,
                &push,
            );

            enumeration_done.store(true, Ordering::SeqCst);
            drop(file_tx_clone);
            drop(file_tx);

            // R3: glob with no match is an error
            if src_has_glob && files_discovered.load(Ordering::SeqCst) == 0 {
                // Join workers first to avoid leaving threads running
                for h in handles {
                    let _ = h.join();
                }
                return Err(anyhow::anyhow!("glob 无匹配项（远端），请确认模式与路径是否正确"));
            }

            for h in handles {
                let _ = h.join();
            }
            drop(failure_tx);
            drop(metrics_tx);
            let failures_vec: Vec<String> = failure_rx.into_iter().collect();
            let mut agg = WorkerMetrics::default();
            for m in metrics_rx.into_iter() {
                agg.bytes += m.bytes;
                agg.session_rebuilds += m.session_rebuilds;
                agg.sftp_rebuilds += m.sftp_rebuilds;
            }
            let _ = mp.clear();
            total_pb.finish_with_message("下载完成");
            let elapsed = start.elapsed().as_secs_f64();
            let total_done = bytes_transferred.load(Ordering::SeqCst);
            if elapsed > 0.0 {
                let mb = total_done as f64 / 1024.0 / 1024.0;
                println!(
                    "平均速率: {:.2} MB/s (传输 {} 字节, 耗时 {:.2} 秒) | 会话重建: {} | SFTP重建: {}",
                    mb / elapsed,
                    total_done,
                    elapsed,
                    agg.session_rebuilds,
                    agg.sftp_rebuilds
                );
            } else {
                println!("平均速率: 0.00 MB/s");
            }

            if !failures_vec.is_empty() {
                eprintln!("下载失败文件列表:");
                for f in failures_vec.iter() {
                    eprintln!(" - {}", f);
                }
                write_failures(output_failures.clone(), &failures_vec);
            }

            Ok(())
        }
        TransferKind::Unknown => Err(anyhow::anyhow!(
            "无法判定传输方向：请确保目标或第一个源使用 alias:/path 格式（例如 host:~/path)"
        )),
    }
}

pub fn write_failures(path: Option<std::path::PathBuf>, failures: &[String]) {
    use chrono::Utc;
    use std::fs::OpenOptions;

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
