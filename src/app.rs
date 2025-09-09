use std::io::Stdout;
use std::path::Path;

use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::Config;
use crate::server::ServerCollection;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct App {
    config: Config,
    collection: ServerCollection,
}

impl App {
    pub fn init(config: Config) -> Self {
        let collection = ServerCollection::read_from(&config.server_file_path);
        Self {
            config,
            collection,
        }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> Result<(), Box<dyn std::error::Error>> {
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

    pub fn save_collection(&self) -> Result<(), Box<dyn std::error::Error>> {
        <ServerCollection as StorageObject>::save_to(&self.collection, &self.config.server_file_path);
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
        serde_json::to_string_pretty(self).unwrap()
    }
    fn save_to<P: AsRef<Path>>(&self, path: P) {
        std::fs::write(path, self.pretty_json()).unwrap();
    }
    fn read_from<R: Default + DeserializeOwned + Serialize, P: AsRef<Path>>(path: P) -> R {
        let v = std::fs::read_to_string(path).unwrap_or_else(|_| R::default().pretty_json());
        serde_json::from_str::<R>(&v).unwrap()
    }
}
