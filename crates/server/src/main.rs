//! The `brainwire` binary. Today: `config init` and `config show`. `serve`
//! and `db` subcommands land in Task 5, once the store and Bolt crates are
//! wired in.

mod config;
mod size;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "brainwire", version, about = "Brainwire graph database")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Open the graph observatory. Live backend is not yet connected.
    Shell {
        /// Use the bounded synthetic demo fixture.
        #[arg(long)]
        demo: bool,
        /// Interface language: pt-BR or en.
        #[arg(long, default_value = "pt-BR")]
        lang: String,
        /// Disable animations (the current demo uses a static layout).
        #[arg(long)]
        no_anim: bool,
        /// Render one plain-text frame without a terminal.
        #[arg(long)]
        snapshot: bool,
        /// Initial view: graph, query, memory or disk.
        #[arg(long, default_value = "graph")]
        view: String,
        /// Snapshot columns (80..240).
        #[arg(long, default_value_t = 160)]
        width: u16,
        /// Snapshot rows (24..80).
        #[arg(long, default_value_t = 48)]
        height: u16,
    },
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

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Shell {
            demo,
            lang,
            no_anim,
            snapshot,
            view,
            width,
            height,
        } => brainwire_tui::run(brainwire_tui::Options {
            demo,
            lang,
            no_anim,
            snapshot,
            view,
            width,
            height,
        }),
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

    let loaded = config::load(path.as_deref(), &env).map_err(|e| e.to_string())?;

    for (key, value, origin) in config::describe(&loaded) {
        println!("{key:<24} = {value:<14} ({origin})");
    }

    Ok(())
}
