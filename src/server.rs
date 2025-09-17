use std::collections::BTreeMap;
use std::path::Path;

use cli_table::{Cell, CellStruct, Style, Table, format::Justify, print_stdout};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

// 运行时不使用 JSON 持久化服务器；配置使用 config.rs 中的 StorageObject —— No JSON persistence for servers at runtime; config uses StorageObject in config.rs only

const PROTOCOL_VERSION: u32 = 2;

pub const fn get_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ServerCollection {
    hosts: BTreeMap<String, Server>,
}

impl ServerCollection {
    pub fn read_from_storage<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        Self::read_from_sqlite(path)
    }

    fn read_from_sqlite<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let conn = Connection::open(path).with_context(|| "Failed to open SQLite database")?;

        // 如果尚不存在则创建表（使用新 schema：id + alias 唯一） — Create table if not exists (new schema with id + alias unique)
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
        .with_context(|| "Failed to create table")?;

        let mut stmt = conn
            .prepare("SELECT id, alias, username, address, port, last_connect FROM servers")
            .with_context(|| "Failed to prepare statement")?;
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
            .with_context(|| "Failed to query servers")?;

        let mut hosts = BTreeMap::new();
        for server_result in server_iter {
            let (alias, server) = server_result.with_context(|| "Failed to read server row")?;
            hosts.insert(alias, server);
        }

        Ok(ServerCollection { hosts })
    }

    pub fn save_to_storage<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        self.save_to_sqlite(path)
    }

    fn save_to_sqlite<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let conn = Connection::open(path).with_context(|| "Failed to open SQLite database")?;

        // 如果尚不存在则创建表（使用新 schema：id + alias 唯一） — Create table if not exists (new schema with id + alias unique)
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
        .with_context(|| "Failed to create table")?;

        // 清空现有数据 — Clear existing data
        conn.execute("DELETE FROM servers", []).with_context(|| "Failed to clear table")?;

        // 插入服务器（让数据库分配 id） — Insert servers (let DB assign id)
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO servers (alias, username, address, port, last_connect) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .with_context(|| "Failed to prepare insert statement")?;

        for (alias, server) in &self.hosts {
            stmt.execute(params![
                alias,
                server.username,
                server.address,
                server.port as i64,
                server.last_connect,
            ])
            .with_context(|| "Failed to insert server")?;
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&Server> {
        self.hosts.get(key)
    }

    pub fn insert(&mut self, key: &str, mut server: Server) -> &mut Self {
        // 确保 alias 字段被填充以保持一致性 — Ensure alias field is filled to keep consistency
        if server.alias.is_none() {
            server.alias = Some(key.to_string());
        }
        self.hosts.insert(key.to_owned(), server);
        self
    }

    pub fn remove(&mut self, key: &str) -> &mut Self {
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
            if let Err(e) = print_stdout(table.table().title(title)) {
                eprintln!("⚠️ 无法渲染表格: {}", e);
            }
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
            Some(ts_str) if !ts_str.is_empty() => {
                // 解析为秒级时间戳并计算相对时间
                match ts_str.parse::<i64>() {
                    Ok(ts) => {
                        let now = chrono::Local::now().timestamp();
                        let diff = now - ts;
                        if diff < 0 {
                            // 未来时间，归为刚刚
                            return "刚刚".to_string();
                        }
                        const MINUTE: i64 = 60;
                        const HOUR: i64 = 60 * MINUTE;
                        const DAY: i64 = 24 * HOUR;

                        if diff < MINUTE {
                            "刚刚".to_string()
                        } else if diff < HOUR {
                            format!("{}分钟前", diff / MINUTE)
                        } else if diff < DAY {
                            format!("{}小时前", diff / HOUR)
                        } else if diff < 2 * DAY {
                            "昨天".to_string()
                        } else if diff < 3 * DAY {
                            "前天".to_string()
                        } else {
                            format!("{}天前", diff / DAY)
                        }
                    }
                    Err(_) => ts_str.clone(),
                }
            }
            _ => "从未".to_string(),
        }
    }

    pub fn set_last_connect_now(&mut self) {
        let now = chrono::Local::now().timestamp().to_string();
        self.last_connect = Some(now);
    }
}
