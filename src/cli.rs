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
    #[clap(
        about = "Create alias for a remote SSH server",
        name = "new",
        display_order = 3
    )]
    Create { alias: String, remote_host: String },
    #[clap(about = "Remove the specify alias", name = "rm", display_order = 4)]
    Remove { alias: String },
    #[clap(about = "Rename the specify alias", name = "mv", display_order = 5)]
    Rename { alias: String, new_alias: String },
    #[clap(about = "Connect to the specify server alias", display_order = 1)]
    Go { alias: String },
    #[clap(about = "List all server alias", name = "ls", display_order = 2)]
    List {},
    #[clap(about = "Copy RSA public key to remote server", name = "ln")]
    Link { alias: String },
    #[clap(about = "Copy files between local and remote server", name = "cp")]
    Copy {
        #[clap(
            short,
            long,
            help = "Recursively copy entire directories.  Note that will follows symbolic links encountered in the tree traversal.",
            display_order = 1
        )]
        recursive: bool,
        #[clap(
            short,
            long,
            help = "Download the file from remote server to local machine",
            display_order = 2
        )]
        download: bool,
        #[clap(
            num_args = 1..,
            required = true,
            help = "Local files or dir",
            display_order = 3
        )]
        local: Vec<String>,
        #[clap(required = true, help = "Remote path")]
        remote: String,
    },
    #[clap(
        about = "Download the file from remote server to local machine",
        name = "dl"
    )]
    Download {
        #[clap(
            short,
            long,
            help = "Recursively copy entire directories.  Note that will follows symbolic links encountered in the tree traversal."
        )]
        recursive: bool,
        #[clap(required = true, help = "Remote path")]
        remote: String,
        #[clap(num_args = 1.., required = true, help = "Local path")]
        local: String,
    },
    #[clap(about = "Configure PSM")]
    Set {
        #[clap(short = 'k', help = "Set the SSH public key path", display_order = 1)]
        pub_key_path: Option<PathBuf>,
        #[clap(short, help = "Set the psm server file path", display_order = 2)]
        server_path: Option<PathBuf>,
        #[clap(short, help = "Set the ssh client path", display_order = 3)]
        client_path: Option<PathBuf>,
        #[clap(short = 'a', help = "Set the scp path", display_order = 4)]
        scp_path: Option<PathBuf>,
    },
    #[clap(about = "Upgrade PSM configuration and data format", name = "upgrade", display_order = 7)]
    Upgrade {},
}
