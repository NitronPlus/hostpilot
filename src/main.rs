use app::App;
use std::io;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::cursor::{Hide, Show};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use clap::Parser;
use server::ServerCollection;

mod app;
mod cli;
mod config;
mod server;
mod tui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cli::Cli::parse();
    let mut config = config::Config::init();

    // Check if upgrade is needed before processing commands; reload config if upgraded
    config = check_and_upgrade_if_needed(&config)?;

    match cli.command {
        Some(cli::Commands::Go { alias }) => {
            let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
            if let Some(server) = collection.get(&alias) {
                let host = format!("{}@{}", server.username, server.address);
                let port = format!("-p{}", server.port);
                let args = vec![host, port];
                let status = std::process::Command::new(&config.ssh_client_app_path)
                    .args(args)
                    .status()?;

                // Update last_connect timestamp after successful connection
                if status.success() {
                    let mut updated_server = server.clone();
                    updated_server.set_last_connect_now();
                    collection.insert(&alias, updated_server);
                    collection.save_to_storage(&config.server_file_path);
                }
            } else {
                eprintln!("Alias '{}' not found", alias);
            }
            Ok(())
        }
        Some(cli::Commands::List {}) => {
            let collection = ServerCollection::read_from_storage(&config.server_file_path);
            collection.show_table();
            Ok(())
        }
        Some(cli::Commands::Upgrade {}) => {
            upgrade_config_and_data(&config)?;
            Ok(())
        }
        Some(other) => {
            eprintln!("Command {:?} not implemented in TUI mode", other);
            Ok(())
        }
        None => {
            if cli.alias != "-" {
                // Connect to the provided alias
                let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
                if let Some(server) = collection.get(&cli.alias) {
                    let host = format!("{}@{}", server.username, server.address);
                    let port = format!("-p{}", server.port);
                    let args = vec![host, port];
                    let status = std::process::Command::new(&config.ssh_client_app_path)
                        .args(args)
                        .status()?;

                    // Update last_connect timestamp after successful connection
                    if status.success() {
                        let mut updated_server = server.clone();
                        updated_server.set_last_connect_now();
                        collection.insert(&cli.alias, updated_server);
                        collection.save_to_storage(&config.server_file_path);
                    }
                } else {
                    eprintln!("Alias '{}' not found", cli.alias);
                }
                Ok(())
            } else {
                // No command, run TUI
                let mut app = App::init(config);
                let mut terminal = setup_terminal()?;
                let result = app.run(&mut terminal);
                restore_terminal(&mut terminal)?;
                result
            }
        }
    }
}

fn check_and_upgrade_if_needed(config: &config::Config) -> Result<config::Config, Box<dyn std::error::Error>> {
    use std::fs;

    // Get the config file path
    let home_dir = dirs::home_dir().unwrap();
    let config_path = home_dir
        .join(".".to_owned() + env!("CARGO_PKG_NAME"))
        .join("config.json");

    // Read existing config
    let config_content = fs::read_to_string(&config_path)?;

    // Parse as serde_json::Value to check version
    let config_json: serde_json::Value = serde_json::from_str(&config_content)?;

    // Check conditions for upgrade (version is numeric in v2)
    let needs_upgrade = match config_json.get("version") {
        Some(v) => v.as_u64().map(|n| n as u32).unwrap_or(0) < server::get_protocol_version(),
        None => true,
    };

    if needs_upgrade {
        println!("🔄 Detected outdated configuration, running automatic upgrade...");
        upgrade_config_and_data(config)?;
        println!("✅ Automatic upgrade completed. Continuing with application startup...");
        // Reload updated config from disk to pick up new server_file_path
        return Ok(config::Config::init());
    }

    Ok(config.clone())
}

fn backup_existing_files_with_paths(server_json_path: Option<&std::path::Path>, server_db_path: Option<&std::path::Path>) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use chrono::Utc;

    // Get the PSM config directory
    let home_dir = dirs::home_dir().unwrap();
    let psm_dir = home_dir.join(".".to_owned() + env!("CARGO_PKG_NAME"));

    // Create backup directory if it doesn't exist
    let backup_dir = psm_dir.join("backups");
    if !backup_dir.exists() {
        fs::create_dir_all(&backup_dir)?;
    }

    // Generate timestamp for backup files
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");

    // Backup config.json
    let config_path = psm_dir.join("config.json");
    if config_path.exists() {
        let backup_config_path = backup_dir.join(format!("config_{}.json", timestamp));
        fs::copy(&config_path, &backup_config_path)?;
        println!("   📋 Backed up config.json to: {}", backup_config_path.display());
    }

    // Backup server.json (if provided and exists)
    if let Some(sjp) = server_json_path
        && sjp.exists() {
        let backup_server_path = backup_dir.join(format!("server_{}.json", timestamp));
        fs::copy(sjp, &backup_server_path)?;
        println!("   🖥️  Backed up server.json to: {}", backup_server_path.display());
    }

    // Backup server.db (if provided and exists)
    if let Some(sdbp) = server_db_path
        && sdbp.exists() {
        let backup_db_path = backup_dir.join(format!("server_{}.db", timestamp));
        fs::copy(sdbp, &backup_db_path)?;
        println!("   🗄️  Backed up server.db to: {}", backup_db_path.display());
    }

    Ok(())
}

fn upgrade_config_and_data(_config: &config::Config) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    // Get the config file path
    let home_dir = dirs::home_dir().unwrap();
    let config_path = home_dir
        .join(".".to_owned() + env!("CARGO_PKG_NAME"))
        .join("config.json");

    // Read existing config
    let config_content = fs::read_to_string(&config_path)?;

    // Parse as serde_json::Value to check version
    let config_json: serde_json::Value = serde_json::from_str(&config_content)?;

    // Check if already at current version (numeric)
    let current_version = server::get_protocol_version();
    if config_json
        .get("version")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0)
        >= current_version
    {
        println!("✅ PSM is already at the latest version (v{})", current_version);
        return Ok(());
    }

    println!("🔄 Starting PSM upgrade process...");

    // Determine old server.json path from old config's server_file_path
    let old_server_json_path_opt: Option<std::path::PathBuf> = config_json
        .get("server_file_path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);

    // Compute target DB path in the same directory as server_file_path (if present)
    let home_dir = dirs::home_dir().unwrap();
    let psm_dir = home_dir.join(".".to_owned() + env!("CARGO_PKG_NAME"));
    let default_db_path = psm_dir.join("server.db");
    let db_path = old_server_json_path_opt
        .as_ref()
        .and_then(|p| p.parent().map(|dir| dir.join("server.db")))
        .unwrap_or(default_db_path.clone());

    // Step 0: Backup existing files before upgrade
    println!("💾 Creating backup of existing configuration files...");
    backup_existing_files_with_paths(old_server_json_path_opt.as_deref(), Some(&db_path))?;

    // Step 1: Read server.json and migrate data
    println!("📖 Reading and migrating server.json data...");
    let collection = if let Some(server_json_path) = old_server_json_path_opt.as_ref() {
        if server_json_path.exists() {
            let server_content = fs::read_to_string(server_json_path)?;
            let server_json: serde_json::Value = serde_json::from_str(&server_content)?;

            // Parse servers from JSON
            let mut collection = server::ServerCollection::default();
            if let Some(hosts) = server_json.get("hosts").and_then(|h| h.as_object()) {
                for (alias, server_data) in hosts {
                    if let (Some(username), Some(address), Some(port)) = (
                        server_data.get("username").and_then(|u| u.as_str()),
                        server_data.get("address").and_then(|a| a.as_str()),
                        server_data.get("port").and_then(|p| p.as_u64()),
                    ) {
                        let server = server::Server {
                            id: None,
                            alias: Some(alias.clone()),
                            username: username.to_string(),
                            address: address.to_string(),
                            port: port as u16,
                            last_connect: None, // Initialize as None for migrated servers
                        };
                        collection.insert(alias, server);
                    }
                }
            }
            println!("   📦 Migrated {} servers from server.json", collection.hosts().len());
            collection
        } else {
            println!("   ⚠️  server.json not found at old path, creating empty database");
            server::ServerCollection::default()
        }
    } else {
        println!("   ⚠️  Old config has no server_file_path, creating empty database");
        server::ServerCollection::default()
    };

    // Step 2: Create SQLite DB and save migrated data
    println!("🗄️  Creating SQLite database and saving data...");

    // Create database and save data
    create_sqlite_database(&db_path)?;
    collection.save_to_storage(&db_path);

    // Step 3: Update config.json
    println!("📝 Updating config.json...");
    let mut config_struct: config::Config = serde_json::from_str(&config_content)?;
    config_struct.server_file_path = db_path;
    config_struct.version = Some(current_version);

    // Write updated config back to file
    let updated_config_content = serde_json::to_string_pretty(&config_struct)?;
    fs::write(&config_path, updated_config_content)?;

    println!("✅ Upgrade completed successfully!");
    println!("📋 Current protocol version: {}", current_version);
    println!("💾 Migrated {} servers to SQLite database", collection.hosts().len());
    println!("📋 server.json preserved as backup");

    Ok(())
}

fn create_sqlite_database(db_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use rusqlite::Connection;

    // Create SQLite connection
    let conn = Connection::open(db_path)?;

    // Create servers table with new schema (id + alias unique)
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
    )?;

    println!("   🗄️  SQLite database ensured with servers table");
    println!("   📋  Table structure: id (PK AUTOINCREMENT), alias (UNIQUE), username, address, port, last_connect");

    Ok(())
}

fn setup_terminal() -> Result<tui::Tui, Box<dyn std::error::Error>> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut tui::Tui) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        Show
    )?;
    terminal.show_cursor()?;
    Ok(())
}
