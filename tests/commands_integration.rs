use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(1);

use hostpilot::{commands, config::Config, server::ServerCollection};

enum Action {
    Create { alias: &'static str, remote: &'static str },
    Rename { alias: &'static str, new_alias: &'static str },
    Remove { alias: &'static str },
}

struct Case {
    actions: &'static [Action],
    expect_exists: &'static [&'static str],
    expect_not_exists: &'static [&'static str],
}

fn unique_db_path() -> PathBuf {
    let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let pid = std::process::id();
    let cnt = TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hostpilot_test_{}_{}_{}.db", now_ns, pid, cnt))
}

fn make_cfg(db_path: PathBuf) -> Config {
    Config {
        pub_key_path: PathBuf::from("/dev/null"),
        server_file_path: db_path,
        ssh_client_app_path: PathBuf::from("ssh"),
        scp_app_path: PathBuf::from("scp"),
        version: Some(2),
        mode: 1,
    }
}

fn run_actions(cfg: &Config, actions: &[Action]) {
    for act in actions.iter() {
        match act {
            Action::Create { alias, remote } => {
                let _ = commands::handle_create(cfg, alias.to_string(), remote.to_string());
            }
            Action::Rename { alias, new_alias } => {
                let _ = commands::handle_rename(cfg, alias.to_string(), new_alias.to_string());
            }
            Action::Remove { alias } => {
                let _ = commands::handle_remove(cfg, alias.to_string());
            }
        }
    }
}

fn assert_state(cfg: &Config, exists: &[&str], not_exists: &[&str]) {
    let col = ServerCollection::read_from_storage(&cfg.server_file_path)
        .expect("failed to read server collection in test");
    for name in exists.iter() {
        assert!(col.get(name).is_some(), "expected '{}' to exist", name);
    }
    for name in not_exists.iter() {
        assert!(col.get(name).is_none(), "expected '{}' to not exist", name);
    }
}

#[test]
fn command_tests_helper_driven() {
    let cases: &[Case] = &[
        Case {
            actions: &[
                Action::Create { alias: "testalias", remote: "user@localhost:2222" },
                Action::Rename { alias: "testalias", new_alias: "newalias" },
                Action::Remove { alias: "newalias" },
            ],
            expect_exists: &[],
            expect_not_exists: &["testalias", "newalias"],
        },
        Case {
            actions: &[
                Action::Create { alias: "dupalias", remote: "u@h" },
                Action::Create { alias: "dupalias", remote: "u@h" },
            ],
            expect_exists: &["dupalias"],
            expect_not_exists: &[],
        },
        Case {
            actions: &[
                Action::Create { alias: "a1", remote: "u1@h" },
                Action::Create { alias: "a2", remote: "u2@h" },
                Action::Rename { alias: "a1", new_alias: "a2" },
            ],
            expect_exists: &["a1", "a2"],
            expect_not_exists: &[],
        },
        Case {
            actions: &[Action::Remove { alias: "nope" }],
            expect_exists: &[],
            expect_not_exists: &["nope"],
        },
        Case {
            actions: &[Action::Create { alias: "bad1", remote: "no-at-symbol" }],
            expect_exists: &[],
            expect_not_exists: &["bad1"],
        },
        Case {
            actions: &[Action::Rename { alias: "nope", new_alias: "x" }],
            expect_exists: &[],
            expect_not_exists: &["nope", "x"],
        },
        Case {
            actions: &[
                Action::Create { alias: "same", remote: "u@h" },
                Action::Rename { alias: "same", new_alias: "same" },
            ],
            expect_exists: &["same"],
            expect_not_exists: &[],
        },
        Case {
            actions: &[
                Action::Create { alias: "todel", remote: "u@h" },
                Action::Remove { alias: "todel" },
                Action::Remove { alias: "todel" },
            ],
            expect_exists: &[],
            expect_not_exists: &["todel"],
        },
    ];

    for case in cases.iter() {
        let db_path = unique_db_path();
        let _ = fs::remove_file(&db_path);
        let cfg = make_cfg(db_path.clone());

        run_actions(&cfg, case.actions);
        assert_state(&cfg, case.expect_exists, case.expect_not_exists);

        let _ = fs::remove_file(&db_path);
    }
}
