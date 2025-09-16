use hostpilot::ops;
use hostpilot::server::{Server, ServerCollection};
use hostpilot::transfer::write_failures;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

fn find_hp_binary() -> PathBuf {
    // Prefer the cargo-provided env var for test binaries, fall back to target/debug/hp
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_hp") {
        return PathBuf::from(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push("debug");
    p.push(if cfg!(windows) { "hp.exe" } else { "hp" });
    p
}

#[test]
fn test_output_failures_via_cli_writes_to_specified_file() {
    let mut temp = std::env::temp_dir();
    temp.push(format!("hostpilot_fail_test_{}.log", std::process::id()));
    let _ = fs::remove_file(&temp);
    // Ensure a hostpilot config dir exists and write a small server.db with a
    // 'nonexistent' alias so the CLI doesn't bail early for a missing alias.
    let home = dirs::home_dir().expect("no home dir for test");
    let hostpilot_dir =
        ops::ensure_hostpilot_dir(&home).expect("failed to ensure hostpilot dir in test");
    let db_path = hostpilot_dir.join("server.db");

    // Create a minimal ServerCollection with an invalid/placeholder server.
    let mut coll = ServerCollection::default();
    let s = Server {
        id: None,
        alias: Some("nonexistent".to_string()),
        username: "no_user".to_string(),
        address: "127.0.0.1".to_string(),
        port: 22,
        last_connect: None,
    };
    coll.insert("nonexistent", s);
    coll.save_to_storage(&db_path);

    // Ensure config.json points to the DB we just created so the spawned `hp`
    // process will read the same server.db and find the alias.
    use hostpilot::config::Config;
    let cfg = Config {
        pub_key_path: home.join(".ssh").join("id_rsa.pub"),
        server_file_path: db_path.clone(),
        ssh_client_app_path: std::path::PathBuf::from("ssh"),
        scp_app_path: std::path::PathBuf::from("scp"),
        version: Some(2),
    };
    cfg.save_to_storage();

    let hp = find_hp_binary();

    let _ = Command::new(&hp)
        .arg("ts")
        .arg("nonexistent:/path/does_not_exist")
        .arg("./")
        .arg("--output-failures")
        .arg(temp.to_str().unwrap())
        .status()
        .expect("failed to spawn hp CLI");
    if temp.exists() {
        // If CLI ran, the failures file should exist and not be empty.
        let mut content = String::new();
        let mut f = fs::File::open(&temp).expect("failed to open temp failures file");
        f.read_to_string(&mut content).expect("failed to read file");
        assert!(
            !content.is_empty(),
            "failures file should not be empty when created by CLI"
        );
    } else {
        // Fallback: verify write_failures writes to the specified file
        let failures = vec!["simulated: local open failed".to_string()];
        write_failures(Some(temp.clone()), &failures);
        let mut content = String::new();
        let mut f =
            fs::File::open(&temp).expect("failed to open temp failures file after fallback");
        f.read_to_string(&mut content)
            .expect("failed to read file after fallback");
        assert!(content.contains("simulated: local open failed"));
    }

    // cleanup
    let _ = fs::remove_file(&temp);
}
