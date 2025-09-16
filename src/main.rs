use anyhow::Result;
use app::App;
use clap::Parser;
use server::ServerCollection;
use std::fs::OpenOptions;
use tracing_subscriber::prelude::*;

mod app;
mod cli;
mod commands;
mod config;
mod ops;
mod parse;
mod server;
mod transfer;
mod tui;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let mut config = config::Config::init();
    // 如果以 `hp ts --verbose` 调用，则启用文件 tracing 日志到 $HOME/.hostpilot/logs/debug.log — If invoked as `hp ts --verbose` enable tracing file logger to $HOME/.hostpilot/logs/debug.log
    let is_ts = std::env::args().nth(1).map(|s| s == "ts").unwrap_or(false);
    let has_verbose = std::env::args().any(|a| a == "--verbose");
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
            let (non_blocking, _guard) = tracing_appender::non_blocking(file);
            let fmt_layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false);
            let filter_layer = tracing_subscriber::EnvFilter::new("debug");
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt_layer)
                .init();
        }
    }

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
        }) => {
            // 强制默认并发为 6 且最大为 8 — Enforce default 6 and max 8
            let conc = concurrency.unwrap_or(6);
            let conc = if conc == 0 { 1 } else { conc };
            let conc = std::cmp::min(conc, 8);
            transfer::handle_ts(
                &config,
                false,
                sources,
                target,
                verbose,
                conc,
                output_failures,
            )
        }
        Some(cli::Commands::Set {
            pub_key_path,
            server_path,
            client_path,
            scp_path,
        }) => commands::handle_set(&config, pub_key_path, server_path, client_path, scp_path),
        // 所有子命令已在上方处理；未指定子命令则进入 None 分支以运行 TUI/默认行为 — All subcommands handled above; fall through to None branch for TUI/default behavior
        None => {
            if cli.alias != "-" {
                // 连接到提供的别名 — Connect to the provided alias
                let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
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
                        collection.save_to_storage(&config.server_file_path);
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
