//! Brainwire configuration, synthetic observatory and embedded Cypher CLI.

mod admin;
mod config;
mod cypher;
mod databases;
mod engine;
mod probe;
mod remote;
mod serve;
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
    /// Check whether a local Bolt server accepts sessions and queries.
    Probe {
        /// Bolt listen address to check from this machine or container.
        #[arg(long, default_value = "127.0.0.1:7687")]
        address: String,
        /// Live checks a Bolt session; ready also runs a read-only query.
        #[arg(long, value_enum, default_value_t = probe::Mode::Ready)]
        mode: probe::Mode,
        /// Total deadline for connection and probe exchange in milliseconds.
        #[arg(long, default_value_t = 2000)]
        timeout_ms: u64,
    },
    /// Serve named durable databases over Bolt/Cypher.
    Serve {
        /// Config file; otherwise uses ./brainwire.toml. Environment overrides it.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Execute one atomic query against an embedded local store (exclusive access).
    #[command(
        long_about = "Execute one atomic Cypher query against an embedded local store. Cannot attach to an already-open server database. Output is JSON {fields, rows}; nodes use {type: node, id, label, properties}, including entity_id. Ontology is applied separately before the query. memory.query bounds conservative per-RUN allocation accounting, not process RSS; output errors may follow committed writes."
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
    /// Open the synthetic observatory, local overview or server ontology snapshot.
    Shell {
        /// Use the bounded synthetic demo fixture.
        #[arg(long, conflicts_with = "overview")]
        demo: bool,
        /// Inspect every named local graph (requires exclusive database access).
        #[arg(long, conflicts_with = "demo")]
        overview: bool,
        /// Fetch a read-only ontology snapshot over Bolt (IP:PORT; --view ontology).
        #[arg(long, conflicts_with_all = ["demo", "overview", "config", "estimated"], requires = "graph")]
        connect: Option<String>,
        /// Total connection/fetch deadline in milliseconds (1..10000).
        #[arg(long, requires = "connect", default_value_t = 3000)]
        timeout_ms: u64,
        /// Config file for --overview; otherwise uses ./brainwire.toml.
        #[arg(long, requires = "overview")]
        config: Option<PathBuf>,
        /// Named graph for --overview or --connect; required with --connect.
        #[arg(long)]
        graph: Option<String>,
        /// Use quick RocksDB key-count estimates instead of full scans.
        #[arg(long, requires = "overview")]
        estimated: bool,
        /// Interface language: en (default) or pt-BR.
        #[arg(long, default_value = "en")]
        lang: String,
        /// Disable animations (the current demo uses a static layout).
        #[arg(long)]
        no_anim: bool,
        /// Render one plain-text frame without a terminal.
        #[arg(long)]
        snapshot: bool,
        /// Initial view: graph, query, memory, disk; ontology with --overview or --connect.
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
    /// Summarize every named graph, including exact node and relationship counts.
    Overview {
        /// Emit one JSON document for the backoffice.
        #[arg(long)]
        json: bool,
        /// Use quick RocksDB key-count estimates instead of full scans.
        #[arg(long)]
        estimated: bool,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Audit graph and ready scalar-index consistency (full offline scan).
    Check {
        name: String,
        #[arg(long)]
        json: bool,
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
    /// Show the latest durable local commit sequence without scanning graph records.
    CommitState {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Show the persisted ontology catalog and current index implementation status.
    Schema {
        name: String,
        /// Emit the full catalog as JSON.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Create one graph's consistent local checkpoint (offline; destination must be new).
    Checkpoint {
        name: String,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Inspect database file metadata and native SST families/levels (offline).
    Files {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 256)]
        limit: usize,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Show per-column-family RocksDB estimates (offline administration).
    Metrics {
        name: String,
        /// Emit metrics as JSON.
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
        Commands::Probe {
            address,
            mode,
            timeout_ms,
        } => probe::run(&address, mode, timeout_ms),
        Commands::Serve { config } => serve::run(config.as_deref()),
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
            overview,
            connect,
            timeout_ms,
            config,
            graph,
            estimated,
            lang,
            no_anim,
            snapshot,
            view,
            width,
            height,
        } => {
            let options = brainwire_tui::Options {
                demo,
                lang,
                no_anim,
                snapshot,
                view,
                width,
                height,
            };
            if let Some(address) = connect {
                let graph = graph
                    .as_deref()
                    .expect("clap requires --graph with --connect");
                brainwire_tui::run_connected_ontology(&options, &address, graph, || {
                    remote::schema(&address, graph, timeout_ms)
                })
            } else if overview {
                // Opening the schema should never require a full instance count scan.
                let quick_counts = estimated || options.view == "ontology";
                admin::graph_summaries(config.as_deref(), quick_counts).and_then(|graphs| {
                    brainwire_tui::run_overview(&options, graphs, graph.as_deref())
                })
            } else {
                if graph.is_some() {
                    Err("--graph requires --overview or --connect".into())
                } else {
                    brainwire_tui::run(options)
                }
            }
        }
        Commands::Db { command } => match command {
            DbCommands::List { config } => admin::list(config.as_deref()),
            DbCommands::Overview {
                json,
                estimated,
                config,
            } => admin::overview(json, estimated, config.as_deref()),
            DbCommands::Check { name, json, config } => {
                admin::check(&name, json, config.as_deref())
            }
            DbCommands::Stats { name, json, config } => {
                admin::stats(&name, json, config.as_deref())
            }
            DbCommands::CommitState { name, json, config } => {
                admin::commit_state(&name, json, config.as_deref())
            }
            DbCommands::Schema { name, json, config } => {
                admin::schema(&name, json, config.as_deref())
            }
            DbCommands::Checkpoint {
                name,
                destination,
                config,
            } => admin::checkpoint(&name, &destination, config.as_deref()),
            DbCommands::Files {
                name,
                json,
                limit,
                config,
            } => admin::files(&name, json, limit, config.as_deref()),
            DbCommands::Metrics { name, json, config } => {
                admin::metrics(&name, json, config.as_deref())
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
