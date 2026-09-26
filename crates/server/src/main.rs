//! The `brainwire` binary. Today: `config init` and `config show`. `serve`
//! and `db` subcommands land in Task 5, once the store and Bolt crates are
//! wired in.

mod config;
mod size;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use config::{Config, ConfigError, Source};

#[derive(Parser)]
#[command(name = "brainwire", version, about = "Brainwire graph database")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect or create the config file.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Write a config file with every key at its default.
    Init {
        /// Where to write the file.
        #[arg(long, default_value = "brainwire.toml")]
        path: PathBuf,
        /// Overwrite the file if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Print the effective config: each key's value and where it came from.
    Show {
        /// Config file to load. Defaults to ./brainwire.toml if it exists.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

/// The dotted keys `config show` prints, in this order.
const KEYS: [&str; 7] = [
    "server.listen",
    "server.data_dir",
    "server.default_database",
    "server.max_message",
    "memory.cache",
    "memory.memtables",
    "memory.query",
];

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Config { command } => match command {
            ConfigCommands::Init { path, force } => run_config_init(&path, force),
            ConfigCommands::Show { config } => run_config_show(config.as_deref()),
        },
    };

    if let Err(message) = result {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run_config_init(path: &Path, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            path.display()
        ));
    }
    std::fs::write(path, config::default_file())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn run_config_show(explicit_path: Option<&Path>) -> Result<(), String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let path = match explicit_path {
        Some(p) => Some(p.to_path_buf()),
        None => {
            let cwd = std::env::current_dir()
                .map_err(|e| format!("cannot read the current directory: {e}"))?;
            config::find_file(&cwd)
        }
    };

    let loaded = config::load(path.as_deref(), &env).map_err(|e: ConfigError| e.to_string())?;

    for key in KEYS {
        let value = display_value(&loaded.config, key);
        let origin = match &loaded.sources[key] {
            Source::Default => "default".to_string(),
            Source::File(p) => format!("file {}", p.display()),
            Source::Env(v) => format!("env {v}"),
        };
        println!("{key:<24} = {value:<14} ({origin})");
    }

    Ok(())
}

fn display_value(config: &Config, key: &str) -> String {
    match key {
        "server.listen" => config.server.listen.clone(),
        "server.data_dir" => config.server.data_dir.display().to_string(),
        "server.default_database" => config.server.default_database.clone(),
        "server.max_message" => config.server.max_message.to_string(),
        "memory.cache" => config.memory.cache.to_string(),
        "memory.memtables" => config.memory.memtables.to_string(),
        "memory.query" => config.memory.query.to_string(),
        _ => unreachable!("display_value called with an unknown key {key:?}"),
    }
}
