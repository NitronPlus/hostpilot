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

fn list_remote_names(sftp: &ssh2::Sftp, root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(entries) = sftp.readdir(root) {
        for (p, _st) in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "." || name == ".." {
                continue;
            }
            v.push(name.to_string());
        }
    }
    v.sort();
    v
}

#[test]
fn ts_dir_glob_non_recursive_e2e() {
    let Some(server) = get_hdev_server() else {
        eprintln!("SKIP: alias 'hdev' not found in server DB");
        return;
    };

    // Prepare local structure: d1/{f1}, d2/sub/{f2}
    let mut src_root = std::env::temp_dir();
    src_root.push(format!("hp_e2e_glob_nonrec_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&src_root);
    std::fs::create_dir_all(src_root.join("d1")).unwrap();
    std::fs::create_dir_all(src_root.join("d2/sub")).unwrap();
    std::fs::write(src_root.join("d1").join("f1.txt"), b"1").unwrap();
    std::fs::write(src_root.join("d2/sub").join("f2.txt"), b"2").unwrap();

    // Pattern that matches only directories at top level: d*
    // According to rules, basename glob expands in parent only and is non-recursive; when it matches directories, we only create directory entries remotely, without recursing their contents.
    let pattern = src_root.join("d*");

    let hp = find_hp_binary();
    let remote_base = format!("~/hp_e2e_glob_nonrec_{}", std::process::id());
    let fail_file = std::env::temp_dir().join(format!("hp_e2e_fail_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&fail_file);

    let status = Command::new(&hp)
        .arg("ts")
        .arg(pattern.to_string_lossy().as_ref())
        .arg(format!("hdev:{}", remote_base))
        .arg("--output-failures")
        .arg(fail_file.to_string_lossy().as_ref())
        .status()
        .expect("spawn hp failed");
    assert!(status.success(), "hp ts failed for glob");

    // Connect and verify only top-level directories exist; no files inside were uploaded
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
    let home = remote_home(&mut sess);
    let expand = |p: &str| -> String {
        if let Some(tail) = p.strip_prefix("~/") {
            format!("{}/{}", home.trim_end_matches('/'), tail)
        } else {
            p.to_string()
        }
    };
    let rbase = PathBuf::from(expand(&remote_base));

    // List immediate children of remote_base
    let top = list_remote_names(&sftp, &rbase);
    assert_eq!(
        top,
        vec!["d1".to_string(), "d2".to_string()],
        "Only directories matched should be created at top-level"
    );

    // Ensure nested files do not exist remotely
    let d1_f = rbase.join("d1/f1.txt");
    let d2_f = rbase.join("d2/sub/f2.txt");
    assert!(sftp.stat(&d1_f).is_err(), "f1.txt should not be uploaded for directory glob match");
    assert!(sftp.stat(&d2_f).is_err(), "f2.txt should not be uploaded for directory glob match");

    // Cleanup remote
    // delete directories if exist
    let _ = sftp.rmdir(&rbase.join("d1"));
    // remove sub first then parent
    let _ = sftp.rmdir(&rbase.join("d2/sub"));
    let _ = sftp.rmdir(&rbase.join("d2"));
    let _ = sftp.rmdir(&rbase);

    // Cleanup local
    let _ = std::fs::remove_dir_all(&src_root);
}
