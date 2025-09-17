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
#[derive(Clone)]
struct FileEntry {
    remote_full: String,
    rel: String,
    size: Option<u64>,
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

// Simple glob matcher used by remote listing (supports '*' and '?').

pub fn wildcard_match(pat: &str, name: &str) -> bool {
    // very small glob matcher used by transfer listing tests
    let p = pat.as_bytes();
    let s = name.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut match_i): (isize, usize) = (-1, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi as isize;
            pi += 1;
            match_i = si;
        } else if star != -1 {
            pi = (star + 1) as usize;
            match_i += 1;
            si = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

pub fn handle_ts(config: &Config, args: HandleTsArgs) -> Result<()> {
    let HandleTsArgs { sources, target, verbose, concurrency, output_failures, max_retries } = args;
    // 确定传输方向 — Determine transfer direction
    let target_is_remote = crate::parse::parse_alias_and_path(&target).is_ok();
    let source0_is_remote =
        sources.first().map(|s| crate::parse::parse_alias_and_path(s).is_ok()).unwrap_or(false);

    // Rule checks (R1..R10 subset enforcement):
    // - Reject when both source and target are remote (双远端禁止)
    if target_is_remote && source0_is_remote {
        return Err(anyhow::anyhow!("源和目标同时为远端是不被支持的（请只指定一个远端别名）"));
    }

    // - Glob validation: allow '*'/'?' only in the final path segment (basename),
    //   disallow '**' anywhere and disallow wildcards in intermediate path segments.
    let contains_wild = |s: &str| s.chars().any(|c| c == '*' || c == '?');

    for s in sources.iter().chain(std::iter::once(&target)) {
        if s.contains("**") {
            return Err(anyhow::anyhow!(format!("不支持递归 glob '**'：{}", s)));
        }
        if contains_wild(s) {
            let segs: Vec<&str> = s.split('/').collect();
            if segs.len() >= 2 {
                for seg in &segs[..segs.len() - 1] {
                    if seg.contains('*') || seg.contains('?') {
                        return Err(anyhow::anyhow!(format!(
                            "不支持在中间路径段使用通配符：{} (问题段 '{}')",
                            s, seg
                        )));
                    }
                }
            }
            // Targets must never contain wildcards
            if s == target.as_str() && contains_wild(s) {
                return Err(anyhow::anyhow!(format!("目标路径不得包含通配符: {}", s)));
            }
        }
    }

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

    if target_is_remote {
        // 上传：本地 -> 远端 — Upload local -> remote
        if sources.is_empty() {
            return Err(anyhow::anyhow!("ts 上传需要至少一个本地源"));
        }
        // 从目标解析 alias:path — Parse alias:path from target
        let (alias, remote_path) = crate::parse::parse_alias_and_path(&target)?;
        let collection = ServerCollection::read_from_storage(&config.server_file_path)?;
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };

        use ssh2::Session;
        use walkdir::WalkDir;

        // 创建 SSH 会话以展开 ~ 并检查目标类型 — Create a session to expand ~ and check target type
        let addr = format!("{}:{}", server.address, server.port);
        tracing::debug!("[ts][upload] connect addr={}", addr);
        let tcp = TcpStream::connect(&addr).with_context(|| format!("TCP 连接到 {} 失败", addr))?;
        let mut sess = Session::new().context("创建 SSH 会话失败")?;
        sess.set_tcp_stream(tcp);
        sess.handshake().with_context(|| format!("SSH 握手失败: {}", addr))?;

        // 尝试使用 agent/密钥认证（尽力而为） — Try agent/auth (best effort)
        let mut auth_errs: Vec<String> = Vec::new();
        // Try pubkey files (do not use ssh-agent)
        if !sess.authenticated() {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法获取 home 目录"))?;
            for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                let p = home.join(".ssh").join(name);
                if p.exists() {
                    if sess.userauth_pubkey_file(&server.username, None, &p, None).is_ok() {
                        break;
                    }
                } else {
                    auth_errs.push(format!("key not found: {}", p.display()));
                }
            }
        }
        if !sess.authenticated() {
            tracing::debug!("[ts][upload] SSH auth failed for {}: {}", addr, auth_errs.join("; "));
            return Err(anyhow::anyhow!(format!("SSH 认证失败: {}", auth_errs.join("; "))));
        }

        // 展开远端路径中的 ~ — Expand remote ~
        let mut expanded_remote_base = remote_path.clone();
        if expanded_remote_base.starts_with('~') {
            let mut channel =
                sess.channel_session().with_context(|| "无法打开远端 shell 来解析 ~")?;
            channel.exec("echo $HOME").ok();
            let mut s = String::new();
            channel.read_to_string(&mut s).ok();
            channel.wait_close().ok();
            let home = s.lines().next().unwrap_or("~").trim().to_string();
            let tail = expanded_remote_base.trim_start_matches('~').trim_start_matches('/');
            if tail.is_empty() {
                expanded_remote_base = home;
            } else {
                expanded_remote_base = format!("{}/{}", home.trim_end_matches('/'), tail);
            }
        }

        // 收集本地文件路径（支持目录与尾部斜杠语义） — Collect local file paths (support directories and trailing-slash semantics)
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        for src in sources.iter() {
            let p = std::path::Path::new(src);
            let src_is_glob = src.chars().any(|c| c == '*' || c == '?');
            if src.ends_with('/') && !src_is_glob {
                if !p.exists() {
                    return Err(anyhow::anyhow!(format!("本地源 '{}' 不存在", src)));
                }
                if !p.is_dir() {
                    return Err(anyhow::anyhow!(format!("本地源 '{}' 以 '/' 结尾但不是目录", src)));
                }
                for e in WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
                    if e.file_type().is_file() {
                        paths.push(e.path().to_path_buf());
                        roots.push(p.to_path_buf());
                    }
                }
            } else if p.is_dir() {
                for e in WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
                    if e.file_type().is_file() {
                        paths.push(e.path().to_path_buf());
                        roots.push(p.to_path_buf());
                    }
                }
            } else {
                paths.push(p.to_path_buf());
                let root = p.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| p.to_path_buf());
                roots.push(root);
            }
        }

        let total_size: u64 =
            paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();

        let mp = Arc::new(if verbose {
            MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
        } else {
            MultiProgress::new()
        });
        let total_pb = mp.add(ProgressBar::new(total_size));
        total_pb.set_style(total_style.clone());

        // 检查远端目标是否存在及类型 — Check remote target existence/type
        let sftp_main = sess.sftp().with_context(|| format!("创建 SFTP 会话失败: {}", addr)).ok();
        let mut target_is_dir_remote = remote_path.ends_with('/');
        if let Some(sftp_ref) = &sftp_main
            && let Ok(st) = sftp_ref.stat(std::path::Path::new(&expanded_remote_base))
        {
            target_is_dir_remote = !st.is_file();
        }

        if paths.len() == 1 {
            let src0 = std::path::Path::new(&sources[0]);
            if src0.is_dir()
                && target_is_dir_remote
                && let Some(name) = src0.file_name().and_then(|n| n.to_str())
            {
                expanded_remote_base =
                    format!("{}/{}", expanded_remote_base.trim_end_matches('/'), name);
                if let Some(sftp_ref) = sftp_main.as_ref() {
                    let _ = sftp_ref.mkdir(std::path::Path::new(&expanded_remote_base), 0o755);
                }
            }
        }

        // 准备基于 channel 的任务队列 — Prepare channel queue
        let paths_arc = Arc::new(paths);
        let roots_arc = Arc::new(roots);
        let total_files = paths_arc.len();
        let mut workers = if concurrency == 0 { 1 } else { concurrency };
        workers = std::cmp::min(workers, max_allowed_workers);
        workers = std::cmp::min(workers, total_files);
        let (tx, rx) = bounded::<usize>(total_files);
        let (failure_tx, failure_rx) = unbounded::<String>();
        // Phase B: connection token bucket to limit concurrent live sessions per-alias
        let (conn_token_tx, conn_token_rx) = bounded::<()>(workers);
        for _ in 0..workers {
            let _ = conn_token_tx.send(());
        }
        for i in 0..total_files {
            let _ = tx.send(i);
        }
        drop(tx);

        let start = Instant::now();

        // `max_retries` provided by caller
        let mut handles = Vec::new();
        for worker_id in 0..workers {
            let rx = rx.clone();
            let pb = total_pb.clone();
            let mp = mp.clone();
            let file_style = file_style.clone();
            let server = server.clone();
            let expanded_remote_base = expanded_remote_base.clone();
            let paths_arc = paths_arc.clone();
            let roots_arc = roots_arc.clone();
            let failure_tx = failure_tx.clone();
            let conn_token_rx = conn_token_rx.clone();
            let conn_token_tx = conn_token_tx.clone();
            let addr = addr.clone();
            // 重用外层的 `verbose` 标志 — Reuse outer `verbose`
            let handle = std::thread::spawn(move || {
                let mut worker_pb: Option<ProgressBar> = None;
                // allocate a reused buffer per worker to avoid repeated allocations
                let mut buf = vec![0u8; WORKER_BUF_SIZE];
                let worker_start = Instant::now();
                let mut worker_bytes: u64 = 0;
                // Phase B: per-worker session with global token limiter
                let mut maybe_sess: Option<Session> = None;
                let mut has_token: bool = false;
                while let Ok(idx) = rx.recv() {
                    tracing::debug!("[ts][worker] worker_id={} picked index={}", worker_id, idx);
                    let local_path = &paths_arc[idx];
                    let root = &roots_arc[idx];
                    let rel = if root.exists() && root.is_dir() {
                        local_path.strip_prefix(root).unwrap_or(local_path)
                    } else {
                        local_path
                            .file_name()
                            .map(std::path::Path::new)
                            .unwrap_or(local_path.as_path())
                    };
                    let remote_full = std::path::Path::new(&expanded_remote_base).join(rel);
                    let remote_str = remote_full.to_string_lossy().replace('\\', "/");
                    // 使用通用重试 helper 处理可重试的传输步骤
                    let transfer_res = retry_operation(max_retries, || -> anyhow::Result<()> {
                        tracing::debug!(
                            "[ts][download] worker_id={} attempting transfer for {:?}",
                            worker_id,
                            remote_full
                        );
                        tracing::debug!(
                            "[ts][worker] worker_id={} start transfer file={}",
                            worker_id,
                            local_path.display()
                        );
                        // 确保会话已建立（必要时建立，包含令牌获取）
                        if maybe_sess.is_none() {
                            // try to acquire token then build one session; if building fails return Err so outer retry_operation retries
                            if !has_token {
                                tracing::debug!(
                                    "[ts][worker] worker_id={} acquiring token",
                                    worker_id
                                );
                                let _ = conn_token_rx.recv();
                                has_token = true;
                            }
                            let server_cl = server.clone();
                            if let Ok(mut addrs) =
                                format!("{}:{}", server_cl.address, server_cl.port)
                                    .to_socket_addrs()
                                && let Some(sock) = addrs.next()
                                && let Ok(tcp) =
                                    TcpStream::connect_timeout(&sock, Duration::from_secs(10))
                            {
                                let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
                                let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
                                if let Ok(mut sess) = Session::new().map(|mut s| {
                                    s.set_tcp_stream(tcp);
                                    s
                                }) {
                                    sess.handshake()
                                        .with_context(|| format!("SSH 握手失败: {}", addr))?;
                                    // Try pubkey files (do not use ssh-agent)
                                    if !sess.authenticated()
                                        && let Some(home_p) = dirs::home_dir()
                                    {
                                        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                                            let p = home_p.join(".ssh").join(name);
                                            if p.exists() {
                                                let _ = sess.userauth_pubkey_file(
                                                    &server_cl.username,
                                                    None,
                                                    &p,
                                                    None,
                                                );
                                                if sess.authenticated() {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if sess.authenticated() {
                                        tracing::debug!(
                                            "[ts][worker] worker_id={} session authenticated for {}",
                                            worker_id,
                                            addr
                                        );
                                        maybe_sess = Some(sess);
                                    }
                                }
                            }
                            if maybe_sess.is_none() {
                                let alias_str = server.alias.as_deref().unwrap_or("<unknown>");
                                tracing::debug!(
                                    "[ts][worker] worker_id={} auth failed for alias {}",
                                    worker_id,
                                    alias_str
                                );
                                let _ = failure_tx
                                    .send(format!("worker auth failed for alias {}", alias_str));
                                if has_token {
                                    tracing::debug!(
                                        "[ts][worker] worker_id={} releasing token after auth fail",
                                        worker_id
                                    );
                                    let _ = conn_token_tx.send(());
                                    has_token = false;
                                }
                                return Ok(()); // treat auth failure as non-retriable for this file
                            }
                        }

                        let sess =
                            maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
                        let sftp =
                            sess.sftp().map_err(|_| anyhow::anyhow!("sftp create failed"))?;

                        if let Some(parent) = std::path::Path::new(&remote_str).parent() {
                            let parent_str = parent.to_string_lossy().replace('\\', "/");
                            let mut acc = String::new();
                            for part in parent_str.split('/') {
                                if part.is_empty() {
                                    if acc.is_empty() {
                                        acc.push('/');
                                    }
                                    continue;
                                }
                                if !acc.ends_with('/') {
                                    acc.push('/');
                                }
                                acc.push_str(part);
                                if sftp.stat(std::path::Path::new(&acc)).is_err() {
                                    let _ = sftp.mkdir(std::path::Path::new(&acc), 0o755);
                                }
                            }
                        }

                        if let Some(old) = worker_pb.take() {
                            old.finish_and_clear();
                        }
                        let file_size =
                            std::fs::metadata(local_path).ok().map(|m| m.len()).unwrap_or_default();
                        let file_pb = mp.add(ProgressBar::new(file_size));
                        file_pb.set_style(file_style.clone());
                        let rel_str = rel.to_string_lossy().to_string().replace('\\', "/");
                        file_pb.set_message(rel_str);
                        worker_pb = Some(file_pb.clone());

                        let mut encountered_error = false;
                        if let Ok(mut local_file) = File::open(local_path) {
                            if let Ok(mut remote_f) = sftp.create(std::path::Path::new(&remote_str))
                            {
                                // reuse worker-local buffer
                                loop {
                                    match local_file.read(&mut buf) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            if remote_f.write_all(&buf[..n]).is_err() {
                                                encountered_error = true;
                                                break;
                                            }
                                            worker_bytes += n as u64;
                                            if let Some(ref p) = worker_pb {
                                                p.inc(n as u64);
                                            }
                                            pb.inc(n as u64);
                                        }
                                        Err(_) => {
                                            encountered_error = true;
                                            break;
                                        }
                                    }
                                }
                            } else {
                                encountered_error = true;
                            }
                        } else {
                            let _ = failure_tx
                                .send(format!("local open failed: {}", local_path.display()));
                            return Ok(()); // 不重试缺失的本地文件
                        }

                        if encountered_error {
                            // drop the session on error and return token only when session
                            // is actually released. we clear maybe_sess here and defer
                            // the token return until after the loop so we don't accidentally
                            // return the token while session is still considered active.
                            maybe_sess = None;
                            tracing::debug!(
                                "[ts][worker] worker_id={} encountered error uploading {}",
                                worker_id,
                                remote_str
                            );
                            Err(anyhow::anyhow!("upload transfer failed"))
                        } else {
                            if let Some(fpb) = worker_pb.take() {
                                fpb.finish_and_clear();
                            }
                            Ok(())
                        }
                    });

                    if let Err(_e) = transfer_res {
                        tracing::debug!(
                            "[ts][worker] worker_id={} transfer failed for {}",
                            worker_id,
                            remote_str
                        );
                        let _ = failure_tx.send(format!("upload failed: {}", remote_str));
                    } else {
                        tracing::debug!(
                            "[ts][worker] worker_id={} transfer succeeded for {}",
                            worker_id,
                            remote_str
                        );
                    }
                    // Return token if we previously acquired one and the session
                    // has been released (maybe_sess == None) or we are exiting.
                    if has_token && maybe_sess.is_none() {
                        tracing::debug!(
                            "[ts][worker] worker_id={} releasing token (session dropped)",
                            worker_id
                        );
                        let _ = conn_token_tx.send(());
                        has_token = false;
                    }
                }
                // worker exiting; log per-worker stats
                let elapsed = worker_start.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let mb = worker_bytes as f64 / 1024.0 / 1024.0;
                    tracing::info!(
                        "[ts][worker] worker_id={} bytes={} MB elapsed={:.2}s avg_MBps={:.2}",
                        worker_id,
                        worker_bytes,
                        elapsed,
                        mb / elapsed
                    );
                } else {
                    tracing::info!(
                        "[ts][worker] worker_id={} bytes={} elapsed=0s",
                        worker_id,
                        worker_bytes
                    );
                }
            });
            handles.push(handle);
        }

        for h in handles {
            let _ = h.join();
        }

        // close failure sender and collect failures
        drop(failure_tx);
        let failures_vec: Vec<String> = failure_rx.into_iter().collect();

        total_pb.finish_with_message("上传完成");
        let elapsed = start.elapsed().as_secs_f64();
        let total_done = total_size;
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

        // 失败汇总（尽力而为） — Failures summary (best-effort)
        if !failures_vec.is_empty() {
            eprintln!("传输失败文件列表:");
            for f in failures_vec.iter() {
                eprintln!(" - {}", f);
            }
            // Delegate writing failures to helper
            write_failures(output_failures.clone(), &failures_vec);
        }

        Ok(())
    } else if source0_is_remote {
        // 下载：远端 -> 本地 — Download remote -> local
        if sources.len() != 1 {
            return Err(anyhow::anyhow!("ts 下载仅支持单个远端源"));
        }
        let (alias, remote_path) = crate::parse::parse_alias_and_path(&sources[0])?;
        let collection = ServerCollection::read_from_storage(&config.server_file_path)?;
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };

        use ssh2::Session;

        let addr = format!("{}:{}", server.address, server.port);
        let tcp = TcpStream::connect(&addr).with_context(|| format!("TCP 连接到 {} 失败", addr))?;
        let mut sess = Session::new().context("创建 SSH 会话失败")?;
        sess.set_tcp_stream(tcp);
        sess.handshake().with_context(|| format!("SSH 握手失败: {}", addr))?;

        // 认证（尽力而为） — Auth (best-effort)
        let mut auth_errs: Vec<String> = Vec::new();
        // Try pubkey files (do not use ssh-agent)
        if !sess.authenticated() {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法获取 home 目录"))?;
            for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                let p = home.join(".ssh").join(name);
                if p.exists() {
                    if sess.userauth_pubkey_file(&server.username, None, &p, None).is_ok() {
                        break;
                    }
                } else {
                    auth_errs.push(format!("key not found: {}", p.display()));
                }
            }
        }
        if !sess.authenticated() {
            tracing::debug!("SSH 认证失败: {}", auth_errs.join("; "));
            return Err(anyhow::anyhow!(format!("SSH 认证失败: {}", auth_errs.join("; "))));
        }

        let sftp = sess.sftp().with_context(|| format!("创建 SFTP 会话失败: {}", addr))?;

        // 在远端展开 ~ — Expand ~ on remote
        let mut remote_root = remote_path.clone();
        if remote_root.starts_with('~') {
            let mut channel =
                sess.channel_session().with_context(|| "无法打开远端 shell 来解析 ~")?;
            channel.exec("echo $HOME").ok();
            let mut s = String::new();
            channel.read_to_string(&mut s).ok();
            channel.wait_close().ok();
            let home = s.lines().next().unwrap_or("~").trim().to_string();
            let tail = remote_root.trim_start_matches('~').trim_start_matches('/');
            if tail.is_empty() {
                remote_root = home;
            } else {
                remote_root = format!("{}/{}", home.trim_end_matches('/'), tail);
            }
        }

        // Ensure remote_root exists and determine whether it's a directory or file.
        let meta = sftp
            .stat(std::path::Path::new(&remote_root))
            .map_err(|e| anyhow::anyhow!("远端路径不存在或无法访问: {}: {}", remote_root, e))?;
        let remote_is_dir = !meta.is_file();

        // 枚举远端文件（支持目录、通配或单文件） — Streamed producer (support dir, glob, or single)
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

        let mut workers = if concurrency == 0 { 6usize } else { concurrency };
        workers = std::cmp::min(workers, max_allowed_workers);

        let (failure_tx, failure_rx) = unbounded::<String>();

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
                // per-worker reused buffer for downloads
                let mut buf = vec![0u8; 1024 * 1024];
                let mut maybe_sess: Option<Session> = None;
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
                    // Decide local target path depending on whether remote source is a directory
                    let local_target = if remote_is_dir {
                        let tpath = std::path::Path::new(&target);
                        if tpath.exists() {
                            if !tpath.is_dir() {
                                let _ = failure_tx.send(format!(
                                    "local target exists and is not a directory: {}",
                                    tpath.display()
                                ));
                                if let Some(fpb) = worker_pb.take() {
                                    fpb.finish_and_clear();
                                }
                                // skip this entry
                                continue;
                            }
                        } else if let Err(e) = std::fs::create_dir_all(tpath) {
                            let _ = failure_tx.send(format!(
                                "failed to create target dir {}: {}",
                                tpath.display(),
                                e
                            ));
                            if let Some(fpb) = worker_pb.take() {
                                fpb.finish_and_clear();
                            }
                            continue;
                        }
                        std::path::Path::new(&target).join(&rel)
                    } else {
                        std::path::Path::new(&target).to_path_buf()
                    };
                    if let Some(parent) = local_target.parent() {
                        // Only create the immediate parent if its parent exists.
                        // This avoids implicit mkdir -p semantics where an absent
                        // higher-level parent would be created. If parent.parent()
                        // doesn't exist, record failure and skip this file.
                        if parent.exists() {
                            let _ = std::fs::create_dir_all(parent);
                        } else if let Some(pp) = parent.parent() {
                            if pp.exists() {
                                let _ = std::fs::create_dir_all(parent);
                            } else {
                                let _ = failure_tx.send(format!(
                                    "local parent directory does not exist: {}",
                                    parent.display()
                                ));
                                if let Some(fpb) = worker_pb.take() {
                                    fpb.finish_and_clear();
                                }
                                continue;
                            }
                        } else {
                            let _ = failure_tx.send(format!(
                                "local parent directory does not exist: {}",
                                parent.display()
                            ));
                            if let Some(fpb) = worker_pb.take() {
                                fpb.finish_and_clear();
                            }
                            continue;
                        }
                    }
                    if let Some(old) = worker_pb.take() {
                        old.finish_and_clear();
                    }
                    let file_size = entry.size.unwrap_or(0);
                    let file_pb = mp.add(ProgressBar::new(file_size));
                    file_pb.set_style(file_style.clone());
                    file_pb.set_message(rel.clone());
                    worker_pb = Some(file_pb.clone());

                    // Use generic retry helper for per-file downloads
                    // Quick sanity check: if local target is not creatable at all, record failure and skip retries.
                    match File::create(&local_target) {
                        Ok(_) => {
                            // drop the temporary file; we'll recreate inside the retry closure to overwrite per attempt
                        }
                        Err(_) => {
                            let _ = failure_tx
                                .send(format!("local create failed: {}", local_target.display()));
                            // skip this file (non-retriable local error)
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
                        // ensure worker has a session (establish on-demand)
                        if maybe_sess.is_none() {
                            let server_cl = server.clone();
                            if let Ok(mut addrs) =
                                format!("{}:{}", server_cl.address, server_cl.port)
                                    .to_socket_addrs()
                                && let Some(sock) = addrs.next()
                                && let Ok(tcp) =
                                    TcpStream::connect_timeout(&sock, Duration::from_secs(10))
                            {
                                let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
                                let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
                                if let Ok(mut sess) = Session::new().map(|mut s| {
                                    s.set_tcp_stream(tcp);
                                    s
                                }) {
                                    if sess.handshake().is_err() {
                                        tracing::debug!(
                                            "[ts][download] worker_id={} handshake failed for {}",
                                            worker_id,
                                            addr
                                        );
                                        return Err(anyhow::anyhow!("handshake failed"));
                                    }
                                    // Try pubkey files (do not use ssh-agent)
                                    if !sess.authenticated()
                                        && let Some(home_p) = dirs::home_dir()
                                    {
                                        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                                            let p = home_p.join(".ssh").join(name);
                                            if p.exists() {
                                                let _ = sess.userauth_pubkey_file(
                                                    &server_cl.username,
                                                    None,
                                                    &p,
                                                    None,
                                                );
                                                if sess.authenticated() {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if sess.authenticated() {
                                        tracing::debug!(
                                            "[ts][download] worker_id={} session authenticated for {}",
                                            worker_id,
                                            addr
                                        );
                                        maybe_sess = Some(sess);
                                    } else {
                                        tracing::debug!(
                                            "[ts][download] worker_id={} session not authenticated for {}",
                                            worker_id,
                                            addr
                                        );
                                        return Err(anyhow::anyhow!("auth failed"));
                                    }
                                }
                            }
                            // if we get here without a session, return Err to trigger retry
                            if maybe_sess.is_none() {
                                return Err(anyhow::anyhow!("failed to build session"));
                            }
                        }

                        // Having a session, perform SFTP open/read -> local write
                        let sess =
                            maybe_sess.as_mut().ok_or_else(|| anyhow::anyhow!("no session"))?;
                        let sftp =
                            sess.sftp().map_err(|_| anyhow::anyhow!("sftp create failed"))?;

                        let mut remote_f = sftp
                            .open(std::path::Path::new(&remote_full))
                            .map_err(|e| anyhow::anyhow!("remote open failed: {}", e))?;

                        // write to a temporary file and rename on success to avoid leaving 0-byte files
                        let parent =
                            local_target.parent().unwrap_or_else(|| std::path::Path::new("."));
                        let tmp_name = format!("{}.hp.part.{}", file_name, std::process::id());
                        let tmp_path = parent.join(tmp_name);
                        let mut local_f = File::create(&tmp_path)
                            .map_err(|e| anyhow::anyhow!("local create failed: {}", e))?;

                        // reuse worker-local buffer
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

                        // try to persist and atomically rename into place
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
        // Enumerate in current thread (streaming) to avoid cloning SFTP/session
        // local helper to push an entry
        let file_tx_clone = file_tx.clone();
        let files_discovered_ref = files_discovered.clone();
        let estimated_total_bytes_ref = estimated_total_bytes.clone();
        let total_pb_clone = total_pb.clone();
        let push = |full: String, rel: String, size: Option<u64>| {
            let entry = FileEntry { remote_full: full, rel, size };
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

        // reuse original traversal logic but stream each discovered file (synchronously)
        let is_glob = remote_root.chars().any(|c| c == '*' || c == '?');
        let explicit_dir_suffix = remote_root.ends_with('/');
        if explicit_dir_suffix && !is_glob {
            if let Ok(st) = sftp.stat(std::path::Path::new(&remote_root))
                && st.is_file()
            {
                // handled below
            }
            let mut q: VecDeque<(String, String)> = VecDeque::new();
            q.push_back((remote_root.clone(), String::new()));
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
                                push(full, rel, stat.size);
                            } else {
                                q.push_back((full, rel));
                            }
                        }
                    }
                }
            }
        } else if is_glob {
            use std::path::Path;
            let p = Path::new(&remote_root);
            let parent = p
                .parent()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            let pattern = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if let Ok(entries) = sftp.readdir(Path::new(&parent)) {
                let mut any = false;
                for (pathbuf, stat) in entries {
                    if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                        if name == "." || name == ".." {
                            continue;
                        }
                        if !stat.is_file() {
                            continue;
                        }
                        if wildcard_match(pattern, name) {
                            any = true;
                            let full = format!("{}/{}", parent.trim_end_matches('/'), name);
                            push(full, name.to_string(), stat.size);
                        }
                    }
                }
                if !any {
                    return Err(anyhow::anyhow!(format!(
                        "glob 没有匹配任何远端文件: {}",
                        remote_root
                    )));
                }
            }
        } else if let Ok(m) = sftp.stat(std::path::Path::new(&remote_root)) {
            if m.is_file() {
                let fname = std::path::Path::new(&remote_root)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&remote_root)
                    .to_string();
                push(remote_root.clone(), fname, m.size);
            } else {
                let mut q: VecDeque<(String, String)> = VecDeque::new();
                q.push_back((remote_root.clone(), String::new()));
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
                                    push(full, rel, stat.size);
                                } else {
                                    q.push_back((full, rel));
                                }
                            }
                        }
                    }
                }
            }
        }

        enumeration_done.store(true, Ordering::SeqCst);
        drop(file_tx_clone);
        drop(file_tx);

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
    } else {
        Err(anyhow::anyhow!(
            "无法判定传输方向：请确保目标或第一个源使用 alias:/path 格式（例如 host:~/path）"
        ))
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
