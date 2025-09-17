use std::io::Read;
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

fn find_hp_binary() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_hp") {
        return std::path::PathBuf::from(p);
    }
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push("debug");
    p.push(if cfg!(windows) { "hp.exe" } else { "hp" });
    p
}

#[test]
fn handle_ts_e2e_with_hdev() {
    // E2E test always runs in this environment as requested; it will perform
    // automatic cleanup of the remote test file.

    // Load config: prefer `$HOME/.hostpilot/config_test.json`, then repo-root `config_test.json`, else fallback to runtime default
    let mut cfg: Option<hostpilot::config::Config> = None;

    // 1) try $HOME/.hostpilot/config_test.json
    if let Some(home) = dirs::home_dir()
        && let Ok(base) = hostpilot::ops::ensure_hostpilot_dir(&home)
    {
        let home_cfg = base.join("config_test.json");
        if home_cfg.exists()
            && let Ok(s) = std::fs::read_to_string(&home_cfg)
        {
            if let Ok(c) = serde_json::from_str::<hostpilot::config::Config>(&s) {
                cfg = Some(c);
            } else {
                eprintln!(
                    "WARN: failed to parse $HOME/.hostpilot/config_test.json, will try repo-root config_test.json"
                );
            }
        }
    }

    // 2) try repo-root config_test.json
    if cfg.is_none() {
        let mut repo_cfg = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        repo_cfg.push("config_test.json");
        if repo_cfg.exists()
            && let Ok(s) = std::fs::read_to_string(&repo_cfg)
        {
            if let Ok(c) = serde_json::from_str::<hostpilot::config::Config>(&s) {
                cfg = Some(c);
            } else {
                eprintln!(
                    "WARN: failed to parse repo-root config_test.json, falling back to default config"
                );
            }
        }
    }

    // 3) fallback
    let cfg = cfg.unwrap_or_else(|| hostpilot::config::Config::init(1));
    let coll = hostpilot::server::ServerCollection::read_from_storage(&cfg.server_file_path)
        .expect("failed to read server collection in test");
    let server = match coll.get("hdev") {
        Some(s) => s,
        None => {
            eprintln!("SKIP: alias 'hdev' not found in server DB");
            return;
        }
    };

    // Use an existing test file from the repo as the upload source
    let mut local_file = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    local_file.push("tests");
    local_file.push("transfer_integration.rs");

    // Remote target directory under user's home
    let remote_dir = "~/hostpilot_test_e2e";
    let remote_target = format!("hdev:{}", remote_dir);

    let fail_file = std::env::temp_dir().join(format!("hp_e2e_fail_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&fail_file);

    // Run hp CLI to upload
    let hp = find_hp_binary();
    let status = Command::new(&hp)
        .arg("ts")
        .arg(local_file.to_string_lossy().as_ref())
        .arg(&remote_target)
        .arg("--output-failures")
        .arg(fail_file.to_string_lossy().as_ref())
        .status()
        .expect("failed to spawn hp CLI");

    if !status.success() {
        // If CLI failed, surface failures file if present
        if fail_file.exists() {
            let mut s = String::new();
            let mut f = std::fs::File::open(&fail_file).expect("open failures file");
            f.read_to_string(&mut s).ok();
            panic!("hp ts failed; failures file:\n{}", s);
        }
        panic!("hp ts exited with status: {:?}", status);
    }

    // Now connect with ssh2 and verify remote file exists
    use ssh2::Session;

    let addr = format!("{}:{}", server.address, server.port);
    let tcp = TcpStream::connect(&addr).expect("TCP connect to server failed");
    let mut sess = Session::new().expect("create SSH session");
    sess.set_tcp_stream(tcp);
    sess.handshake().expect("SSH handshake");

    // Try agent and then keys (same heuristics as transfer)
    // Do not rely on ssh-agent in tests; rely on pubkey files or skip if unavailable
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

    // Expand ~ on remote to compute full path
    let mut channel = sess.channel_session().expect("open channel");
    channel.exec("echo $HOME").ok();
    let mut home_out = String::new();
    channel.read_to_string(&mut home_out).ok();
    channel.wait_close().ok();
    let remote_home = home_out.lines().next().unwrap_or("~").trim().to_string();

    let local_basename = local_file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .expect("local file has no basename");
    let full_remote = format!(
        "{}/{}{}",
        remote_home.trim_end_matches('/'),
        remote_dir.trim_start_matches('~').trim_start_matches('/'),
        ""
    ) + "/"
        + &local_basename;

    // Stat remote file
    let stat = sftp.stat(Path::new(&full_remote));
    match stat {
        Ok(st) => {
            assert!(st.is_file(), "remote path is not a file");
            assert!(st.size.unwrap_or(0) > 0, "remote file has zero size");
        }
        Err(e) => panic!("remote file not found: {} (err: {})", full_remote, e),
    }

    // Automatic cleanup: remove the uploaded remote file and attempt to
    // remove its parent directory (best-effort).
    let _ = sftp.unlink(Path::new(&full_remote));
    if let Some(parent) = Path::new(&full_remote).parent() {
        let _ = sftp.rmdir(parent);
    }
}
