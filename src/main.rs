use anyhow::Result;
use app::App;
use clap::Parser;
use server::ServerCollection;
use std::fs::OpenOptions;
use tracing_appender::non_blocking;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

mod app;
mod auto_concurrency;
mod cli;
mod commands;
mod config;
mod error;
mod ops;
mod parse;
mod server;
mod transfer;
mod tui;
mod util;

pub use error::MkdirError;
pub use error::TransferError;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let mut config = config::Config::init(0);
    // Initialize tracing/logging if requested (used by `hp --debug`)
    // Use the config storage directory (where config.json lives) as the canonical
    // location for logs: <config_dir>/logs. This path is not configurable.
    init_tracing_if_requested(&config, cli.debug);

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
            json,
            quiet,
            retry,
            retry_backoff_ms,
            buf_mib,
        }) => {
            // 默认并发改为 auto（由 transfer 根据文件数/大小选择），上限提高到 32
            // concurrency 可以为 numeric 或 "auto"；当未提供或为 "auto" 时传 None
            let conc_opt: Option<usize> = match concurrency.as_deref() {
                None => None,
                Some("auto") => None,
                Some(s) => match s.parse::<usize>() {
                    Ok(0) => Some(1),
                    Ok(n) => Some(std::cmp::min(n, 32)),
                    Err(_) => None,
                },
            };
            let max_retries = retry.unwrap_or(3usize);
            if let Some(ms) = retry_backoff_ms {
                util::set_backoff_ms(ms);
            }
            let args = transfer::HandleTsArgs {
                sources,
                target,
                verbose,
                json,
                quiet,
                concurrency: conc_opt,
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

    res?;
    Ok(())
}

fn init_tracing_if_requested(cfg: &config::Config, debug: bool) {
    // Initialize tracing: attempt to write to the canonical debug log file
    // unconditionally (fallback to console-only if file cannot be created).
    // Determine the canonical config storage dir where config.json lives. Use
    // ops::ensure_hostpilot_dir to get/create ~/.hostpilot (or fallback to home if error).
    let logs_dir =
        match dirs::home_dir().and_then(|home_dir| match ops::ensure_hostpilot_dir(&home_dir) {
            Ok(p) => Some(p.join("logs")),
            Err(_) => None,
        }) {
            Some(d) => d,
            None => {
                // As a very conservative fallback, derive from the current config's
                // server_file_path parent (this should be inside the config dir).
                if let Some(parent) = cfg.server_file_path.parent() {
                    parent.join("logs")
                } else {
                    // Worst case: use home/.{pkgname}/logs
                    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                    home.join(".".to_string() + env!("CARGO_PKG_NAME")).join("logs")
                }
            }
        };

    // Ensure logs dir exists and register it early so that retry attempts can be
    // recorded during the run (even when not running with --verbose).
    let _ = std::fs::create_dir_all(&logs_dir);
    crate::util::init_retry_jsonl_dir(logs_dir.clone());

    // Initialize tracing so that all tracing output goes into the canonical
    // debug log file only. We intentionally do not attach a console fmt layer
    // so console output remains unaffected. If the file cannot be opened we
    // skip initializing tracing (no tracing output will be emitted).
    let log_path = logs_dir.join("debug.log");
    // Determine tracing level: default WARN; if CLI parsed --debug flag is set, use INFO
    let level_str = if debug { "debug" } else { "warn" };

    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => {
            let (non_blocking_writer, guard) = non_blocking(file);
            // Leak the worker guard so the background thread remains alive for
            // the duration of the process. If the guard is dropped when this
            // function returns, the writer thread will stop and logs may be
            // lost.
            let _ = Box::leak(Box::new(guard));
            // Use a debug-level EnvFilter by default; this ensures tracing logs
            // are recorded at debug and above in the file.
            let file_layer = fmt::layer()
                .with_writer(non_blocking_writer)
                .with_ansi(false)
                .with_filter(EnvFilter::new(level_str));
            tracing_subscriber::registry().with(file_layer).init();
        }
        Err(e) => {
            // Could not open log file; avoid writing tracing to console per user
            // request. Emit a single stderr message so user knows why tracing is
            // disabled for this run.
            eprintln!("warning: could not open debug log at {}: {}", log_path.display(), e);
        }
    }
}
