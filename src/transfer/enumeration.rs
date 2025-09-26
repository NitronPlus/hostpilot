use anyhow::Result;
use std::collections::VecDeque;

use super::wildcard_match;
use super::{EntryKind, FileEntry};

// enumerate local sources per rules (R3/R4/R9)
pub(super) fn enumerate_local_sources(sources: &[String]) -> Result<(Vec<FileEntry>, u64)> {
    use walkdir::WalkDir;
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut total_size: u64 = 0;
    for src in sources {
        let src_norm = crate::transfer::helpers::normalize_path(src, false);
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
                                return Err(crate::TransferError::WorkerIo(format!(
                                    "本地 stat 失败: {} — {}",
                                    full.display(),
                                    e
                                ))
                                .into());
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
                    return Err(crate::TransferError::GlobNoMatches(src.clone()).into());
                }
            } else {
                return Err(crate::TransferError::WorkerIo(format!(
                    "无法读取目录: {}",
                    parent.display()
                ))
                .into());
            }
        } else {
            let p = std::path::Path::new(&src_norm);
            if ends_slash {
                if !p.exists() || !p.is_dir() {
                    return Err(crate::TransferError::WorkerIo(format!(
                        "源以 '/' 结尾但不是目录: {} (本地)",
                        src
                    ))
                    .into());
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
                    return Err(crate::TransferError::WorkerIo(format!(
                        "源不存在: {} (本地)",
                        src
                    ))
                    .into());
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
pub(super) fn enumerate_remote_and_push(
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
