use std::collections::BTreeMap;
use std::path::Path;

use cli_table::{format::Justify, print_stdout, Cell, CellStruct, Style, Table};
use serde::{Deserialize, Serialize};

use crate::app::StorageObject;

const PROTOCOL_VERSION: u32 = 1;

pub const fn get_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ServerCollection {
    hosts: BTreeMap<String, Server>,
}

impl ServerCollection {
    pub fn init(path: &Path) {
        ServerCollection::default().save_to(path);
    }

    pub fn get(&self, key: &String) -> Option<&Server> {
        self.hosts.get(key)
    }

    pub fn insert(&mut self, key: &String, server: Server) -> &mut Self {
        self.hosts.insert(key.to_owned(), server);
        self
    }

    pub fn remove(&mut self, key: &String) -> &mut Self {
        self.hosts.remove(key);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub fn hosts(&self) -> &BTreeMap<String, Server> {
        &self.hosts
    }

    pub fn show_table(&self) {
        if !self.is_empty() {
            let title = vec![
                "Alias".cell().bold(true),
                "Username".cell().bold(true),
                "Address".cell().bold(true),
                "Port".cell().bold(true),
                "Last Connect".cell().bold(true),
            ];
            let mut table: Vec<Vec<CellStruct>> = Vec::new();
            for (alias, server) in &self.hosts {
                let port = server.port;
                let last_connect = server.get_last_connect_display();
                let col = vec![
                    alias.cell(),
                    server.username.to_string().cell().justify(Justify::Right),
                    server.address.to_string().cell().justify(Justify::Right),
                    port.cell().justify(Justify::Right),
                    last_connect.cell().justify(Justify::Right),
                ];
                table.push(col);
            }
            print_stdout(table.table().title(title)).unwrap();
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Server {
    pub username: String,
    pub address: String,
    pub port: u16,
    #[serde(default)]
    pub last_connect: Option<String>,
}

impl Server {
    pub fn get_last_connect_display(&self) -> String {
        match &self.last_connect {
            Some(timestamp_str) if !timestamp_str.is_empty() => {
                // 尝试解析时间戳字符串并格式化显示
                match timestamp_str.parse::<i64>() {
                    Ok(timestamp) => {
                        // 假设时间戳是秒级别的，使用本地时间
                        let dt = chrono::DateTime::from_timestamp(timestamp, 0);
                        match dt {
                            Some(dt) => {
                                // 转换为本地时间并格式化
                                let local_dt = dt.with_timezone(&chrono::Local);
                                local_dt.format("%Y-%m-%d %H:%M:%S").to_string()
                            }
                            None => "Invalid timestamp".to_string(),
                        }
                    }
                    Err(_) => timestamp_str.clone(),
                }
            }
            _ => "Never".to_string(),
        }
    }

    pub fn set_last_connect_now(&mut self) {
        let now = chrono::Local::now().timestamp().to_string();
        self.last_connect = Some(now);
    }
}
