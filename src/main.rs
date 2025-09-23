use anyhow::Result;
use app::App;
use clap::Parser;
use server::ServerCollection;
use std::fs::OpenOptions;
use tracing_appender::non_blocking;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

mod app;
mod cli;
mod commands;
mod config;
mod ops;
mod parse;
mod server;
mod transfer;
mod tui;
mod util;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let mut config = config::Config::init(0);
    // Initialize tracing/logging if requested (used by `hp ts --verbose`)
    init_tracing_if_requested();

    // 在处理命令前检查是否需要升级；如已升级则重新加载配置 — Check if upgrade is needed before processing commands; reload config if upgraded
    config = ops::check_and_upgrade_if_needed(&config)?;

    let res = match cli.command {
        Some(cli::Commands::Create { alias, remote_host }) => {
            commands::handle_create(&config, alias, remote_host)
        }
        Some(cli::Commands::Rename { alias, new_alias }) => {
            commands::handle_rename(&config, alias, new_alias)
        }
        Some(cli::Commands::List {}) => commands::handle_list(&config),
        Some(cli::Commands::Remove { alias }) => commands::handle_remove(&config, alias),
        Some(cli::Commands::Link { alias }) => commands::handle_link(&config, alias),
        Some(cli::Commands::Ts {
            sources,
            target,
            concurrency,
            verbose,
            output_failures,
            retry,
            retry_backoff_ms,
            buf_mib,
        }) => {
            // 默认并发 8，上限 16
            let conc = concurrency.unwrap_or(8);
            let conc = if conc == 0 { 1 } else { conc };
            let conc = std::cmp::min(conc, 16);
            let max_retries = retry.unwrap_or(3usize);
            if let Some(ms) = retry_backoff_ms {
                util::set_backoff_ms(ms);
            }
            let args = transfer::HandleTsArgs {
                sources,
                target,
                verbose,
                concurrency: conc,
                output_failures,
                max_retries,
                buf_size: buf_mib.map(|m| m.clamp(1, 8) * 1024 * 1024).unwrap_or(1024 * 1024),
            };
            transfer::handle_ts(&config, args)
        }
        Some(cli::Commands::Set { pub_key_path, server_path, client_path, scp_path }) => {
            commands::handle_set(&config, pub_key_path, server_path, client_path, scp_path)
        }
        // 所有子命令已在上方处理；未指定子命令则进入 None 分支以运行 TUI/默认行为 — All subcommands handled above; fall through to None branch for TUI/default behavior
        None => {
            if cli.alias != "-" {
                // 连接到提供的别名 — Connect to the provided alias
                let mut collection = ServerCollection::read_from_storage(&config.server_file_path)?;
                if let Some(server) = collection.get(&cli.alias) {
                    let host = format!("{}@{}", server.username, server.address);
                    let port = format!("-p{}", server.port);
                    let args = vec![host, port];
                    let status = std::process::Command::new(&config.ssh_client_app_path)
                        .args(args)
                        .status()?;

                    // 在连接成功后更新 last_connect 时间戳 — Update last_connect timestamp after successful connection
                    if status.success() {
                        let mut updated_server = server.clone();
                        updated_server.set_last_connect_now();
                        collection.insert(cli.alias.as_str(), updated_server);
                        collection.save_to_storage(&config.server_file_path)?;
                    }
                } else {
                    eprintln!("❌ 别名 '{}' 未找到", cli.alias);
                }
                Ok(())
            } else {
                // 未指定命令，运行 TUI — No command, run TUI
                let mut app = App::init(config);
                let mut terminal = ops::setup_terminal()?;
                let result = app.run(&mut terminal);
                ops::restore_terminal(&mut terminal)?;
                result
            }
        }
    };

    if let Err(e) = res {
        eprintln!("错误: {}", e);
        // 将错误向上传播以便在需要时 main 返回非零退出码 — Propagate error so main returns non-zero if caller expects it
        return Err(e);
    }
    Ok(())
}

fn init_tracing_if_requested() {
    // Determine if the user passed --verbose and the subcommand is `ts`
    let is_ts = std::env::args().nth(1).map(|s| s == "ts").unwrap_or(false);
    let has_verbose = std::env::args().any(|a| a == "--verbose");
    // Initialize a default subscriber respecting RUST_LOG so logs can be enabled via env
    if std::env::var_os("RUST_LOG").is_some() {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        tracing_subscriber::registry().with(filter).with(fmt::layer()).init();
    }

    // If user explicitly asked for ts --verbose, also add a file appender at debug level
    if is_ts
        && has_verbose
        && let Some(home_dir) = dirs::home_dir()
    {
        // Use ~/.hostpilot for logs; migrate legacy ~/.psm if needed
        let base = match ops::ensure_hostpilot_dir(&home_dir) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("⚠️ 无法准备日志目录: {}", e);
                home_dir.join(".".to_string() + env!("CARGO_PKG_NAME"))
            }
        };
        let logs_dir = base.join("logs");
        let _ = std::fs::create_dir_all(&logs_dir);
        let log_path = logs_dir.join("debug.log");
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            let (non_blocking_writer, _guard) = non_blocking(file);
            let fmt_layer = fmt::layer().with_writer(non_blocking_writer).with_ansi(false);
            let filter_layer = EnvFilter::new("debug");
            // Merge with existing registry
            tracing_subscriber::registry().with(filter_layer).with(fmt_layer).init();
        }
    }
}
