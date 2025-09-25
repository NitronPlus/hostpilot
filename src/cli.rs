use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
#[clap(subcommand_negates_reqs(true))]
pub struct Cli {
    #[clap(default_value = "-", hide_default_value(true), hide(true))]
    pub alias: String,
    #[clap(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[clap(about = "Create alias for a remote SSH server", name = "new", display_order = 3)]
    Create { alias: String, remote_host: String },
    #[clap(about = "Remove the specify alias", name = "rm", display_order = 4)]
    Remove { alias: String },
    #[clap(about = "Rename the specify alias", name = "mv", display_order = 5)]
    Rename { alias: String, new_alias: String },
    #[clap(about = "List all server alias", name = "ls", display_order = 2)]
    List {},
    #[clap(about = "Copy RSA public key to remote server", name = "ln")]
    Link { alias: String },

    #[clap(about = "Transfer files using builtin ssh2 SFTP (no password support)", name = "ts")]
    Ts {
        #[clap(num_args = 1.., required = true, help = "Source paths (local or remote alias:/path)")]
        sources: Vec<String>,
        #[clap(required = true, help = "Target path (local or remote alias:/path)")]
        target: String,
        #[clap(
            short = 'c',
            long = "concurrency",
            help = "Number of concurrent workers or 'auto' (default auto, max 32)"
        )]
        concurrency: Option<String>,
        #[clap(
            short = 'r',
            long = "retry",
            help = "Number of retries per file on transient errors (default 3)"
        )]
        retry: Option<usize>,
        #[clap(
            short = 'b',
            long = "retry-backoff-ms",
            help = "Base backoff in milliseconds used between retry attempts (default 100)"
        )]
        retry_backoff_ms: Option<u64>,
        #[clap(short, long, help = "Print verbose diagnostic logs for debugging")]
        verbose: bool,
        #[clap(long = "json", help = "Emit a single-line JSON summary at end (machine-readable)")]
        json: bool,
        #[clap(
            short = 'o',
            long = "output-failures",
            help = "Write failures to this file (append)"
        )]
        output_failures: Option<PathBuf>,
        #[clap(
            short = 'f',
            long = "buf-mib",
            help = "Per-worker IO buffer size in MiB (default 1, max 8)",
            value_parser
        )]
        buf_mib: Option<usize>,
    },
    #[clap(about = "Configure HostPilot")]
    Set {
        #[clap(short = 'k', help = "Set the SSH public key path", display_order = 1)]
        pub_key_path: Option<PathBuf>,
        #[clap(short, help = "Set the hostpilot server file path", display_order = 2)]
        server_path: Option<PathBuf>,
        #[clap(short, help = "Set the ssh client path", display_order = 3)]
        client_path: Option<PathBuf>,
        #[clap(short = 'a', help = "Set the scp path", display_order = 4)]
        scp_path: Option<PathBuf>,
    },
}
