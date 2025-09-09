use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::StorageObject;
use crate::server::ServerCollection;

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Config {
    pub pub_key_path: PathBuf,
    pub server_file_path: PathBuf,
    pub ssh_client_app_path: PathBuf,
    pub scp_app_path: PathBuf,
    pub version: Option<String>
}

impl Config {
    pub fn init() -> Self {
        match dirs::home_dir() {
            Some(home_dir) => {
                let psm_config_storage_dir = home_dir.join(".".to_owned() + env!("CARGO_PKG_NAME"));
                let pub_key_path = home_dir.join(".ssh").join("id_rsa.pub");
                let server_file_path = psm_config_storage_dir.join("server.json");
                let psm_config_path = psm_config_storage_dir.join("config.json");
                if !psm_config_storage_dir.exists() {
                    std::fs::create_dir(&psm_config_storage_dir).unwrap();
                    ServerCollection::init(&server_file_path);
                    let config = Config {
                        pub_key_path,
                        server_file_path,
                        ssh_client_app_path: PathBuf::from("ssh"),
                        scp_app_path: PathBuf::from("scp"),
                        version: "1".to_string().into(),
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
}
