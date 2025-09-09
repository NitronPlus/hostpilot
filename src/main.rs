use app::App;
use std::io;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::cursor::{Hide, Show};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use clap::Parser;

mod app;
mod cli;
mod config;
mod server;
mod tui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cli::Cli::parse();
    let config = config::Config::init();
    
    // Check if upgrade is needed before processing commands
    check_and_upgrade_if_needed(&config)?;
    
    match cli.command {
        Some(cli::Commands::Go { alias }) => {
            let collection = <server::ServerCollection as app::StorageObject>::read_from::<server::ServerCollection, _>(&config.server_file_path);
            if let Some(server) = collection.get(&alias) {
                let host = format!("{}@{}", server.username, server.address);
                let port = format!("-p{}", server.port);
                let args = vec![host, port];
                std::process::Command::new(&config.ssh_client_app_path)
                    .args(args)
                    .status()?;
            } else {
                eprintln!("Alias '{}' not found", alias);
            }
            Ok(())
        }
        Some(cli::Commands::List {}) => {
            let collection = <server::ServerCollection as app::StorageObject>::read_from::<server::ServerCollection, _>(&config.server_file_path);
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
                let collection = <server::ServerCollection as app::StorageObject>::read_from::<server::ServerCollection, _>(&config.server_file_path);
                if let Some(server) = collection.get(&cli.alias) {
                    let host = format!("{}@{}", server.username, server.address);
                    let port = format!("-p{}", server.port);
                    let args = vec![host, port];
                    std::process::Command::new(&config.ssh_client_app_path)
                        .args(args)
                        .status()?;
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

fn check_and_upgrade_if_needed(config: &config::Config) -> Result<(), Box<dyn std::error::Error>> {
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
    
    // Check conditions for upgrade:
    // 1. config doesn't have version field
    // 2. version is less than current PROTOCOL_VERSION
    let needs_upgrade = if let Some(version_value) = config_json.get("version") {
        if let Some(version_str) = version_value.as_str() {
            // Parse version and compare with current protocol version
            match version_str.parse::<u32>() {
                Ok(version_num) => version_num < server::get_protocol_version(),
                Err(_) => true, // If version is not a valid number, upgrade
            }
        } else {
            true // If version is not a string, upgrade
        }
    } else {
        true // If no version field, upgrade
    };
    
    if needs_upgrade {
        println!("🔄 Detected outdated configuration, running automatic upgrade...");
        upgrade_config_and_data(config)?;
        println!("✅ Automatic upgrade completed. Continuing with application startup...");
    }
    
    Ok(())
}

fn backup_existing_files(config: &config::Config) -> Result<(), Box<dyn std::error::Error>> {
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
    
    // Backup server.json
    if config.server_file_path.exists() {
        let backup_server_path = backup_dir.join(format!("server_{}.json", timestamp));
        fs::copy(&config.server_file_path, &backup_server_path)?;
        println!("   🖥️  Backed up server.json to: {}", backup_server_path.display());
    }
    
    Ok(())
}

fn upgrade_config_and_data(config: &config::Config) -> Result<(), Box<dyn std::error::Error>> {
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
    
    // Check if already at current version
    let current_version = server::get_protocol_version();
    if let Some(version_value) = config_json.get("version")
        && let Some(version_str) = version_value.as_str()
        && let Ok(version_num) = version_str.parse::<u32>()
        && version_num >= current_version {
        println!("✅ PSM is already at the latest version (v{})", current_version);
        return Ok(());
    }
    
    println!("🔄 Starting PSM upgrade process...");
    
    // Step 0: Backup existing files before upgrade
    println!("💾 Creating backup of existing configuration files...");
    backup_existing_files(config)?;
    
    // Step 1: Upgrade config.json to add version field
    println!("📝 Upgrading config.json...");
    upgrade_config_file(config)?;
    
    // Step 2: Upgrade server.json to add last_connect field to servers that don't have it
    println!("🖥️  Upgrading server.json...");
    upgrade_server_file(&config.server_file_path)?;
    
    println!("✅ Upgrade completed successfully!");
    println!("📋 Current protocol version: {}", server::get_protocol_version());
    
    Ok(())
}

fn upgrade_config_file(_config: &config::Config) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    
    // Get the config file path
    let home_dir = dirs::home_dir().unwrap();
    let config_path = home_dir
        .join(".".to_owned() + env!("CARGO_PKG_NAME"))
        .join("config.json");
    
    // Read existing config
    let config_content = fs::read_to_string(&config_path)?;
    
    // Parse as serde_json::Value to manipulate
    let mut config_json: serde_json::Value = serde_json::from_str(&config_content)?;
    
    // Check if version field exists
    if config_json.get("version").is_none() {
        // Add version field
        config_json["version"] = serde_json::Value::String("1".to_string());
        println!("   ➕ Added version field to config.json");
        
        // Write back to file
        let updated_content = serde_json::to_string_pretty(&config_json)?;
        fs::write(&config_path, updated_content)?;
        println!("   💾 Config file updated");
    } else {
        println!("   ✅ Version field already exists in config.json");
    }
    
    Ok(())
}

fn upgrade_server_file(server_file_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    
    // Read existing server file
    let server_content = fs::read_to_string(server_file_path)?;
    
    // Parse as serde_json::Value to manipulate
    let mut server_json: serde_json::Value = serde_json::from_str(&server_content)?;
    
    // Check if hosts object exists
    if let Some(hosts) = server_json.get_mut("hosts") {
        if let Some(hosts_obj) = hosts.as_object_mut() {
            let mut upgraded_count = 0;
            
            for (alias, server_value) in hosts_obj.iter_mut() {
                if let Some(server_obj) = server_value.as_object_mut() {
                    // Check if last_connect field exists
                    if server_obj.get("last_connect").is_none() {
                        // Add last_connect field with empty string
                        server_obj.insert("last_connect".to_string(), serde_json::Value::String("".to_string()));
                        upgraded_count += 1;
                        println!("   ➕ Added last_connect field to server '{}'", alias);
                    }
                }
            }
            
            if upgraded_count > 0 {
                // Write back to file
                let updated_content = serde_json::to_string_pretty(&server_json)?;
                fs::write(server_file_path, updated_content)?;
                println!("   💾 Server file updated ({} servers upgraded)", upgraded_count);
            } else {
                println!("   ✅ All servers already have last_connect field");
            }
        }
    } else {
        println!("   ⚠️  No hosts found in server.json");
    }
    
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
