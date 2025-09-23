use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use hostpilot::config::Config;
use hostpilot::server::ServerCollection;

// This integration test verifies that `handle_ts` returns quickly (with an error)
// when the remote session cannot be established and that providing a retry
// count does not hang the process. It uses a fake server entry pointing to
// a high-numbered port on localhost where no SSH server is expected to run.

#[test]
fn retry_option_triggers_and_returns_err() {
    // prepare a temporary sqlite path inside temp dir
    let mut db_path = std::env::temp_dir();
    db_path.push(format!("hostpilot_test_servers_{}.db", chrono::Utc::now().timestamp_millis()));

    // create a server collection with a server that points to localhost:65000
    let mut coll = ServerCollection::default();
    let server = hostpilot::server::Server {
        id: None,
        alias: Some("fakehost".to_string()),
        username: "nobody".to_string(),
        address: "127.0.0.1".to_string(),
        port: 65000u16,
        last_connect: None,
    };
    coll.insert("fakehost", server);
    let _ = coll.save_to_storage(&db_path);

    // construct a Config pointing to that server DB
    let cfg = Config {
        pub_key_path: PathBuf::from("~/.ssh/id_rsa.pub"),
        server_file_path: db_path.clone(),
        ssh_client_app_path: PathBuf::from("ssh"),
        scp_app_path: PathBuf::from("scp"),
        version: Some(2),
        mode: 0,
    };

    // create a small temporary local file to upload
    let mut local = std::env::temp_dir();
    local.push(format!("hostpilot_test_file_{}.txt", chrono::Utc::now().timestamp_millis()));
    let mut f = File::create(&local).expect("create temp file");
    let _ = f.write_all(b"hello world\n");
    drop(f);

    // call handle_ts to upload to fakehost, with retry=2 and concurrency=1
    let sources = vec![local.to_string_lossy().to_string()];
    let target = "fakehost:~/upload_dest".to_string();

    let args = hostpilot::transfer::HandleTsArgs {
        sources,
        target,
        verbose: false,
        concurrency: 1,
        output_failures: None,
        max_retries: 2,
        buf_size: 1024 * 1024,
    };
    let res = hostpilot::transfer::handle_ts(&cfg, args);

    // We expect an error because there's no SSH server at 127.0.0.1:65000
    assert!(res.is_err(), "expected handle_ts to return Err when SSH unavailable");
}
