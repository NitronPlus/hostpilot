use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_hp_binary() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_hp") {
        return PathBuf::from(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push("debug");
    p.push(if cfg!(windows) { "hp.exe" } else { "hp" });
    p
}

fn get_hdev_server() -> Option<hostpilot::server::Server> {
    // Load config: prefer $HOME/.hostpilot/config_test.json, then repo-root config_test.json, else default
    let mut cfg: Option<hostpilot::config::Config> = None;
    if let Some(home) = dirs::home_dir()
        && let Ok(base) = hostpilot::ops::ensure_hostpilot_dir(&home)
    {
        let home_cfg = base.join("config_test.json");
        if home_cfg.exists()
            && let Ok(s) = std::fs::read_to_string(&home_cfg)
            && let Ok(c) = serde_json::from_str::<hostpilot::config::Config>(&s)
        {
            cfg = Some(c);
        }
    }
    if cfg.is_none() {
        let mut repo_cfg = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        repo_cfg.push("config_test.json");
        if repo_cfg.exists()
            && let Ok(s) = std::fs::read_to_string(&repo_cfg)
            && let Ok(c) = serde_json::from_str::<hostpilot::config::Config>(&s)
        {
            cfg = Some(c);
        }
    }
    let cfg = cfg.unwrap_or_else(|| hostpilot::config::Config::init(1));
    let coll =
        hostpilot::server::ServerCollection::read_from_storage(&cfg.server_file_path).ok()?;
    coll.get("hdev").cloned()
}

fn remote_home(sess: &mut ssh2::Session) -> String {
    let mut ch = sess.channel_session().expect("open channel");
    ch.exec("echo $HOME").ok();
    let mut s = String::new();
    ch.read_to_string(&mut s).ok();
    ch.wait_close().ok();
    s.lines().next().unwrap_or("~").trim().to_string()
}

fn list_remote_tree(sftp: &ssh2::Sftp, root: &Path) -> Vec<(String, bool)> {
    // returns (relative_path, is_dir)
    fn walk(acc: &mut Vec<(String, bool)>, sftp: &ssh2::Sftp, base: &Path, cur: &Path) {
        if let Ok(entries) = sftp.readdir(cur) {
            for (p, st) in entries {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "." || name == ".." {
                    continue;
                }
                let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
                let is_dir = st.is_dir();
                acc.push((rel.clone(), is_dir));
                if is_dir {
                    walk(acc, sftp, base, &p);
                }
            }
        }
    }
    let mut v = Vec::new();
    walk(&mut v, sftp, root, root);
    v.sort();
    v
}

fn remove_remote_tree(sftp: &ssh2::Sftp, root: &Path) {
    // best-effort recursive delete
    if let Ok(entries) = sftp.readdir(root) {
        for (p, st) in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "." || name == ".." {
                continue;
            }
            if st.is_dir() {
                remove_remote_tree(sftp, &p);
                let _ = sftp.rmdir(&p);
            } else {
                let _ = sftp.unlink(&p);
            }
        }
    }
    let _ = sftp.rmdir(root);
}

#[test]
fn ts_dir_and_dir_slash_equivalent_e2e() {
    let Some(server) = get_hdev_server() else {
        eprintln!("SKIP: alias 'hdev' not found in server DB");
        return;
    };

    // Prepare local source directory with nested files
    let mut src_root = std::env::temp_dir();
    src_root.push(format!("hp_e2e_dir_equiv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&src_root);
    std::fs::create_dir_all(src_root.join("nested/sub")).expect("create local dirs");
    std::fs::write(src_root.join("a.txt"), b"aaa").unwrap();
    std::fs::write(src_root.join("nested").join("b.txt"), b"bbb").unwrap();
    std::fs::write(src_root.join("nested/sub").join("c.txt"), b"ccc").unwrap();

    // Remote targets
    let hp = find_hp_binary();
    let remote_base = format!("~/hp_e2e_dir_equiv_{}", std::process::id());
    let remote_a = format!("{}/A", remote_base);
    let remote_b = format!("{}/B", remote_base);

    let fail_file = std::env::temp_dir().join(format!("hp_e2e_fail_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&fail_file);

    // Upload using dir (no slash)
    let status = Command::new(&hp)
        .arg("ts")
        .arg(src_root.to_string_lossy().as_ref())
        .arg(format!("hdev:{}", remote_a))
        .arg("--output-failures")
        .arg(fail_file.to_string_lossy().as_ref())
        .status()
        .expect("spawn hp failed");
    assert!(status.success(), "hp ts failed (dir)");

    // Upload using dir/ (with slash)
    let mut src_with_slash = src_root.to_string_lossy().to_string();
    if !src_with_slash.ends_with('/') {
        src_with_slash.push('/');
    }
    let status2 = Command::new(&hp)
        .arg("ts")
        .arg(src_with_slash)
        .arg(format!("hdev:{}", remote_b))
        .arg("--output-failures")
        .arg(fail_file.to_string_lossy().as_ref())
        .status()
        .expect("spawn hp failed");
    assert!(status2.success(), "hp ts failed (dir/)");

    // Verify remote A and B contents are identical (contents-only, no container)
    use ssh2::Session;
    let addr = format!("{}:{}", server.address, server.port);
    let tcp = TcpStream::connect(&addr).expect("TCP connect failed");
    let mut sess = Session::new().expect("create SSH session");
    sess.set_tcp_stream(tcp);
    sess.handshake().expect("handshake failed");
    if !sess.authenticated()
        && let Some(home) = dirs::home_dir()
    {
        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let p = home.join(".ssh").join(name);
            if p.exists() {
                let _ = sess.userauth_pubkey_file(&server.username, None, &p, None);
                if sess.authenticated() {
                    break;
                }
            }
        }
    }
    assert!(sess.authenticated(), "SSH authentication failed for alias hdev");
    let sftp = sess.sftp().expect("create sftp");

    // Expand ~ into absolute path for listing/cleanup
    let home = remote_home(&mut sess);
    let expand = |p: &str| -> String {
        if let Some(tail) = p.strip_prefix("~/") {
            format!("{}/{}", home.trim_end_matches('/'), tail)
        } else {
            p.to_string()
        }
    };
    let ra = PathBuf::from(expand(&remote_a));
    let rb = PathBuf::from(expand(&remote_b));

    let a_items = list_remote_tree(&sftp, &ra);
    let b_items = list_remote_tree(&sftp, &rb);
    assert_eq!(a_items, b_items, "remote A and B trees differ (dir vs dir/ should be equivalent)");

    // Cleanup remote
    remove_remote_tree(&sftp, &ra);
    remove_remote_tree(&sftp, &rb);

    // Cleanup local
    let _ = std::fs::remove_dir_all(&src_root);
}
