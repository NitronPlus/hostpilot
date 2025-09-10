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
}

impl Config {
    pub fn init() -> Self {
        match dirs::home_dir() {
            Some(home_dir) => {
                let psm_config_storage_dir = home_dir.join(".".to_owned() + env!("CARGO_PKG_NAME"));
                let pub_key_path = home_dir.join(".ssh").join("id_rsa.pub");
                let server_db_path = psm_config_storage_dir.join("server.db");
                let psm_config_path = psm_config_storage_dir.join("config.json");
                if !psm_config_storage_dir.exists() {
                    if let Err(e) = std::fs::create_dir(&psm_config_storage_dir) {
                        eprintln!(
                            "⚠️ 无法创建配置目录 {}: {}",
                            psm_config_storage_dir.display(),
                            e
                        );
                    }
                    let config = Config {
                        pub_key_path,
                        server_file_path: server_db_path,
                        ssh_client_app_path: PathBuf::from("ssh"),
                        scp_app_path: PathBuf::from("scp"),
                        // 新安装直接使用最新版本与SQLite
                        version: Some(2),
                    };
                    config.save_to(&psm_config_path);
                }
                Config::read_from(psm_config_path)
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
            let psm_config_storage_dir = home_dir.join(".".to_string() + env!("CARGO_PKG_NAME"));
            let psm_config_path = psm_config_storage_dir.join("config.json");
            self.save_to(psm_config_path);
        } else {
            eprintln!("⚠️ 无法找到 home 目录，无法保存配置");
        }
    }
}
