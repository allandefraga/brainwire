//! Brainwire configuration, synthetic observatory and embedded Cypher CLI.

mod admin;
mod config;
mod cypher;
mod databases;
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
    /// Execute one atomic query against an embedded local store (exclusive access).
    #[command(
        long_about = "Execute one atomic Cypher query against an embedded local store. Cannot attach to an already-open server database. Output is JSON {fields, rows}; nodes use {type: node, id, label, properties}, including entity_id. Ontology is applied separately before the query. Query memory budgets are loaded but not yet enforced; output errors may follow committed writes."
    )]
    Cypher {
        /// Directory holding the local database; RocksDB requires exclusive access.
        #[arg(long)]
        database: PathBuf,
        /// One query in the supported Cypher subset.
        #[arg(long)]
        query: String,
        /// Graph-mapping JSON used to initialize or update the ontology.
        #[arg(long)]
        ontology: Option<PathBuf>,
        /// JSON object of named parameters; integers must fit signed i64.
        #[arg(long, conflicts_with = "params_file")]
        params: Option<String>,
        /// File containing the same JSON object of named parameters as --params.
        #[arg(long, value_name = "JSON_FILE")]
        params_file: Option<PathBuf>,
        /// Config file; otherwise uses ./brainwire.toml. Environment overrides it.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Open the graph observatory. Live backend is not yet connected.
    Shell {
        /// Use the bounded synthetic demo fixture.
        #[arg(long)]
        demo: bool,
        /// Interface language: en (default) or pt-BR.
        #[arg(long, default_value = "en")]
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
    /// Manage named local databases (requires exclusive access to each store).
    Db {
        #[command(subcommand)]
        command: DbCommands,
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

#[derive(Subcommand)]
enum DbCommands {
    /// List databases in the configured data directory.
    List {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Count persisted nodes and relationships (scans the database).
    Stats {
        name: String,
        /// Emit one JSON object for scripts and future backoffice integration.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Create a database and optionally apply its ontology.
    Create {
        name: String,
        #[arg(long)]
        ontology: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Apply an additive graph-mapping JSON ontology.
    ApplyOntology {
        name: String,
        file: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Cypher {
            database,
            query,
            ontology,
            params,
            params_file,
            config,
        } => cypher::run(
            &database,
            &query,
            ontology.as_deref(),
            params.as_deref(),
            params_file.as_deref(),
            config.as_deref(),
        ),
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
        Commands::Db { command } => match command {
            DbCommands::List { config } => admin::list(config.as_deref()),
            DbCommands::Stats { name, json, config } => {
                admin::stats(&name, json, config.as_deref())
            }
            DbCommands::Create {
                name,
                ontology,
                config,
            } => admin::create(&name, ontology.as_deref(), config.as_deref()),
            DbCommands::ApplyOntology { name, file, config } => {
                admin::apply_ontology(&name, &file, config.as_deref())
            }
        },
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
