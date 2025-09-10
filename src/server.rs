use std::collections::BTreeMap;
use std::path::Path;

use cli_table::{format::Justify, print_stdout, Cell, CellStruct, Style, Table};
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};

// no JSON persistence for servers at runtime; config uses StorageObject in config.rs only

const PROTOCOL_VERSION: u32 = 2;

pub const fn get_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ServerCollection {
    hosts: BTreeMap<String, Server>,
}

impl ServerCollection {

    pub fn read_from_storage<P: AsRef<Path>>(path: P) -> Self {
        Self::read_from_sqlite(path)
    }

    fn read_from_sqlite<P: AsRef<Path>>(path: P) -> Self {
        let conn = Connection::open(path).expect("Failed to open SQLite database");

        // Create table if not exists (new schema with id + alias unique)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS servers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                alias TEXT UNIQUE NOT NULL,
                username TEXT NOT NULL,
                address TEXT NOT NULL,
                port INTEGER NOT NULL,
                last_connect TEXT
            )",
            [],
        )
        .expect("Failed to create table");

        let mut stmt = conn
            .prepare("SELECT id, alias, username, address, port, last_connect FROM servers")
            .expect("Failed to prepare statement");
        let server_iter = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let alias: String = row.get(1)?;
                let s = Server {
                    id: Some(id),
                    alias: Some(alias.clone()),
                    username: row.get(2)?,
                    address: row.get(3)?,
                    port: row.get(4)?,
                    last_connect: row.get(5)?,
                };
                Ok((alias, s))
            })
            .expect("Failed to query servers");

        let mut hosts = BTreeMap::new();
        for server_result in server_iter {
            let (alias, server) = server_result.expect("Failed to read server");
            hosts.insert(alias, server);
        }

        ServerCollection { hosts }
    }

    pub fn save_to_storage<P: AsRef<Path>>(&self, path: P) {
        self.save_to_sqlite(path)
    }

    fn save_to_sqlite<P: AsRef<Path>>(&self, path: P) {
        let conn = Connection::open(path).expect("Failed to open SQLite database");

        // Create table if not exists (new schema with id + alias unique)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS servers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                alias TEXT UNIQUE NOT NULL,
                username TEXT NOT NULL,
                address TEXT NOT NULL,
                port INTEGER NOT NULL,
                last_connect TEXT
            )",
            [],
        )
        .expect("Failed to create table");

        // Clear existing data
        conn.execute("DELETE FROM servers", [])
            .expect("Failed to clear table");

        // Insert servers (let DB assign id)
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO servers (alias, username, address, port, last_connect) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .expect("Failed to prepare insert statement");

        for (alias, server) in &self.hosts {
            stmt
                .execute(params![
                    alias,
                    server.username,
                    server.address,
                    server.port as i64,
                    server.last_connect,
                ])
                .expect("Failed to insert server");
        }
    }

    pub fn get(&self, key: &String) -> Option<&Server> {
        self.hosts.get(key)
    }

    pub fn insert(&mut self, key: &String, mut server: Server) -> &mut Self {
        // Ensure alias field is filled to keep consistency
        if server.alias.is_none() {
            server.alias = Some(key.clone());
        }
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub alias: Option<String>,
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
