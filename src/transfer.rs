use crate::config::Config;
use crate::server::ServerCollection;
use anyhow::{Context, Result};

use crossbeam_channel::bounded;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Simple glob matcher used by remote listing (supports '*' and '?').
#[allow(dead_code)]
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

pub fn handle_ts(
    config: &Config,
    _recursive: bool,
    sources: Vec<String>,
    target: String,
    verbose: bool,
    concurrency: usize,
    output_failures: Option<std::path::PathBuf>,
) -> Result<()> {
    // 确定传输方向 — Determine transfer direction
    let target_is_remote = crate::parse::parse_alias_and_path(&target).is_ok();
    let source0_is_remote = sources
        .first()
        .map(|s| crate::parse::parse_alias_and_path(s).is_ok())
        .unwrap_or(false);

    let original_target = target.clone();

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
    .unwrap()
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
        let collection = ServerCollection::read_from_storage(&config.server_file_path);
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };

        use ssh2::Session;
        use walkdir::WalkDir;

        // 创建 SSH 会话以展开 ~ 并检查目标类型 — Create a session to expand ~ and check target type
        let addr = format!("{}:{}", server.address, server.port);
        let tcp = TcpStream::connect(&addr).with_context(|| format!("TCP 连接到 {} 失败", addr))?;
        let mut sess = Session::new().context("创建 SSH 会话失败")?;
        sess.set_tcp_stream(tcp);
        sess.handshake()
            .with_context(|| format!("SSH 握手失败: {}", addr))?;

        // 尝试使用 agent/密钥认证（尽力而为） — Try agent/auth (best effort)
        let mut auth_errs: Vec<String> = Vec::new();
        match sess.userauth_agent(&server.username) {
            Ok(()) => {}
            Err(e) => auth_errs.push(format!("agent: {}", e)),
        }
        if !sess.authenticated() {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法获取 home 目录"))?;
            for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                let p = home.join(".ssh").join(name);
                if p.exists() {
                    if sess
                        .userauth_pubkey_file(&server.username, None, &p, None)
                        .is_ok()
                    {
                        break;
                    }
                } else {
                    auth_errs.push(format!("key not found: {}", p.display()));
                }
            }
        }
        if !sess.authenticated() {
            tracing::debug!("SSH 认证失败: {}", auth_errs.join("; "));
            return Err(anyhow::anyhow!(format!(
                "SSH 认证失败: {}",
                auth_errs.join("; ")
            )));
        }

        // 展开远端路径中的 ~ — Expand remote ~
        let mut expanded_remote_base = remote_path.clone();
        if expanded_remote_base.starts_with('~') {
            let mut channel = sess
                .channel_session()
                .with_context(|| "无法打开远端 shell 来解析 ~")?;
            channel.exec("echo $HOME").ok();
            let mut s = String::new();
            channel.read_to_string(&mut s).ok();
            channel.wait_close().ok();
            let home = s.lines().next().unwrap_or("~").trim().to_string();
            let tail = expanded_remote_base
                .trim_start_matches('~')
                .trim_start_matches('/');
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
                    return Err(anyhow::anyhow!(format!(
                        "本地源 '{}' 以 '/' 结尾但不是目录",
                        src
                    )));
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
                let root = p
                    .parent()
                    .map(|x| x.to_path_buf())
                    .unwrap_or_else(|| p.to_path_buf());
                roots.push(root);
            }
        }

        let total_size: u64 = paths
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();

        let mp = Arc::new(if verbose {
            MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
        } else {
            MultiProgress::new()
        });
        let total_pb = mp.add(ProgressBar::new(total_size));
        total_pb.set_style(total_style.clone());

        // 检查远端目标是否存在及类型 — Check remote target existence/type
        let sftp_main = sess.sftp().ok();
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
        let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        for i in 0..total_files {
            let _ = tx.send(i);
        }
        drop(tx);

        let start = Instant::now();

        let mut handles = Vec::new();
        for _ in 0..workers {
            let rx = rx.clone();
            let pb = total_pb.clone();
            let mp = mp.clone();
            let server = server.clone();
            let expanded_remote_base = expanded_remote_base.clone();
            let paths_arc = paths_arc.clone();
            let roots_arc = roots_arc.clone();
            let failures = failures.clone();
            // 重用外层的 `verbose` 标志 — Reuse outer `verbose`
            let handle = std::thread::spawn(move || {
                let mut worker_pb: Option<ProgressBar> = None;
                while let Ok(idx) = rx.recv() {
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

                    // 每个 worker 建立连接 — Connect per worker
                    if let Ok(mut addrs) =
                        format!("{}:{}", server.address, server.port).to_socket_addrs()
                        && let Some(sock) = addrs.next()
                        && let Ok(tcp) = TcpStream::connect_timeout(&sock, Duration::from_secs(10))
                    {
                        let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
                        let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
                        if let Ok(mut sess) = Session::new().map(|mut s| {
                            s.set_tcp_stream(tcp);
                            s
                        }) {
                            let _ = sess.handshake();
                            let _ = sess.userauth_agent(&server.username);
                            if !sess.authenticated()
                                && let Some(home_p) = dirs::home_dir()
                            {
                                for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                                    let p = home_p.join(".ssh").join(name);
                                    if p.exists() {
                                        let _ = sess.userauth_pubkey_file(
                                            &server.username,
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
                            if let Ok(sftp) = sess.sftp() {
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
                                        let exists = sftp.stat(std::path::Path::new(&acc)).is_ok();
                                        if !exists {
                                            let _ = sftp.mkdir(std::path::Path::new(&acc), 0o755);
                                        }
                                    }
                                }

                                if let Some(old) = worker_pb.take() {
                                    old.finish_and_clear();
                                }
                                let file_size = std::fs::metadata(local_path)
                                    .ok()
                                    .map(|m| m.len())
                                    .unwrap_or_default();
                                let file_pb = mp.add(ProgressBar::new(file_size));
                                file_pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})").unwrap().progress_chars("=> "));
                                let rel_str = rel.to_string_lossy().to_string().replace('\\', "/");
                                file_pb.set_message(rel_str);
                                worker_pb = Some(file_pb.clone());

                                if let Ok(mut local_file) = File::open(local_path) {
                                    if let Ok(mut remote_f) =
                                        sftp.create(std::path::Path::new(&remote_str))
                                    {
                                        let mut buf = vec![0u8; 1024 * 1024];
                                        loop {
                                            match local_file.read(&mut buf) {
                                                Ok(0) => break,
                                                Ok(n) => {
                                                    if remote_f.write_all(&buf[..n]).is_err() {
                                                        let mut f = failures.lock().unwrap();
                                                        f.push(format!(
                                                            "upload failed: {}",
                                                            remote_str
                                                        ));
                                                        break;
                                                    }
                                                    if let Some(ref p) = worker_pb {
                                                        p.inc(n as u64);
                                                    }
                                                    pb.inc(n as u64);
                                                }
                                                Err(_) => {
                                                    let mut f = failures.lock().unwrap();
                                                    f.push(format!(
                                                        "upload read failed: {}",
                                                        remote_str
                                                    ));
                                                    break;
                                                }
                                            }
                                        }
                                    } else {
                                        let mut f = failures.lock().unwrap();
                                        f.push(format!("remote create failed: {}", remote_str));
                                    }
                                } else {
                                    let mut f = failures.lock().unwrap();
                                    f.push(format!("local open failed: {}", local_path.display()));
                                }

                                if let Some(fpb) = worker_pb.take() {
                                    fpb.finish_and_clear();
                                }
                            }
                        }
                    }
                }
            });
            handles.push(handle);
        }

        for h in handles {
            let _ = h.join();
        }

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

        // 失败汇总（尽力而为） — Failures summary (best-effort)
        let failures_guard = failures.lock().unwrap();
        if !failures_guard.is_empty() {
            eprintln!("传输失败文件列表:");
            for f in failures_guard.iter() {
                eprintln!(" - {}", f);
            }
            // Delegate writing failures to helper
            write_failures(output_failures.clone(), &failures_guard);
        }

        Ok(())
    } else if source0_is_remote {
        // 下载：远端 -> 本地 — Download remote -> local
        if sources.len() != 1 {
            return Err(anyhow::anyhow!("ts 下载仅支持单个远端源"));
        }
        let (alias, remote_path) = crate::parse::parse_alias_and_path(&sources[0])?;
        let collection = ServerCollection::read_from_storage(&config.server_file_path);
        let Some(server) = collection.get(&alias) else {
            return Err(anyhow::anyhow!(format!("别名 '{}' 不存在", alias)));
        };

        use ssh2::Session;

        let addr = format!("{}:{}", server.address, server.port);
        let tcp = TcpStream::connect(&addr).with_context(|| format!("TCP 连接到 {} 失败", addr))?;
        let mut sess = Session::new().context("创建 SSH 会话失败")?;
        sess.set_tcp_stream(tcp);
        sess.handshake()
            .with_context(|| format!("SSH 握手失败: {}", addr))?;

        // 认证（尽力而为） — Auth (best-effort)
        let mut auth_errs: Vec<String> = Vec::new();
        match sess.userauth_agent(&server.username) {
            Ok(()) => {}
            Err(e) => auth_errs.push(format!("agent: {}", e)),
        }
        if !sess.authenticated() {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法获取 home 目录"))?;
            for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                let p = home.join(".ssh").join(name);
                if p.exists() {
                    if sess
                        .userauth_pubkey_file(&server.username, None, &p, None)
                        .is_ok()
                    {
                        break;
                    }
                } else {
                    auth_errs.push(format!("key not found: {}", p.display()));
                }
            }
        }
        if !sess.authenticated() {
            tracing::debug!("SSH 认证失败: {}", auth_errs.join("; "));
            return Err(anyhow::anyhow!(format!(
                "SSH 认证失败: {}",
                auth_errs.join("; ")
            )));
        }

        let sftp = sess
            .sftp()
            .with_context(|| format!("创建 SFTP 会话失败: {}", addr))?;

        // 在远端展开 ~ — Expand ~ on remote
        let mut remote_root = remote_path.clone();
        if remote_root.starts_with('~') {
            let mut channel = sess
                .channel_session()
                .with_context(|| "无法打开远端 shell 来解析 ~")?;
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

        let meta = sftp.stat(std::path::Path::new(&remote_root)).ok();

        // 枚举远端文件（支持目录、通配或单文件） — Enumerate remote files (support dir, glob, or single)
        let mut remote_files: Vec<(String, String)> = Vec::new();
        fn wildcard_match(pat: &str, name: &str) -> bool {
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

        let is_glob = remote_root.chars().any(|c| c == '*' || c == '?');
        let explicit_dir_suffix = remote_root.ends_with('/');
        if explicit_dir_suffix && !is_glob {
            if let Ok(st) = sftp.stat(std::path::Path::new(&remote_root)) {
                if st.is_file() {
                    return Err(anyhow::anyhow!(format!(
                        "远端源 '{}' 以 '/' 结尾但不是目录",
                        remote_root
                    )));
                }
            } else {
                return Err(anyhow::anyhow!(format!("远端源 '{}' 不存在", remote_root)));
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
                                remote_files.push((full, rel));
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
                for (pathbuf, stat) in entries {
                    if let Some(name) = pathbuf.file_name().and_then(|n| n.to_str()) {
                        if name == "." || name == ".." {
                            continue;
                        }
                        if !stat.is_file() {
                            continue;
                        }
                        if wildcard_match(pattern, name) {
                            let full = format!("{}/{}", parent.trim_end_matches('/'), name);
                            remote_files.push((full, name.to_string()));
                        }
                    }
                }
            }
            if remote_files.is_empty() {
                let fname = std::path::Path::new(&remote_root)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&remote_root)
                    .to_string();
                remote_files.push((remote_root.clone(), fname));
            }
        } else if let Some(m) = &meta {
            if m.is_file() {
                let fname = std::path::Path::new(&remote_root)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&remote_root)
                    .to_string();
                remote_files.push((remote_root.clone(), fname));
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
                                    remote_files.push((full, rel));
                                } else {
                                    q.push_back((full, rel));
                                }
                            }
                        }
                    }
                }
            }
        } else {
            let fname = std::path::Path::new(&remote_root)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&remote_root)
                .to_string();
            remote_files.push((remote_root.clone(), fname));
        }

        let multi_match = remote_files.len() > 1;
        if multi_match {
            let target_path = std::path::Path::new(&target);
            if original_target.ends_with('/') {
                if !target_path.exists() {
                    return Err(anyhow::anyhow!(format!(
                        "本地目标 {} 不存在（需要存在以接收多文件）",
                        target_path.display()
                    )));
                }
                if !target_path.is_dir() {
                    return Err(anyhow::anyhow!(format!(
                        "本地目标 {} 已存在且不是目录",
                        target_path.display()
                    )));
                }
            } else if target_path.exists() {
                if !target_path.is_dir() {
                    return Err(anyhow::anyhow!(format!(
                        "本地目标 {} 已存在且不是目录",
                        target_path.display()
                    )));
                }
            } else {
                std::fs::create_dir_all(target_path)
                    .with_context(|| format!("无法创建本地目标目录: {}", target_path.display()))?;
            }
        }

        let mut total_size: u64 = 0;
        let mut sizes: Vec<u64> = Vec::new();
        for (rf, _rel) in remote_files.iter() {
            if let Ok(st) = sftp.stat(std::path::Path::new(rf)) {
                let s = st.size.unwrap_or(0);
                sizes.push(s);
                total_size += s;
            } else {
                sizes.push(0);
            }
        }

        // 准备进度条与任务队列 — Prepare progress and queue
        let start = Instant::now();
        let mp = Arc::new(if verbose {
            MultiProgress::with_draw_target(ProgressDrawTarget::stdout())
        } else {
            MultiProgress::new()
        });
        let total_pb = mp.add(ProgressBar::new(total_size));
        total_pb.set_style(total_style.clone());

        let remote_files_arc = Arc::new(remote_files);
        let sizes_arc = Arc::new(sizes);
        let total_files = remote_files_arc.len();
        let mut workers = if concurrency == 0 {
            6usize
        } else {
            concurrency
        };
        workers = std::cmp::min(workers, max_allowed_workers);
        workers = std::cmp::min(workers, total_files);

        let (tx, rx) = bounded::<usize>(total_files);
        for i in 0..total_files {
            let _ = tx.send(i);
        }
        drop(tx);
        let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..workers {
            let rx = rx.clone();
            let mp = mp.clone();
            let total_pb = total_pb.clone();
            let server = server.clone();
            let target = target.clone();
            let remote_files_arc = remote_files_arc.clone();
            let sizes_arc = sizes_arc.clone();
            let failures = failures.clone();
            let handle = std::thread::spawn(move || {
                let mut worker_pb: Option<ProgressBar> = None;
                while let Ok(idx) = rx.recv() {
                    let (remote_full, rel) = &remote_files_arc[idx];
                    let file_name = std::path::Path::new(rel)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(rel.as_str());
                    let local_target = if std::path::Path::new(&target).is_dir() {
                        std::path::Path::new(&target).join(rel)
                    } else {
                        std::path::Path::new(&target).to_path_buf()
                    };
                    if let Some(parent) = local_target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Some(old) = worker_pb.take() {
                        old.finish_and_clear();
                    }
                    let file_size = sizes_arc[idx];
                    let file_pb = mp.add(ProgressBar::new(file_size));
                    file_pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})").unwrap().progress_chars("=> "));
                    file_pb.set_message(rel.clone());
                    worker_pb = Some(file_pb.clone());

                    if let Ok(mut addrs) =
                        format!("{}:{}", server.address, server.port).to_socket_addrs()
                        && let Some(sock) = addrs.next()
                        && let Ok(tcp) = TcpStream::connect_timeout(&sock, Duration::from_secs(10))
                        && let Ok(mut sess) = Session::new().map(|mut s| {
                            s.set_tcp_stream(tcp);
                            s
                        })
                    {
                        if sess.handshake().is_err() {
                            let mut f = failures.lock().unwrap();
                            f.push(format!("handshake failed: {}", remote_full));
                            continue;
                        }
                        let _ = sess.userauth_agent(&server.username);
                        if !sess.authenticated()
                            && let Some(home_p) = dirs::home_dir()
                        {
                            for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                                let p = home_p.join(".ssh").join(name);
                                if p.exists() {
                                    let _ =
                                        sess.userauth_pubkey_file(&server.username, None, &p, None);
                                    if sess.authenticated() {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Ok(sftp) = sess.sftp() {
                            if let Ok(mut remote_f) = sftp.open(std::path::Path::new(remote_full)) {
                                if let Ok(mut local_f) = File::create(&local_target) {
                                    let mut buf = vec![0u8; 1024 * 1024];
                                    loop {
                                        match remote_f.read(&mut buf) {
                                            Ok(0) => break,
                                            Ok(n) => {
                                                if local_f.write_all(&buf[..n]).is_err() {
                                                    let mut f = failures.lock().unwrap();
                                                    f.push(format!(
                                                        "local write failed: {}",
                                                        local_target.display()
                                                    ));
                                                    break;
                                                }
                                                if let Some(ref p) = worker_pb {
                                                    p.inc(n as u64);
                                                }
                                                total_pb.inc(n as u64);
                                            }
                                            Err(_) => {
                                                let mut f = failures.lock().unwrap();
                                                f.push(format!(
                                                    "remote read failed: {}",
                                                    remote_full
                                                ));
                                                break;
                                            }
                                        }
                                    }
                                } else {
                                    let mut f = failures.lock().unwrap();
                                    f.push(format!(
                                        "local create failed: {}",
                                        local_target.display()
                                    ));
                                }
                            } else {
                                let mut f = failures.lock().unwrap();
                                f.push(format!("remote open failed: {}", remote_full));
                            }
                        }
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

        for h in handles {
            let _ = h.join();
        }
        let _ = mp.clear();
        total_pb.finish_with_message("下载完成");
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

        let failures_guard = failures.lock().unwrap();
        if !failures_guard.is_empty() {
            eprintln!("下载失败文件列表:");
            for f in failures_guard.iter() {
                eprintln!(" - {}", f);
            }
            // Delegate writing failures to helper
            write_failures(output_failures.clone(), &failures_guard);
        }

        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "无法判定传输方向：请确保目标或第一个源使用 alias:/path 格式（例如 host:~/path）"
        ))
    }
}

/// Write failures to either a user-specified path or the default hostpilot logs directory.
pub fn write_failures(output_failures: Option<std::path::PathBuf>, failures: &[String]) {
    if failures.is_empty() {
        return;
    }
    if let Some(out) = &output_failures {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
        {
            Ok(mut ffile) => {
                let timestamp = chrono::Utc::now().format("%Y%m%d").to_string();
                let _ = writeln!(ffile, "Transfer failures ({}):", timestamp);
                for line in failures.iter() {
                    let _ = writeln!(ffile, "{}", line);
                }
            }
            Err(e) => eprintln!("⚠️ 无法写入指定失败文件 {}: {}", out.display(), e),
        }
    } else if let Some(home) = dirs::home_dir()
        && let Ok(base) = crate::ops::ensure_hostpilot_dir(&home)
    {
        let logs_dir = base.join("logs");
        let _ = std::fs::create_dir_all(&logs_dir);
        let timestamp = chrono::Utc::now().format("%Y%m%d").to_string();
        let fail_path = logs_dir.join(format!("failures_{}.log", timestamp));
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&fail_path)
        {
            Ok(mut ffile) => {
                let _ = writeln!(ffile, "Transfer failures ({}):", timestamp);
                for line in failures.iter() {
                    let _ = writeln!(ffile, "{}", line);
                }
            }
            Err(e) => eprintln!("⚠️ 无法写入失败文件: {}: {}", fail_path.display(), e),
        }
    }
}
