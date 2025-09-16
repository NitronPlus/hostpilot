use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;

use crate::server;

pub type Tui = Terminal<CrosstermBackend<std::io::Stdout>>;

// Ensure HostPilot config directory exists and migrate legacy `.psm` if present.
// If `~/.psm` exists and `~/.hostpilot` does not, attempt to rename. If rename fails,
// fall back to recursive copy then remove the old directory. Returns the `~/.hostpilot` path.
pub fn ensure_hostpilot_dir(home_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use std::fs;
    use std::path::Path;

    let hostpilot_dir = home_dir.join(".hostpilot");
    let psm_dir = home_dir.join(".psm");

    if psm_dir.exists() && !hostpilot_dir.exists() {
        println!(
            "🔁 Migrating legacy config directory: {} -> {}",
            psm_dir.display(),
            hostpilot_dir.display()
        );

        // Prefer atomic rename; fall back to recursive copy if rename fails (cross-device, etc.)
        match fs::rename(&psm_dir, &hostpilot_dir) {
            Ok(_) => {
                println!(
                    "   ✅ Renamed legacy directory to {}",
                    hostpilot_dir.display()
                );
            }
            Err(e) => {
                println!(
                    "   ⚠️  Rename failed ({}), falling back to recursive copy...",
                    e
                );

                // recursive copy helper
                fn copy_recursively(src: &Path, dst: &Path) -> std::io::Result<()> {
                    if !dst.exists() {
                        fs::create_dir_all(dst)?;
                    }
                    for entry in fs::read_dir(src)? {
                        let entry = entry?;
                        let file_type = entry.file_type()?;
                        let from = entry.path();
                        let to = dst.join(entry.file_name());
                        if file_type.is_dir() {
                            copy_recursively(&from, &to)?;
                        } else if file_type.is_file() {
                            fs::copy(&from, &to)?;
                        } else {
                            // ignore symlinks or special files
                        }
                    }
                    Ok(())
                }

                copy_recursively(&psm_dir, &hostpilot_dir)?;
                // remove old dir after successful copy
                fs::remove_dir_all(&psm_dir)?;
                println!(
                    "   ✅ Copied legacy directory to {}",
                    hostpilot_dir.display()
                );
            }
        }
    }

    // ensure hostpilot dir exists
    if !hostpilot_dir.exists() {
        fs::create_dir_all(&hostpilot_dir)?;
    }

    Ok(hostpilot_dir)
}

pub fn check_and_upgrade_if_needed(
    config: &crate::config::Config,
) -> Result<crate::config::Config> {
    use std::fs;

    // 获取配置文件路径 — Get the config file path
    let home_dir = match dirs::home_dir() {
        Some(p) => p,
        None => {
            eprintln!("❌ 无法找到用户主目录");
            std::process::exit(1);
        }
    };
    // Ensure we use ~/.hostpilot and migrate legacy ~/.psm if needed
    let config_dir = ensure_hostpilot_dir(&home_dir)?;
    let config_path = config_dir.join("config.json");

    // 读取现有配置内容 — Read existing config
    let config_content = fs::read_to_string(&config_path)?;

    // 解析为 serde_json::Value 以检查版本 — Parse as serde_json::Value to check version
    let config_json: serde_json::Value = serde_json::from_str(&config_content)?;

    // 如果已存在 server.db（在旧 server_file_path 附近或默认 hostpilot 目录），立即执行升级逻辑以更新 config.json 指向 DB — If a server.db already exists (either next to old server_file_path or in the default hostpilot dir), perform the upgrade logic immediately so config.json is updated to point to the DB.
    {
        use std::path::Path;
        let database_dir = ensure_hostpilot_dir(&home_dir)?; // returns hostpilot dir
        let default_db_path = database_dir.join("server.db");
        let db_path = config_json
            .get("server_file_path")
            .and_then(|v| v.as_str())
            .and_then(|s| Path::new(s).parent().map(|dir| dir.join("server.db")))
            .unwrap_or(default_db_path.clone());

        if db_path.exists() && config.version.is_none() {
            println!(
                "   ⚠️  Detected existing server.db at {}, running upgrade to update config.json...",
                db_path.display()
            );
            upgrade_config_and_data(&crate::config::Config::init())?;
            // 从磁盘重新加载已更新的配置以拾取新的 server_file_path — Reload updated config from disk to pick up new server_file_path
            return Ok(crate::config::Config::init());
        }
    }

    // 检查是否需要升级（在 v2 中版本为数字） — Check conditions for upgrade (version is numeric in v2)
    let needs_upgrade = match config_json.get("version") {
        Some(v) => v.as_u64().map(|n| n as u32).unwrap_or(0) < server::get_protocol_version(),
        None => true,
    };

    if needs_upgrade {
        println!("🔄 Detected outdated configuration, running automatic upgrade...");
        upgrade_config_and_data(&crate::config::Config::init())?;
        println!("✅ Automatic upgrade completed. Continuing with application startup...");
        // Reload updated config from disk to pick up new server_file_path
        return Ok(crate::config::Config::init());
    }

    Ok(config.clone())
}

pub fn backup_existing_files_with_paths(
    server_json_path: Option<&std::path::Path>,
    server_db_path: Option<&std::path::Path>,
) -> Result<()> {
    use chrono::Utc;
    use std::fs;

    // 获取 HostPilot 配置目录 — Get the HostPilot config directory
    let home_dir = match dirs::home_dir() {
        Some(p) => p,
        None => {
            eprintln!("❌ 无法找到用户主目录");
            std::process::exit(1);
        }
    };
    let app_dir = ensure_hostpilot_dir(&home_dir)?;

    // 如果不存在则创建备份目录 — Create backup directory if it doesn't exist
    let backup_dir = app_dir.join("backups");
    if !backup_dir.exists() {
        fs::create_dir_all(&backup_dir)?;
    }

    // 为备份文件生成时间戳 — Generate timestamp for backup files
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");

    // 备份 config.json — Backup config.json
    let config_path = app_dir.join("config.json");
    if config_path.exists() {
        let backup_config_path = backup_dir.join(format!("config_{}.json", timestamp));
        fs::copy(&config_path, &backup_config_path)?;
        println!(
            "   📋 Backed up config.json to: {}",
            backup_config_path.display()
        );
    }

    // 备份 server.json（如提供并存在） — Backup server.json (if provided and exists)
    if let Some(sjp) = server_json_path
        && sjp.exists()
    {
        let backup_server_path = backup_dir.join(format!("server_{}.json", timestamp));
        fs::copy(sjp, &backup_server_path)?;
        println!(
            "   🖥️  Backed up server.json to: {}",
            backup_server_path.display()
        );
    }

    // 备份 server.db（如提供并存在） — Backup server.db (if provided and exists)
    if let Some(sdbp) = server_db_path
        && sdbp.exists()
    {
        let backup_db_path = backup_dir.join(format!("server_{}.db", timestamp));
        fs::copy(sdbp, &backup_db_path)?;
        println!(
            "   🗄️  Backed up server.db to: {}",
            backup_db_path.display()
        );
    }

    Ok(())
}

pub fn upgrade_config_and_data(_config: &crate::config::Config) -> Result<()> {
    use std::fs;

    // Get the config file path
    let home_dir = match dirs::home_dir() {
        Some(p) => p,
        None => {
            eprintln!("❌ 无法找到用户主目录");
            std::process::exit(1);
        }
    };
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
        println!(
            "✅ HostPilot is already at the latest version (v{})",
            current_version
        );
        return Ok(());
    }

    println!("🔄 Starting HostPilot upgrade process...");

    // Determine old server.json path from old config's server_file_path
    let old_server_json_path_opt: Option<std::path::PathBuf> = config_json
        .get("server_file_path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);

    // Compute target DB path in the same directory as server_file_path (if present)
    let home_dir = match dirs::home_dir() {
        Some(p) => p,
        None => {
            eprintln!("❌ 无法找到用户主目录");
            std::process::exit(1);
        }
    };
    let app_dir = home_dir.join(".".to_owned() + env!("CARGO_PKG_NAME"));
    let default_db_path = app_dir.join("server.db");
    let db_path = old_server_json_path_opt
        .as_ref()
        .and_then(|p| p.parent().map(|dir| dir.join("server.db")))
        .unwrap_or(default_db_path.clone());

    // 第 0 步：在升级前备份现有文件 — Step 0: Backup existing files before upgrade
    println!("💾 Creating backup of existing configuration files...");
    backup_existing_files_with_paths(old_server_json_path_opt.as_deref(), Some(&db_path))?;

    // 第 1 步：读取 server.json 并迁移数据 — Step 1: Read server.json and migrate data
    println!("📖 Reading and migrating server.json data...");
    let collection = if let Some(server_json_path) = old_server_json_path_opt.as_ref() {
        if server_json_path.exists() {
            let server_content = fs::read_to_string(server_json_path)?;
            let server_json: serde_json::Value = serde_json::from_str(&server_content)?;

            // 从 JSON 中解析服务器信息 — Parse servers from JSON
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
            println!(
                "   📦 Migrated {} servers from server.json",
                collection.hosts().len()
            );
            collection
        } else {
            println!("   ⚠️  server.json not found at old path, creating empty database");
            server::ServerCollection::default()
        }
    } else {
        println!("   ⚠️  Old config has no server_file_path, creating empty database");
        server::ServerCollection::default()
    };

    // 第 2 步：创建 SQLite 数据库并保存迁移的数据 — Step 2: Create SQLite DB and save migrated data
    println!("🗄️  Creating SQLite database and saving data...");

    // 创建数据库并保存数据 — Create database and save data
    create_sqlite_database(&db_path)?;
    collection.save_to_storage(&db_path);

    // 第 3 步：更新 config.json — Step 3: Update config.json
    println!("📝 Updating config.json...");
    let mut config_struct: crate::config::Config = serde_json::from_str(&config_content)?;
    config_struct.server_file_path = db_path;
    config_struct.version = Some(current_version);

    // 将更新后的配置写回文件 — Write updated config back to file
    let updated_config_content = serde_json::to_string_pretty(&config_struct)?;
    fs::write(&config_path, updated_config_content)?;

    println!("✅ Upgrade completed successfully!");
    println!("📋 Current protocol version: {}", current_version);
    println!(
        "💾 Migrated {} servers to SQLite database",
        collection.hosts().len()
    );
    println!("📋 server.json preserved as backup");

    Ok(())
}

pub fn create_sqlite_database(db_path: &std::path::Path) -> Result<()> {
    use rusqlite::Connection;

    // 创建 SQLite 连接 — Create SQLite connection
    let conn = Connection::open(db_path)?;

    // 使用新 schema 创建 servers 表（id + alias 唯一） — Create servers table with new schema (id + alias unique)
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
    println!(
        "   📋  Table structure: id (PK AUTOINCREMENT), alias (UNIQUE), username, address, port, last_connect"
    );

    Ok(())
}

pub fn setup_terminal() -> Result<Tui> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    terminal.show_cursor()?;
    Ok(())
}
