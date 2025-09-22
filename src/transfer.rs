// transfer module: file transfer orchestration and helpers
use crate::config::Config;
use crate::server::ServerCollection;
use anyhow::{Context, Result};

use crate::util::retry_operation;
use crossbeam_channel::{TrySendError, bounded, unbounded};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Size of per-worker IO buffer (1 MiB)
const WORKER_BUF_SIZE: usize = 1024 * 1024;

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
}

// Context structs to keep worker function params concise
/// 公共的工作线程上下文：封装上传/下载共享的配置与资源引用。
/// - `workers`: 线程数量
/// - `mp`/`total_pb`/`file_style`: 进度展示相关
/// - `server`/`addr`: 远端主机信息
/// - `max_retries`: 单文件传输失败的重试次数
/// - `target_is_dir_final`: 目标是否最终为目录（影响目标路径拼接）
/// - `failure_tx`: 失败信息通道
/// ```rust,ignore
/// let common = WorkerCommonCtx {
///     workers,
///     mp: mp.clone(),
///     total_pb: total_pb.clone(),
///     file_style: file_style.clone(),
///     server: server.clone(),
///     addr: addr.clone(),
///     max_retries,
///     target_is_dir_final,
///     failure_tx: failure_tx.clone(),
/// };
/// ```
/// };
/// ```
fn expand_remote_tilde(sess: &ssh2::Session, path: &str) -> anyhow::Result<String> {
    let mut channel = sess.channel_session()?;
    // Try to print the remote home directory; fall back to '~' if unavailable
    let _ = channel.exec("printf '%s' \"$HOME\" || echo '~'");
    let mut s = String::new();
    channel.read_to_string(&mut s).ok();
    channel.wait_close().ok();
    let home = s.lines().next().unwrap_or("~").trim().to_string();
    let tail = path.trim_start_matches('~').trim_start_matches('/');
    let expanded =
        if tail.is_empty() { home } else { format!("{}/{}", home.trim_end_matches('/'), tail) };
    Ok(expanded)
}

// Worker context structs used to keep function parameter lists short
use crossbeam_channel::{Receiver, Sender};

struct WorkerCommonCtx {
    workers: usize,
    mp: Arc<MultiProgress>,
    total_pb: ProgressBar,
    file_style: ProgressStyle,
    server: crate::server::Server,
    addr: String,
    max_retries: usize,
    target_is_dir_final: bool,
    failure_tx: Sender<String>,
}

struct UploadWorkersCtx {
    common: WorkerCommonCtx,
    rx: Receiver<FileEntry>,
    expanded_remote_base: String,
    conn_token_rx: Receiver<()>,
    conn_token_tx: Sender<()>,
}

struct DownloadWorkersCtx {
    common: WorkerCommonCtx,
    file_rx: Receiver<FileEntry>,
    target: String,
    bytes_transferred: Arc<AtomicU64>,
    verbose: bool,
}

// Simple glob-style matcher supporting '*' and '?'. Not full-featured but
// sufficient for our use (matching file/directory names).
pub fn wildcard_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn helper(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == '*' {
            // Try to match '*' with any number of chars
            if helper(&p[1..], t) {
                return true;
            }
            if !t.is_empty() && helper(p, &t[1..]) {
                return true;
            }
            return false;
        } else if !t.is_empty() && (p[0] == '?' || p[0] == t[0]) {
            return helper(&p[1..], &t[1..]);
        }
        false
    }
    helper(&p, &t)
}

fn is_windows_drive(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let c = bytes[0] as char;
        return c.is_ascii_uppercase() || c.is_ascii_lowercase();
    }
    false
}

fn is_remote_spec(s: &str) -> bool {
    if is_windows_drive(s) {
        return false;
    }
    // Consider something remote if it contains ':' before any '/'
    if let Some(pos) = s.find(':') {
        if let Some(slash_pos) = s.find('/') {
            return pos < slash_pos;
        }
        return true;
    }
    false
}

fn is_disallowed_glob(s: &str) -> bool {
    if s.contains("**") {
        return true;
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() <= 1 {
        return false;
    }
    for seg in &parts[..parts.len() - 1] {
        if seg.contains('*') || seg.contains('?') {
            return true;
        }
    }
    false
}

fn connect_session(server: &crate::server::Server) -> anyhow::Result<ssh2::Session> {
    let addr = format!("{}:{}", server.address, server.port);
    let mut addrs = addr.to_socket_addrs()?;
    let sock = addrs.next().ok_or_else(|| anyhow::anyhow!("no address"))?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(10))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
    let mut sess =
        ssh2::Session::new().map_err(|_| anyhow::anyhow!("ssh session create failed"))?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|_| anyhow::anyhow!("ssh handshake failed"))?;
    if !sess.authenticated()
        && let Some(home_p) = dirs::home_dir()
    {
        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let p = home_p.join(".ssh").join(name);
            if p.exists() {
                let _ = sess.userauth_pubkey_file(&server.username, None, &p, None);
                if sess.authenticated() {
                    break;
                }
            }
        }
    }
    if sess.authenticated() { Ok(sess) } else { Err(anyhow::anyhow!("failed to authenticate")) }
}

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

// enumerate local sources per rules (R3/R4/R9)
fn enumerate_local_sources(sources: &[String]) -> Result<(Vec<FileEntry>, u64)> {
    use walkdir::WalkDir;
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut total_size: u64 = 0;
    for src in sources {
        let src_norm = src.replace('\\', "/");
        let has_glob = src_norm.contains('*') || src_norm.contains('?');
        let ends_slash = src_norm.ends_with('/');
        if has_glob {
            // R3: only expand within the parent dir, non-recursive
            let p = std::path::Path::new(&src_norm);
            let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
            let pat = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if let Ok(rd) = std::fs::read_dir(parent) {
                let mut matched = 0usize;
                for ent in rd.flatten() {
                    let name = ent.file_name();
                    let name = name.to_string_lossy().to_string();
                    if wildcard_match(pat, &name) {
                        matched += 1;
                        let full = parent.join(&name);
                        let md = match std::fs::metadata(&full) {
                            Ok(m) => m,
                            Err(e) => {
                                return Err(anyhow::anyhow!(format!(
                                    "本地 stat 失败: {} — {}",
                                    full.display(),
                                    e
                                )));
                            }
                        };
                        if md.is_file() {
                            total_size += md.len();
                            let full = parent.join(&name);
                            entries.push(FileEntry {
                                remote_full: String::new(),
                                rel: name.clone(),
                                size: Some(md.len()),
                                kind: EntryKind::File,
                                local_full: Some(full.to_string_lossy().to_string()),
                            });
                        } else {
                            let full = parent.join(&name);
                            entries.push(FileEntry {
                                remote_full: String::new(),
                                rel: name.clone(),
                                size: None,
                                kind: EntryKind::Dir,
                                local_full: Some(full.to_string_lossy().to_string()),
                            });
                        }
                    }
                }
                if matched == 0 {
                    return Err(anyhow::anyhow!(format!("glob 无匹配项（本地）：{}", src)));
                }
            } else {
                return Err(anyhow::anyhow!(format!("无法读取目录: {}", parent.display())));
            }
        } else {
            let p = std::path::Path::new(&src_norm);
            if ends_slash {
                if !p.exists() || !p.is_dir() {
                    return Err(anyhow::anyhow!(format!(
                        "源以 '/' 结尾但不是目录: {} (本地)",
                        src
                    )));
                }
                let root = p;
                for e in WalkDir::new(p).into_iter().filter_map(|x| x.ok()) {
                    let path = e.path();
                    if e.file_type().is_dir() {
                        let rel =
                            path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
                        if rel.is_empty() {
                            continue;
                        }
                        let abs = path.to_path_buf();
                        entries.push(FileEntry {
                            remote_full: String::new(),
                            rel,
                            size: None,
                            kind: EntryKind::Dir,
                            local_full: Some(abs.to_string_lossy().to_string()),
                        });
                    } else if e.file_type().is_file() {
                        let md = std::fs::metadata(path).unwrap();
                        total_size += md.len();
                        let rel =
                            path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
                        entries.push(FileEntry {
                            remote_full: String::new(),
                            rel,
                            size: Some(md.len()),
                            kind: EntryKind::File,
                            local_full: Some(path.to_string_lossy().to_string()),
                        });
                    }
                }
            } else {
                if !p.exists() {
                    return Err(anyhow::anyhow!(format!("源不存在: {} (本地)", src)));
                }
                if p.is_dir() {
                    // 目录无论是否带 '/'，均复制“目录内容”（不含容器），递归
                    let root = p;
                    for e in WalkDir::new(p).into_iter().filter_map(|x| x.ok()) {
                        let path = e.path();
                        if e.file_type().is_dir() {
                            let rel = path
                                .strip_prefix(root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .to_string();
                            if rel.is_empty() {
                                continue;
                            }
                            let abs = path.to_path_buf();
                            entries.push(FileEntry {
                                remote_full: String::new(),
                                rel,
                                size: None,
                                kind: EntryKind::Dir,
                                local_full: Some(abs.to_string_lossy().to_string()),
                            });
                        } else if e.file_type().is_file() {
                            let md = std::fs::metadata(path).unwrap();
                            total_size += md.len();
                            let rel = path
                                .strip_prefix(root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .to_string();
                            entries.push(FileEntry {
                                remote_full: String::new(),
                                rel,
                                size: Some(md.len()),
                                kind: EntryKind::File,
                                local_full: Some(path.to_string_lossy().to_string()),
                            });
                        }
                    }
                } else {
                    let md = std::fs::metadata(p).unwrap();
                    total_size += md.len();
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    entries.push(FileEntry {
                        remote_full: String::new(),
                        rel: name,
                        size: Some(md.len()),
                        kind: EntryKind::File,
                        local_full: Some(p.to_string_lossy().to_string()),
                    });
                }
            }
        }
    }
    Ok((entries, total_size))
}

// enumerate remote entries and push into a bounded channel (streaming)
fn enumerate_remote_and_push(
    sftp: &ssh2::Sftp,
    remote_root: &str,
    explicit_dir_suffix: bool,
    src_has_glob: bool,
    push: &dyn Fn(String, String, Option<u64>, EntryKind),
) {
    let is_glob = src_has_glob;
    if explicit_dir_suffix && !is_glob {
        if let Ok(st) = sftp.stat(std::path::Path::new(remote_root))
            && st.is_file()
        {
            // handled in the generic branch below (no-op here)
        }
        let mut q: VecDeque<(String, String)> = VecDeque::new();
        q.push_back((remote_root.to_string(), String::new()));
        while let Some((cur, rel_prefix)) = q.pop_front() {
            if let Ok(entries) = sftp.readdir(std::path::Path::new(&cur)) {
                for (pathbuf, stat) in entries {
                    if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                        if name == "." || name == ".." {
                            continue;
                        }
                        let full = format!("{}/{}", cur.trim_end_matches('/'), name);
                        let rel = if rel_prefix.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}/{}", rel_prefix, name)
                        };
                        if stat.is_file() {
                            push(full, rel, stat.size, EntryKind::File);
                        } else {
                            push(full.clone(), rel.clone(), None, EntryKind::Dir);
                            q.push_back((full, rel));
                        }
                    }
                }
            }
        }
    } else if is_glob {
        use std::path::Path;
        let p = Path::new(remote_root);
        let parent =
            p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "/".to_string());
        let pattern = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Ok(entries) = sftp.readdir(Path::new(&parent)) {
            for (pathbuf, stat) in entries {
                if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                    if name == "." || name == ".." {
                        continue;
                    }
                    if wildcard_match(pattern, name) {
                        let full = format!("{}/{}", parent.trim_end_matches('/'), name);
                        if stat.is_file() {
                            push(full, name.to_string(), stat.size, EntryKind::File);
                        } else {
                            // Matched a directory; do not recurse when using glob
                            push(full, name.to_string(), None, EntryKind::Dir);
                        }
                    }
                }
            }
        }
    } else if let Ok(m) = sftp.stat(std::path::Path::new(remote_root)) {
        if m.is_file() {
            let fname = std::path::Path::new(remote_root)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(remote_root)
                .to_string();
            push(remote_root.to_string(), fname, m.size, EntryKind::File);
        } else if explicit_dir_suffix {
            let mut q: VecDeque<(String, String)> = VecDeque::new();
            q.push_back((remote_root.to_string(), String::new()));
            while let Some((cur, rel_prefix)) = q.pop_front() {
                if let Ok(entries) = sftp.readdir(std::path::Path::new(&cur)) {
                    for (pathbuf, stat) in entries {
                        if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                            if name == "." || name == ".." {
                                continue;
                            }
                            let full = format!("{}/{}", cur.trim_end_matches('/'), name);
                            let rel = if rel_prefix.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}/{}", rel_prefix, name)
                            };
                            if stat.is_file() {
                                push(full, rel, stat.size, EntryKind::File);
                            } else {
                                push(full.clone(), rel.clone(), None, EntryKind::Dir);
                                q.push_back((full, rel));
                            }
                        }
                    }
                }
            }
        } else {
            // 目录无论是否带 '/'，均复制“目录内容”（不含容器），递归
            let mut q: VecDeque<(String, String)> = VecDeque::new();
            q.push_back((remote_root.to_string(), String::new()));
            while let Some((cur, rel_prefix)) = q.pop_front() {
                if let Ok(entries) = sftp.readdir(std::path::Path::new(&cur)) {
                    for (pathbuf, stat) in entries {
                        if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                            if name == "." || name == ".." {
                                continue;
                            }
                            let full = format!("{}/{}", cur.trim_end_matches('/'), name);
                            let rel = if rel_prefix.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}/{}", rel_prefix, name)
                            };
                            if stat.is_file() {
                                push(full, rel, stat.size, EntryKind::File);
                            } else {
                                push(full.clone(), rel.clone(), None, EntryKind::Dir);
                                q.push_back((full, rel));
                            }
                        }
                    }
                }
            }
        }
    }
}

// ensure a worker has an SSH session established and authenticated
fn ensure_worker_session(
    maybe_sess: &mut Option<ssh2::Session>,
    server: &crate::server::Server,
    addr: &str,
) -> anyhow::Result<()> {
    if maybe_sess.is_some() {
        return Ok(());
    }
    if let Ok(mut addrs) = format!("{}:{}", server.address, server.port).to_socket_addrs()
        && let Some(sock) = addrs.next()
        && let Ok(tcp) = TcpStream::connect_timeout(&sock, Duration::from_secs(10))
    {
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
        if let Ok(mut sess) = ssh2::Session::new().map(|mut s| {
            s.set_tcp_stream(tcp);
            s
        }) {
            if sess.handshake().is_err() {
                return Err(anyhow::anyhow!(format!("SSH 握手失败: {}", addr)));
            }
            if !sess.authenticated()
                && let Some(home_p) = dirs::home_dir()
            {
                for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                    let p = home_p.join(".ssh").join(name);
                    if p.exists() {
                        let _ = sess.userauth_pubkey_file(&server.username, None, &p, None);
                        if sess.authenticated() {
                            break;
                        }
                    }
                }
            }
            if sess.authenticated() {
                *maybe_sess = Some(sess);
                return Ok(());
            }
        }
    }
    Err(anyhow::anyhow!("failed to build session"))
}

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
    let mut workers = if concurrency == 0 { 6usize } else { concurrency };
    workers = std::cmp::min(workers, max_allowed_workers);
    workers
}

/// 启动上传工作线程并阻塞直至全部完成。
///
/// 行为说明:
/// - 从 `ctx.rx` 消费待传条目（文件/目录）。目录在远端作存在性与必要的单级 mkdir 处理，文件则执行读写传输。
/// - 使用 `retry_operation(max_retries)` 包裹单文件传输，减少瞬时错误的影响。
/// - 通过 `conn_token_rx/tx` 作为令牌桶限制并发建连次数，避免过多 SSH 同时握手。
/// - 使用 `MultiProgress` 与每文件 `ProgressBar` 更新总进度与单文件进度。
/// - 对失败条目通过 `failure_tx` 上报，函数最后会 join 所有线程。
fn run_upload_workers(ctx: UploadWorkersCtx) {
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
    for _worker_id in 0..workers {
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
            let mut has_token = false;
            while let Ok(entry) = rx.recv() {
                let remote_full = if expanded_remote_base.ends_with('/') || target_is_dir_final {
                    std::path::Path::new(&expanded_remote_base).join(&entry.rel)
                } else {
                    std::path::Path::new(&expanded_remote_base).to_path_buf()
                };
                let remote_str = remote_full.to_string_lossy().replace('\\', "/");
                let rel = entry.rel.clone();

                let transfer_res = retry_operation(max_retries, || -> anyhow::Result<()> {
                    if maybe_sess.is_none() {
                        if !has_token {
                            let _ = conn_token_rx.recv();
                            has_token = true;
                        }
                        if let Err(_e) = ensure_worker_session(&mut maybe_sess, &server, &addr) {
                            let _ = failure_tx.send(format!(
                                "认证失败: {} (远端)",
                                server.alias.as_deref().unwrap_or("<unknown>")
                            ));
                            if has_token {
                                let _ = conn_token_tx.send(());
                                has_token = false;
                            }
                            return Ok(());
                        }
                    }

                    let sess = maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
                    let sftp = sess.sftp().map_err(|_| anyhow::anyhow!("sftp create failed"))?;

                    if entry.kind == EntryKind::Dir {
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
                    let file_pb = mp.add(ProgressBar::new(entry.size.unwrap_or(0)));
                    file_pb.set_style(file_style.clone());
                    file_pb.set_message(rel.clone());
                    worker_pb = Some(file_pb.clone());

                    let local_full = if let Some(ref lf) = entry.local_full {
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
                    loop {
                        match local_file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                remote_f.write_all(&buf[..n]).map_err(|e| {
                                    anyhow::anyhow!(format!("远端写入失败: {} — {}", remote_str, e))
                                })?;
                                worker_bytes += n as u64;
                                if let Some(ref p) = worker_pb {
                                    p.inc(n as u64);
                                }
                                pb.inc(n as u64);
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
                    if let Some(fpb) = worker_pb.take() {
                        fpb.finish_and_clear();
                    }
                    Ok(())
                });

                if let Err(_e) = transfer_res {
                    let _ = failure_tx.send(format!("上传失败: {}", remote_str));
                }

                if has_token && maybe_sess.is_none() {
                    let _ = conn_token_tx.send(());
                    has_token = false;
                }
            }
            let elapsed = worker_start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let mb = worker_bytes as f64 / 1024.0 / 1024.0;
                tracing::info!("[ts][worker] upload avg_MBps={:.2}", mb / elapsed);
            }
        });
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
}

/// 启动下载工作线程并返回其 JoinHandle 列表，由调用者负责 join。
///
/// 行为说明:
/// - 从 `ctx.file_rx` 消费远端条目；目录按需在本地创建，文件采用“写入临时文件 -> fsync -> 原子重命名”的方式落盘。
/// - 传输过程受 `retry_operation(max_retries)` 保护，降低临时网络/IO 问题的失败率。
/// - 进度更新通过 `MultiProgress` 与 `total_pb` 展示；累计字节写入 `bytes_transferred`。
/// - 错误信息通过 `failure_tx` 上报。
fn run_download_workers(ctx: DownloadWorkersCtx) -> Vec<std::thread::JoinHandle<()>> {
    let DownloadWorkersCtx { common, file_rx, target, bytes_transferred, verbose } = ctx;
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
        let handle = std::thread::spawn(move || {
            let mut worker_pb: Option<ProgressBar> = None;
            let mut buf = vec![0u8; 1024 * 1024];
            let mut maybe_sess: Option<ssh2::Session> = None;
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

                match File::create(&local_target) {
                    Ok(_) => {}
                    Err(_) => {
                        let _ = failure_tx
                            .send(format!("local create failed: {}", local_target.display()));
                        if let Some(fpb) = worker_pb.take() {
                            fpb.finish_and_clear();
                        }
                        if verbose {
                            tracing::debug!(
                                "[ts][download] skip {} due to local create failure",
                                file_name
                            );
                        }
                        continue;
                    }
                }

                let transfer_res = retry_operation(max_retries, || -> anyhow::Result<()> {
                    if maybe_sess.is_none()
                        && let Err(_e) = ensure_worker_session(&mut maybe_sess, &server, &addr)
                    {
                        return Err(anyhow::anyhow!("failed to build session"));
                    }
                    let sess = maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
                    let sftp = sess.sftp().map_err(|_| anyhow::anyhow!("sftp create failed"))?;
                    let mut remote_f = sftp
                        .open(std::path::Path::new(&remote_full))
                        .map_err(|e| anyhow::anyhow!("remote open failed: {}", e))?;
                    let parent = local_target.parent().unwrap_or_else(|| std::path::Path::new("."));
                    let tmp_name = format!("{}.hp.part.{}", file_name, std::process::id());
                    let tmp_path = parent.join(tmp_name);
                    let mut local_f = File::create(&tmp_path)
                        .map_err(|e| anyhow::anyhow!("local create failed: {}", e))?;
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
                                if let Some(ref p) = worker_pb {
                                    p.inc(n as u64);
                                }
                                total_pb.inc(n as u64);
                                bytes_transferred.fetch_add(n as u64, Ordering::SeqCst);
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "[ts][download] remote read error for {}: {:?}",
                                    remote_full,
                                    e
                                );
                                let _ = std::fs::remove_file(&tmp_path);
                                return Err(anyhow::anyhow!("remote read failed: {}", e));
                            }
                        }
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
                    if let Err(e) = std::fs::rename(&tmp_path, &local_target) {
                        tracing::debug!(
                            "[ts][download] rename temp {} -> {} failed: {:?}",
                            tmp_path.display(),
                            local_target.display(),
                            e
                        );
                        let _ = std::fs::remove_file(&tmp_path);
                        return Err(anyhow::anyhow!("rename failed: {}", e));
                    }
                    Ok(())
                });

                if let Err(_e) = transfer_res {
                    let _ = failure_tx.send(format!("download failed: {}", remote_full));
                }

                if let Some(fpb) = worker_pb.take() {
                    fpb.finish_and_clear();
                }
                if verbose {
                    tracing::debug!("[ts][download] finished {}", file_name);
                }
            }
        });
        handles.push(handle);
    }
    handles
}

/// 传输子命令主入口：根据源/目标判定方向，完成上传或下载。
///
/// 概览:
/// - 方向判定：源或目标必须且只有一端是远端（`alias:/path`）。
/// - glob 规则：仅允许最后一段使用 `*`/`?`，禁止 `**` 递归；无匹配时报错。
/// - 目录语义：目录是否带 `/` 会影响枚举与目标路径拼接；目标是否为目录会在前置检查中确定。
/// - 进度与并发：使用 `MultiProgress` 展示总体/单文件进度，工人线程并发受限且单文件传输带重试。
/// - 失败输出：可选将失败清单追加写入文件（参数 `output_failures`）。
pub fn handle_ts(config: &Config, args: HandleTsArgs) -> Result<()> {
    let HandleTsArgs { sources, target, verbose, concurrency, output_failures, max_retries } = args;
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
    let max_allowed_workers = 8usize;

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
            let (tx, rx) = bounded::<FileEntry>(std::cmp::max(4, workers * 4));
            let (failure_tx, failure_rx) = unbounded::<String>();
            // connection token bucket
            let (conn_token_tx, conn_token_rx) = bounded::<()>(workers);
            for _ in 0..workers {
                let _ = conn_token_tx.send(());
            }

            for e in entries.drain(..) {
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
                },
                rx,
                expanded_remote_base: expanded_remote_base.clone(),
                conn_token_rx: conn_token_rx.clone(),
                conn_token_tx: conn_token_tx.clone(),
            });
            drop(failure_tx);
            let failures_vec: Vec<String> = failure_rx.into_iter().collect();

            total_pb.finish_with_message("上传完成");
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let mb = total_size as f64 / 1024.0 / 1024.0;
                println!(
                    "平均速率: {:.2} MB/s (传输 {} 字节, 耗时 {:.2} 秒)",
                    mb / elapsed,
                    total_size,
                    elapsed
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
            let producer_workers = if concurrency == 0 { 6usize } else { concurrency };
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
                },
                file_rx: file_rx.clone(),
                target: target.clone(),
                bytes_transferred: bytes_transferred.clone(),
                verbose,
            });
            // Enumerate in current thread (streaming) to avoid cloning SFTP/session
            // local helper to push an entry
            let file_tx_clone = file_tx.clone();
            let files_discovered_ref = files_discovered.clone();
            let estimated_total_bytes_ref = estimated_total_bytes.clone();
            let total_pb_clone = total_pb.clone();
            let push = |full: String, rel: String, size: Option<u64>, kind: EntryKind| {
                let entry = FileEntry { remote_full: full, rel, size, kind, local_full: None };
                let mut backoff = 10u64; // ms
                loop {
                    match file_tx_clone.try_send(entry.clone()) {
                        Ok(()) => break,
                        Err(TrySendError::Full(_)) => {
                            std::thread::sleep(std::time::Duration::from_millis(backoff));
                            backoff = std::cmp::min(backoff * 2, 500);
                        }
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
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
            let failures_vec: Vec<String> = failure_rx.into_iter().collect();
            let _ = mp.clear();
            total_pb.finish_with_message("下载完成");
            let elapsed = start.elapsed().as_secs_f64();
            let total_done = bytes_transferred.load(Ordering::SeqCst);
            if elapsed > 0.0 {
                let mb = total_done as f64 / 1024.0 / 1024.0;
                println!(
                    "平均速率: {:.2} MB/s (传输 {} 字节, 耗时 {:.2} 秒)",
                    mb / elapsed,
                    total_done,
                    elapsed
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
