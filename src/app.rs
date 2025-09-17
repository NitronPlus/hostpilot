use std::io::Stdout;
use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::Config;
use crate::server::ServerCollection;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct App {
    config: Config,
    collection: ServerCollection,
}

impl App {
    pub fn init(config: Config) -> Self {
        let collection = match ServerCollection::read_from_storage(&config.server_file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("⚠️ 无法读取 server 集合: {}，使用空集合作为回退", e);
                ServerCollection::default()
            }
        };
        Self { config, collection }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> anyhow::Result<()> {
        crate::tui::run_app(self, terminal)
    }

    pub fn get_config(&self) -> &Config {
        &self.config
    }

    pub fn get_collection(&self) -> &ServerCollection {
        &self.collection
    }

    pub fn get_collection_mut(&mut self) -> &mut ServerCollection {
        &mut self.collection
    }

    pub fn save_collection(&self) -> anyhow::Result<()> {
        self.collection.save_to_storage(&self.config.server_file_path)?;
        Ok(())
    }
}

pub(crate) trait StorageObject {
    fn pretty_json(&self) -> String;
    fn save_to<P: AsRef<Path>>(&self, path: P)
    where
        Self: Serialize;
    fn read_from<T: Default + DeserializeOwned + Serialize, P: AsRef<Path>>(path: P) -> T;
}

impl<T: Serialize> StorageObject for T {
    fn pretty_json(&self) -> String {
        match serde_json::to_string_pretty(self) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️ 序列化失败: {}，使用空对象作为回退", e);
                "{}".to_string()
            }
        }
    }
    fn save_to<P: AsRef<Path>>(&self, path: P) {
        if let Err(e) = std::fs::write(path, self.pretty_json()) {
            eprintln!("⚠️ 写入文件失败: {}", e);
        }
    }
    fn read_from<R: Default + DeserializeOwned + Serialize, P: AsRef<Path>>(path: P) -> R {
        let v = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return R::default(),
        };
        match serde_json::from_str::<R>(&v) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("⚠️ 解析 JSON 失败: {}，返回默认值", e);
                R::default()
            }
        }
    }
}
