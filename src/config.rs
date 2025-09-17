use std::path::PathBuf;

use crate::app::StorageObject;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Config {
    pub pub_key_path: PathBuf,
    // 字段名保持为 server_file_path；兼容上一版的 server_db_path（向后兼容） — Field name kept as server_file_path; compatible with previous server_db_path (backward compatibility)
    #[serde(alias = "server_db_path")]
    pub server_file_path: PathBuf,
    pub ssh_client_app_path: PathBuf,
    pub scp_app_path: PathBuf,
    pub version: Option<u32>,
    #[serde(skip)]
    pub mode: u8,
}

impl Config {
    pub fn init(mode: u8) -> Self {
        match dirs::home_dir() {
            Some(home_dir) => {
                // Ensure we use ~/.hostpilot and migrate legacy ~/.psm if needed
                let config_storage_dir = match crate::ops::ensure_hostpilot_dir(&home_dir) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("⚠️ 无法准备配置目录: {}", e);
                        std::process::exit(1);
                    }
                };
                let pub_key_path = home_dir.join(".ssh").join("id_rsa.pub");
                let server_db_path = config_storage_dir.join("server.db");
                let config_file_path = config_storage_dir.join("config.json");
                // 根据 mode 决定是否优先使用 test 配置文件（mode==1 表示 test 模式）
                let chosen_config = if mode == 1 {
                    let test_path = config_storage_dir.join("config_test.json");
                    if test_path.exists() { test_path } else { config_file_path.clone() }
                } else {
                    config_file_path.clone()
                };
                if !config_storage_dir.exists() {
                    if let Err(e) = std::fs::create_dir(&config_storage_dir) {
                        eprintln!("⚠️ 无法创建配置目录 {}: {}", config_storage_dir.display(), e);
                    }
                    let config = Config {
                        pub_key_path,
                        server_file_path: server_db_path,
                        ssh_client_app_path: PathBuf::from("ssh"),
                        scp_app_path: PathBuf::from("scp"),
                        // 新安装直接使用最新版本与SQLite
                        version: Some(2),
                        mode,
                    };
                    config.save_to(&config_file_path);
                }
                let mut conf: Config = Config::read_from(chosen_config);
                conf.mode = mode;
                conf
            }
            None => {
                println!("Cannot find user's home dir");
                std::process::exit(1);
            }
        }
    }

    /// 将配置保存回 $HOME/.{pkgname}/config.json — Save config back to expected config.json under $HOME/.{pkgname}/config.json
    pub fn save_to_storage(&self) {
        if let Some(home_dir) = dirs::home_dir() {
            let config_storage_dir = match crate::ops::ensure_hostpilot_dir(&home_dir) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("⚠️ 无法准备配置目录: {}", e);
                    return;
                }
            };
            // 根据 mode 决定写回到哪一个配置文件；mode==1 时写回 config_test.json
            let config_path = if self.mode == 1 {
                config_storage_dir.join("config_test.json")
            } else {
                config_storage_dir.join("config.json")
            };
            self.save_to(&config_path);
        } else {
            eprintln!("⚠️ 无法找到 home 目录，无法保存配置");
        }
    }
}
