//! The Brainwire config: defaults, an optional TOML file, and environment
//! overrides (spec "Configuration": one TOML file with environment overrides
//! for containers). Every key can be set by an environment variable named
//! `BRAINWIRE_<SECTION>_<KEY>`, uppercase, e.g. `BRAINWIRE_MEMORY_CACHE`.
//!
//! `load` is pure: given `path: None` it never looks at the current
//! directory, and it never reads `std::env::vars()` itself. `main.rs` is
//! responsible for the implicit `./brainwire.toml` lookup (`find_file`) and
//! for collecting the process environment.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::size::ByteSize;

/// The whole config: the Bolt server and the RocksDB memory budget.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub server: ServerSection,
    pub memory: MemorySection,
}

/// Bolt listener, data directory, default database and message-size limit.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSection {
    pub listen: String,
    pub data_dir: PathBuf,
    pub default_database: String,
    pub max_message: ByteSize,
    pub max_decoded_message: ByteSize,
    pub io_timeout_ms: u64,
    pub query_timeout_ms: u64,
    pub shutdown_grace_ms: u64,
    pub auth_file: Option<PathBuf>,
    /// Root for online `CALL db.checkpoint(name)`; unset disables it.
    pub checkpoint_dir: Option<PathBuf>,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            listen: "127.0.0.1:7687".to_string(),
            data_dir: PathBuf::from("./data"),
            default_database: "default".to_string(),
            max_message: ByteSize(256 << 20),
            max_decoded_message: ByteSize(64 << 20),
            io_timeout_ms: 300_000,
            query_timeout_ms: 30_000,
            // Below Docker's default 10 s stop timeout, so the drain ends first.
            shutdown_grace_ms: 8_000,
            auth_file: None,
            checkpoint_dir: None,
        }
    }
}

/// The 16 GB node's memory budget (design spec 1.5): a RocksDB block cache
/// with the memtables charged inside it, and a ceiling for query execution.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySection {
    pub cache: ByteSize,
    pub memtables: ByteSize,
    pub query: ByteSize,
}

impl Default for MemorySection {
    fn default() -> Self {
        MemorySection {
            cache: ByteSize(10 << 30),
            memtables: ByteSize(1 << 30),
            query: ByteSize(2 << 30),
        }
    }
}

/// Where one config field's effective value came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Default,
    File(PathBuf),
    Env(String),
}

/// A loaded config together with the provenance of every field, keyed by its
/// dotted name (`"server.listen"`, `"memory.cache"`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub sources: BTreeMap<&'static str, Source>,
}

/// Everything that can go wrong loading a config. Never panics on bad input.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the config file: {0}")]
    Io(String),
    #[error("cannot parse the config file: {0}")]
    Parse(String),
    #[error("bad value for `{key}`: {message}")]
    BadValue { key: String, message: String },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// The dotted keys `load` tracks provenance for, in the order `config show`
/// prints them. `pub(crate)` so `main.rs` and `describe` share this single
/// list instead of each keeping their own copy (fix round 1: a second,
/// hand-copied list in `main.rs` could drift and panic on lookup).
pub(crate) const KEYS: [&str; 13] = [
    "server.listen",
    "server.data_dir",
    "server.default_database",
    "server.max_message",
    "server.max_decoded_message",
    "server.io_timeout_ms",
    "server.query_timeout_ms",
    "server.shutdown_grace_ms",
    "server.auth_file",
    "server.checkpoint_dir",
    "memory.cache",
    "memory.memtables",
    "memory.query",
];

/// The minimum block cache: below this, RocksDB has no room left for the
/// memtables it charges inside the cache plus any actual block data.
const MIN_CACHE_BYTES: u64 = 64 << 20;

fn env_var_name(dotted_key: &str) -> String {
    format!(
        "BRAINWIRE_{}",
        dotted_key.to_ascii_uppercase().replace('.', "_")
    )
}

/// Mirrors `Config` with every field optional, so a partial TOML file can be
/// told apart from an unset one. Not part of the public API: `Config` itself
/// has no `Deserialize` impl, since `load` needs per-field provenance, not
/// just the merged values.
///
/// `deny_unknown_fields` on all three (fix round 1): without it, a typo like
/// `chace` for `cache`, or an entire bogus `[section]`, was silently ignored
/// (every key still reads as its default, exit 0) instead of being reported.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    server: FileServerSection,
    #[serde(default)]
    memory: FileMemorySection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServerSection {
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    data_dir: Option<PathBuf>,
    #[serde(default)]
    default_database: Option<String>,
    #[serde(default)]
    max_message: Option<ByteSize>,
    #[serde(default)]
    max_decoded_message: Option<ByteSize>,
    #[serde(default)]
    io_timeout_ms: Option<u64>,
    #[serde(default)]
    query_timeout_ms: Option<u64>,
    #[serde(default)]
    shutdown_grace_ms: Option<u64>,
    #[serde(default)]
    auth_file: Option<PathBuf>,
    #[serde(default)]
    checkpoint_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMemorySection {
    #[serde(default)]
    cache: Option<ByteSize>,
    #[serde(default)]
    memtables: Option<ByteSize>,
    #[serde(default)]
    query: Option<ByteSize>,
}

/// Looks for `brainwire.toml` in `cwd`, for the implicit lookup `load` itself
/// never performs. `main.rs` calls this and passes the result on as `path`.
pub fn find_file(cwd: &Path) -> Option<PathBuf> {
    let candidate = cwd.join("brainwire.toml");
    candidate.is_file().then_some(candidate)
}

/// Loads defaults, then `path` if given, then `env`. Pure: it touches neither
/// the process environment nor the current directory; the caller collects
/// both (see `find_file` and the module docs).
pub fn load(path: Option<&Path>, env: &BTreeMap<String, String>) -> Result<Loaded, ConfigError> {
    let mut config = Config::default();
    let mut sources: BTreeMap<&'static str, Source> =
        KEYS.iter().map(|&key| (key, Source::Default)).collect();

    if let Some(path) = path {
        apply_file(path, &mut config, &mut sources)?;
    }
    apply_env(env, &mut config, &mut sources)?;
    validate(&config)?;

    Ok(Loaded { config, sources })
}

fn apply_file(
    path: &Path,
    config: &mut Config,
    sources: &mut BTreeMap<&'static str, Source>,
) -> Result<(), ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
    let file: FileConfig = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;

    if let Some(v) = file.server.listen {
        config.server.listen = v;
        sources.insert("server.listen", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.data_dir {
        config.server.data_dir = v;
        sources.insert("server.data_dir", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.default_database {
        config.server.default_database = v;
        sources.insert("server.default_database", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.max_message {
        config.server.max_message = v;
        sources.insert("server.max_message", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.max_decoded_message {
        config.server.max_decoded_message = v;
        sources.insert(
            "server.max_decoded_message",
            Source::File(path.to_path_buf()),
        );
    }
    if let Some(v) = file.server.query_timeout_ms {
        // The query and network budgets remain independent.
        config.server.query_timeout_ms = v;
        sources.insert("server.query_timeout_ms", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.io_timeout_ms {
        config.server.io_timeout_ms = v;
        sources.insert("server.io_timeout_ms", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.server.shutdown_grace_ms {
        config.server.shutdown_grace_ms = v;
        sources.insert("server.shutdown_grace_ms", Source::File(path.to_path_buf()));
    }
    if let Some(path_value) = file.server.auth_file {
        config.server.auth_file = Some(auth_file_path(path_value)?);
        sources.insert("server.auth_file", Source::File(path.to_path_buf()));
    }
    if let Some(path_value) = file.server.checkpoint_dir {
        config.server.checkpoint_dir = Some(nonblank_path("server.checkpoint_dir", path_value)?);
        sources.insert("server.checkpoint_dir", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.memory.cache {
        config.memory.cache = v;
        sources.insert("memory.cache", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.memory.memtables {
        config.memory.memtables = v;
        sources.insert("memory.memtables", Source::File(path.to_path_buf()));
    }
    if let Some(v) = file.memory.query {
        config.memory.query = v;
        sources.insert("memory.query", Source::File(path.to_path_buf()));
    }

    Ok(())
}

fn apply_env(
    env: &BTreeMap<String, String>,
    config: &mut Config,
    sources: &mut BTreeMap<&'static str, Source>,
) -> Result<(), ConfigError> {
    let var = env_var_name("server.listen");
    if let Some(v) = env.get(&var) {
        config.server.listen = v.clone();
        sources.insert("server.listen", Source::Env(var));
    }

    let var = env_var_name("server.data_dir");
    if let Some(v) = env.get(&var) {
        config.server.data_dir = PathBuf::from(v);
        sources.insert("server.data_dir", Source::Env(var));
    }

    let var = env_var_name("server.default_database");
    if let Some(v) = env.get(&var) {
        config.server.default_database = v.clone();
        sources.insert("server.default_database", Source::Env(var));
    }

    let var = env_var_name("server.max_message");
    if let Some(v) = env.get(&var) {
        config.server.max_message = parse_byte_size(v, "server.max_message")?;
        sources.insert("server.max_message", Source::Env(var));
    }

    let var = env_var_name("server.query_timeout_ms");
    let io_var = env_var_name("server.io_timeout_ms");
    if let Some(v) = env.get(&io_var) {
        config.server.io_timeout_ms =
            v.parse()
                .map_err(|error: std::num::ParseIntError| ConfigError::BadValue {
                    key: "server.io_timeout_ms".into(),
                    message: error.to_string(),
                })?;
        sources.insert("server.io_timeout_ms", Source::Env(io_var));
    }
    let grace_var = env_var_name("server.shutdown_grace_ms");
    if let Some(v) = env.get(&grace_var) {
        config.server.shutdown_grace_ms =
            v.parse()
                .map_err(|error: std::num::ParseIntError| ConfigError::BadValue {
                    key: "server.shutdown_grace_ms".into(),
                    message: error.to_string(),
                })?;
        sources.insert("server.shutdown_grace_ms", Source::Env(grace_var));
    }
    let decoded_var = env_var_name("server.max_decoded_message");
    if let Some(v) = env.get(&decoded_var) {
        config.server.max_decoded_message = parse_byte_size(v, "server.max_decoded_message")?;
        sources.insert("server.max_decoded_message", Source::Env(decoded_var));
    }
    if let Some(value) = env.get(&var) {
        config.server.query_timeout_ms =
            value
                .parse()
                .map_err(|error: std::num::ParseIntError| ConfigError::BadValue {
                    key: "server.query_timeout_ms".into(),
                    message: error.to_string(),
                })?;
        sources.insert("server.query_timeout_ms", Source::Env(var));
    }

    let var = env_var_name("server.auth_file");
    if let Some(value) = env.get(&var) {
        config.server.auth_file = Some(auth_file_path(PathBuf::from(value))?);
        sources.insert("server.auth_file", Source::Env(var));
    }

    let var = env_var_name("server.checkpoint_dir");
    if let Some(value) = env.get(&var) {
        config.server.checkpoint_dir = Some(nonblank_path(
            "server.checkpoint_dir",
            PathBuf::from(value),
        )?);
        sources.insert("server.checkpoint_dir", Source::Env(var));
    }

    let var = env_var_name("memory.cache");
    if let Some(v) = env.get(&var) {
        config.memory.cache = parse_byte_size(v, "memory.cache")?;
        sources.insert("memory.cache", Source::Env(var));
    }

    let var = env_var_name("memory.memtables");
    if let Some(v) = env.get(&var) {
        config.memory.memtables = parse_byte_size(v, "memory.memtables")?;
        sources.insert("memory.memtables", Source::Env(var));
    }

    let var = env_var_name("memory.query");
    if let Some(v) = env.get(&var) {
        config.memory.query = parse_byte_size(v, "memory.query")?;
        sources.insert("memory.query", Source::Env(var));
    }

    Ok(())
}

fn auth_file_path(path: PathBuf) -> Result<PathBuf, ConfigError> {
    if path.to_string_lossy().trim().is_empty() {
        return Err(ConfigError::BadValue {
            key: "server.auth_file".into(),
            message: "must be a nonblank path; omit the setting for local development mode".into(),
        });
    }
    Ok(path)
}

fn nonblank_path(key: &str, path: PathBuf) -> Result<PathBuf, ConfigError> {
    if path.to_string_lossy().trim().is_empty() {
        return Err(ConfigError::BadValue {
            key: key.into(),
            message: "must be a nonblank path; omit the setting to disable it".into(),
        });
    }
    Ok(path)
}

fn parse_byte_size(value: &str, key: &str) -> Result<ByteSize, ConfigError> {
    value
        .parse::<ByteSize>()
        .map_err(|e| ConfigError::BadValue {
            key: key.to_string(),
            message: e.to_string(),
        })
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    if !(1..=3_600_000).contains(&config.server.io_timeout_ms) {
        return Err(ConfigError::Invalid(
            "server.io_timeout_ms must be between 1 and 3600000".into(),
        ));
    }
    if config.server.max_decoded_message.0 == 0 {
        return Err(ConfigError::Invalid(
            "server.max_decoded_message must be positive".into(),
        ));
    }
    if !(1..=3_600_000).contains(&config.server.query_timeout_ms) {
        return Err(ConfigError::Invalid(
            "server.query_timeout_ms must be between 1 and 3600000".into(),
        ));
    }
    if config.server.shutdown_grace_ms > 3_600_000 {
        return Err(ConfigError::Invalid(
            "server.shutdown_grace_ms must be between 0 and 3600000".into(),
        ));
    }
    if let Some(root) = &config.server.checkpoint_dir {
        // Lexical guard: a checkpoint under data_dir would reopen as a named graph.
        if root.starts_with(&config.server.data_dir) || config.server.data_dir.starts_with(root) {
            return Err(ConfigError::Invalid(
                "server.checkpoint_dir must be outside server.data_dir and not contain it".into(),
            ));
        }
    }
    if config.memory.query.0 == 0 {
        return Err(ConfigError::Invalid("memory.query must be positive".into()));
    }
    if !crate::databases::valid_name(&config.server.default_database) {
        return Err(ConfigError::Invalid(format!(
            "server.default_database must use 1..128 ASCII letters, digits, hyphens or underscores, got {:?}",
            config.server.default_database
        )));
    }
    if config.memory.memtables.0 >= config.memory.cache.0 {
        return Err(ConfigError::Invalid(format!(
            "memory.memtables ({}) must be smaller than memory.cache ({}); the memtables are charged inside the cache",
            config.memory.memtables, config.memory.cache
        )));
    }

    if config.memory.cache.0 < MIN_CACHE_BYTES {
        return Err(ConfigError::Invalid(format!(
            "memory.cache must be at least {}, got {}",
            ByteSize(MIN_CACHE_BYTES),
            config.memory.cache
        )));
    }

    if config.server.listen.parse::<SocketAddr>().is_err() {
        return Err(ConfigError::Invalid(format!(
            "server.listen must be a socket address (host:port), got {:?}",
            config.server.listen
        )));
    }

    Ok(())
}

/// A commented TOML file with every key at its default. `load` reads this
/// back to exactly `Config::default()` (see the round-trip test below).
pub fn default_file() -> String {
    let d = Config::default();
    format!(
        "# Brainwire configuration.\n\
         # Every value below can be overridden by an environment variable\n\
         # named BRAINWIRE_<SECTION>_<KEY>, uppercase, e.g. BRAINWIRE_MEMORY_CACHE.\n\
         \n\
         [server]\n\
         # Address the Bolt server listens on.\n\
         listen = \"{listen}\"\n\
         # Directory holding one subdirectory per database.\n\
         data_dir = \"{data_dir}\"\n\
         # Database used when a driver does not name one in BEGIN or RUN.\n\
         default_database = \"{default_database}\"\n\
         # Largest Bolt message accepted from a client.\n\
         max_message = \"{max_message}\"\n\
         # Requested owned PackStream allocations per message, before query execution (not RSS).\n\
         max_decoded_message = \"{max_decoded_message}\"\n\
         # Complete authenticated request/response IO budget; query execution is separate.\n\
         io_timeout_ms = {io_timeout_ms}\n\
         \n\
         # Cooperative per-RUN deadline; storage calls and commit can overrun.\n\
         query_timeout_ms = {query_timeout_ms}\n\
         \n\
         # SIGTERM/SIGINT drain: idle sessions close at once; open transactions\n\
         # and result streams get this long before the process exits.\n\
         shutdown_grace_ms = {shutdown_grace_ms}\n\
         \n\
         # Optional protected Bolt policy; omit for local development mode.\n\
         # auth_file = \"/run/secrets/brainwire-policy.json\"\n\
         \n\
         # Optional root for online admin `CALL db.checkpoint(name)`; must be\n\
         # outside data_dir. Omit to disable online checkpoints.\n\
         # checkpoint_dir = \"/backups\"\n\
         \n\
         [memory]\n\
         # RocksDB block cache. The memtables below are charged inside it.\n\
         cache = \"{cache}\"\n\
         # Total memtable budget across all open databases, charged inside `cache`.\n\
         memtables = \"{memtables}\"\n\
         # Shared server query reservation pool and per-RUN guard (not process RSS).\n\
         query = \"{query}\"\n",
        listen = d.server.listen,
        data_dir = d.server.data_dir.display(),
        default_database = d.server.default_database,
        max_message = d.server.max_message,
        max_decoded_message = d.server.max_decoded_message,
        io_timeout_ms = d.server.io_timeout_ms,
        query_timeout_ms = d.server.query_timeout_ms,
        shutdown_grace_ms = d.server.shutdown_grace_ms,
        cache = d.memory.cache,
        memtables = d.memory.memtables,
        query = d.memory.query,
    )
}

/// One `(key, value, source)` row per entry in `KEYS`, in that order —
/// `config show`'s only source of what to print; `main.rs` just formats
/// these rows (fix round 1: this replaces a second, hand-copied key list and
/// a `loaded.sources[key]` index in `main.rs`, either of which panicked if it
/// ever drifted from this module's own `KEYS`/field set).
///
/// Never panics: a key in `KEYS` with no matching entry in `loaded.sources`
/// (which `load` always populates from this same `KEYS`, so this should not
/// happen) reads as `"default"` rather than indexing the map; a key with no
/// matching arm in the value lookup (same non-happening case) reads as an
/// empty value rather than hitting an `unreachable!()`.
pub fn describe(loaded: &Loaded) -> Vec<(&'static str, String, String)> {
    KEYS.iter()
        .map(|&key| {
            let value = value_text(&loaded.config, key);
            let source = loaded.sources.get(key).cloned().unwrap_or(Source::Default);
            (key, value, source_text(&source))
        })
        .collect()
}

fn value_text(config: &Config, key: &str) -> String {
    match key {
        "server.listen" => config.server.listen.clone(),
        "server.data_dir" => config.server.data_dir.display().to_string(),
        "server.default_database" => config.server.default_database.clone(),
        "server.max_message" => config.server.max_message.to_string(),
        "server.max_decoded_message" => config.server.max_decoded_message.to_string(),
        "server.io_timeout_ms" => config.server.io_timeout_ms.to_string(),
        "server.query_timeout_ms" => config.server.query_timeout_ms.to_string(),
        "server.shutdown_grace_ms" => config.server.shutdown_grace_ms.to_string(),
        "server.auth_file" => config
            .server
            .auth_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<unset>".into()),
        "server.checkpoint_dir" => config
            .server
            .checkpoint_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<unset>".into()),
        "memory.cache" => config.memory.cache.to_string(),
        "memory.memtables" => config.memory.memtables.to_string(),
        "memory.query" => config.memory.query.to_string(),
        _ => String::new(),
    }
}

fn source_text(source: &Source) -> String {
    match source {
        Source::Default => "default".to_string(),
        Source::File(p) => format!("file {}", p.display()),
        Source::Env(v) => format!("env {v}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn decoded_message_limit_has_independent_default_file_and_env_provenance() {
        let loaded = load(None, &env(&[])).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.max_decoded_message"
                && value == "64 MiB"
                && source == "default"));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[server]\nmax_decoded_message='16MiB'\n").unwrap();
        let loaded = load(Some(&file), &env(&[])).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.max_decoded_message"
                && value == "16 MiB"
                && source.starts_with("file ")));
        let loaded = load(
            Some(&file),
            &env(&[("BRAINWIRE_SERVER_MAX_DECODED_MESSAGE", "8MiB")]),
        )
        .unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.max_decoded_message"
                && value == "8 MiB"
                && source == "env BRAINWIRE_SERVER_MAX_DECODED_MESSAGE"));
        assert_eq!(loaded.config.server.max_message.0, 256 << 20);
        for invalid in ["0", "invalid", "18446744073709551616"] {
            assert!(load(
                None,
                &env(&[("BRAINWIRE_SERVER_MAX_DECODED_MESSAGE", invalid)])
            )
            .is_err());
        }
    }

    #[test]
    fn checkpoint_dir_is_optional_nonblank_and_outside_data_dir() {
        let loaded = load(None, &env(&[])).unwrap();
        assert_eq!(loaded.config.server.checkpoint_dir, None);
        let loaded = load(
            None,
            &env(&[("BRAINWIRE_SERVER_CHECKPOINT_DIR", "/backups")]),
        )
        .unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.checkpoint_dir"
                && value == "/backups"
                && source == "env BRAINWIRE_SERVER_CHECKPOINT_DIR"));
        for invalid in ["", "  ", "./data", "./data/backups", "."] {
            assert!(
                load(None, &env(&[("BRAINWIRE_SERVER_CHECKPOINT_DIR", invalid)])).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn auth_file_defaults_unset_and_reports_path_provenance() {
        let loaded = load(None, &env(&[])).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.auth_file"
                && value == "<unset>"
                && source == "default"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brainwire.toml");
        std::fs::write(&path, "[server]\nauth_file='/run/secrets/policy.json'\n").unwrap();
        let loaded = load(Some(&path), &env(&[])).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.auth_file"
                && value == "/run/secrets/policy.json"
                && source.starts_with("file ")));
        let loaded = load(
            Some(&path),
            &env(&[("BRAINWIRE_SERVER_AUTH_FILE", "/run/secrets/override.json")]),
        )
        .unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.auth_file"
                && value == "/run/secrets/override.json"
                && source == "env BRAINWIRE_SERVER_AUTH_FILE"));
    }

    #[test]
    fn io_budget_is_positive_independent_and_reports_provenance() {
        let loaded = load(None, &env(&[])).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.io_timeout_ms"
                && value == "300000"
                && source == "default"));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[server]\nio_timeout_ms=1000\n").unwrap();
        let loaded = load(
            Some(&file),
            &env(&[("BRAINWIRE_SERVER_IO_TIMEOUT_MS", "2000")]),
        )
        .unwrap();
        assert_eq!(loaded.config.server.io_timeout_ms, 2000);
        assert_eq!(loaded.config.server.query_timeout_ms, 30000);
        assert!(matches!(
            loaded.sources["server.io_timeout_ms"],
            Source::Env(_)
        ));
        for invalid in ["0", "3600001", "-1", "2.5", ""] {
            assert!(load(None, &env(&[("BRAINWIRE_SERVER_IO_TIMEOUT_MS", invalid)])).is_err());
        }
    }

    #[test]
    fn auth_file_rejects_explicit_blank_paths_in_file_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brainwire.toml");
        for blank in ["", " ", "\t"] {
            let error = load(None, &env(&[("BRAINWIRE_SERVER_AUTH_FILE", blank)])).unwrap_err();
            assert!(error.to_string().contains("server.auth_file"));
            std::fs::write(&path, format!("[server]\nauth_file={blank:?}\n")).unwrap();
            let error = load(Some(&path), &env(&[])).unwrap_err();
            assert!(error.to_string().contains("auth_file"));
            // An invalid explicit file setting is never silently overridden into access.
            assert!(load(
                Some(&path),
                &env(&[("BRAINWIRE_SERVER_AUTH_FILE", "/run/secrets/policy.json")])
            )
            .is_err());
        }
        std::fs::write(&path, "[server]\nauth_file='/run/secrets/policy.json'\n").unwrap();
        assert!(load(Some(&path), &env(&[("BRAINWIRE_SERVER_AUTH_FILE", "")])).is_err());
    }

    #[test]
    fn defaults_follow_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.toml");
        let loaded = load(Some(&missing), &env(&[])).unwrap_err();
        assert!(matches!(loaded, ConfigError::Io(_)));
        let loaded = load(None, &env(&[])).unwrap();
        assert_eq!(loaded.config.memory.cache.0, 10 << 30);
        assert_eq!(loaded.config.memory.memtables.0, 1 << 30);
        assert_eq!(loaded.config.memory.query.0, 2 << 30);
        assert_eq!(loaded.config.server.default_database, "default");
        assert!(matches!(loaded.sources["memory.cache"], Source::Default));
    }

    #[test]
    fn default_database_cannot_escape_the_data_directory() {
        let error = load(
            None,
            &env(&[("BRAINWIRE_SERVER_DEFAULT_DATABASE", "../outside")]),
        )
        .unwrap_err();
        assert!(
            matches!(error, ConfigError::Invalid(message) if message.contains("server.default_database"))
        );
    }

    #[test]
    fn file_then_env_override_and_sources_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(
            &file,
            "[memory]\ncache = \"4GiB\"\n[server]\nlisten = \"0.0.0.0:7687\"\n",
        )
        .unwrap();
        let loaded = load(
            Some(&file),
            &env(&[("BRAINWIRE_MEMORY_MEMTABLES", "512MiB")]),
        )
        .unwrap();
        assert_eq!(loaded.config.memory.cache.0, 4 << 30);
        assert_eq!(loaded.config.memory.memtables.0, 512 << 20);
        assert_eq!(loaded.config.server.listen, "0.0.0.0:7687");
        assert!(matches!(loaded.sources["memory.cache"], Source::File(_)));
        assert!(
            matches!(&loaded.sources["memory.memtables"], Source::Env(v) if v == "BRAINWIRE_MEMORY_MEMTABLES")
        );
        assert!(matches!(loaded.sources["memory.query"], Source::Default));
    }

    #[test]
    fn memtables_must_fit_inside_the_cache() {
        let err = load(
            None,
            &env(&[
                ("BRAINWIRE_MEMORY_CACHE", "1GiB"),
                ("BRAINWIRE_MEMORY_MEMTABLES", "1GiB"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn shutdown_grace_has_file_env_provenance_and_rejects_invalid_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("brainwire.toml");
        std::fs::write(&path, "[server]\nshutdown_grace_ms=0\n").unwrap();
        let loaded = load(Some(&path), &BTreeMap::new()).unwrap();
        assert_eq!(loaded.config.server.shutdown_grace_ms, 0);
        let env = BTreeMap::from([("BRAINWIRE_SERVER_SHUTDOWN_GRACE_MS".into(), "25000".into())]);
        let loaded = load(Some(&path), &env).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.shutdown_grace_ms"
                && value == "25000"
                && source.starts_with("env ")));
        for bad in ["3600001", "oops", "-1"] {
            let env = BTreeMap::from([("BRAINWIRE_SERVER_SHUTDOWN_GRACE_MS".into(), bad.into())]);
            assert!(load(None, &env).is_err(), "{bad}");
        }
        assert_eq!(Config::default().server.shutdown_grace_ms, 8_000);
    }

    #[test]
    fn query_timeout_has_file_env_provenance_and_rejects_invalid_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("brainwire.toml");
        std::fs::write(&path, "[server]\nquery_timeout_ms=750\n").unwrap();
        let loaded = load(Some(&path), &BTreeMap::new()).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.query_timeout_ms"
                && value == "750"
                && source.starts_with("file ")));
        let env = BTreeMap::from([("BRAINWIRE_SERVER_QUERY_TIMEOUT_MS".into(), "1250".into())]);
        let loaded = load(Some(&path), &env).unwrap();
        assert!(describe(&loaded)
            .iter()
            .any(|(key, value, source)| *key == "server.query_timeout_ms"
                && value == "1250"
                && source.starts_with("env ")));
        for bad in ["0", "3600001", "oops", "-1"] {
            assert!(
                load(
                    None,
                    &BTreeMap::from([("BRAINWIRE_SERVER_QUERY_TIMEOUT_MS".into(), bad.into())])
                )
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn query_allocation_guard_must_be_positive() {
        let err = load(None, &env(&[("BRAINWIRE_MEMORY_QUERY", "0")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(message) if message.contains("memory.query")));
    }

    #[test]
    fn a_bad_value_names_its_key() {
        let err = load(None, &env(&[("BRAINWIRE_MEMORY_CACHE", "lots")])).unwrap_err();
        assert!(matches!(err, ConfigError::BadValue { ref key, .. } if key == "memory.cache"));
    }

    #[test]
    fn the_default_file_loads_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, default_file()).unwrap();
        let from_file = load(Some(&file), &env(&[])).unwrap().config;
        let defaults = load(None, &env(&[])).unwrap().config;
        assert_eq!(from_file, defaults);
    }

    // Beyond the brief: fill in coverage for the remaining public functions,
    // the remaining validation rules and the remaining ConfigError variants.

    #[test]
    fn find_file_locates_brainwire_toml_in_cwd() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_file(dir.path()), None);
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "").unwrap();
        assert_eq!(find_file(dir.path()), Some(file));
    }

    #[test]
    fn cache_below_the_floor_is_invalid() {
        let err = load(
            None,
            &env(&[
                ("BRAINWIRE_MEMORY_CACHE", "32MiB"),
                ("BRAINWIRE_MEMORY_MEMTABLES", "1MiB"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn listen_must_parse_as_a_socket_address() {
        let err = load(None, &env(&[("BRAINWIRE_SERVER_LISTEN", "not-an-address")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "this is not [[[ valid toml").unwrap();
        let err = load(Some(&file), &env(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn wrong_type_for_a_known_key_is_a_parse_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[memory]\ncache = true\n").unwrap();
        let err = load(Some(&file), &env(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn config_error_messages_are_human_readable() {
        assert_eq!(
            ConfigError::BadValue {
                key: "memory.cache".to_string(),
                message: "bad".to_string()
            }
            .to_string(),
            "bad value for `memory.cache`: bad"
        );
        assert_eq!(
            ConfigError::Io("boom".to_string()).to_string(),
            "cannot read the config file: boom"
        );
        assert_eq!(
            ConfigError::Parse("boom".to_string()).to_string(),
            "cannot parse the config file: boom"
        );
        assert_eq!(
            ConfigError::Invalid("boom".to_string()).to_string(),
            "invalid configuration: boom"
        );
    }

    #[test]
    fn env_overrides_every_key() {
        let loaded = load(
            None,
            &env(&[
                ("BRAINWIRE_SERVER_LISTEN", "0.0.0.0:9999"),
                ("BRAINWIRE_SERVER_DATA_DIR", "/tmp/brainwire-data"),
                ("BRAINWIRE_SERVER_DEFAULT_DATABASE", "other_db"),
                ("BRAINWIRE_SERVER_MAX_MESSAGE", "512MiB"),
                ("BRAINWIRE_SERVER_MAX_DECODED_MESSAGE", "32MiB"),
                ("BRAINWIRE_SERVER_IO_TIMEOUT_MS", "12000"),
                ("BRAINWIRE_SERVER_QUERY_TIMEOUT_MS", "1250"),
                ("BRAINWIRE_SERVER_SHUTDOWN_GRACE_MS", "20000"),
                ("BRAINWIRE_SERVER_AUTH_FILE", "/run/secrets/policy.json"),
                ("BRAINWIRE_SERVER_CHECKPOINT_DIR", "/backups"),
                ("BRAINWIRE_MEMORY_CACHE", "1GiB"),
                ("BRAINWIRE_MEMORY_MEMTABLES", "64MiB"),
                ("BRAINWIRE_MEMORY_QUERY", "128MiB"),
            ]),
        )
        .unwrap();
        assert_eq!(loaded.config.server.listen, "0.0.0.0:9999");
        assert_eq!(
            loaded.config.server.data_dir,
            PathBuf::from("/tmp/brainwire-data")
        );
        assert_eq!(loaded.config.server.default_database, "other_db");
        assert_eq!(loaded.config.server.max_message.0, 512 << 20);
        assert_eq!(loaded.config.server.max_decoded_message.0, 32 << 20);
        assert_eq!(loaded.config.server.io_timeout_ms, 12000);
        assert_eq!(loaded.config.server.query_timeout_ms, 1250);
        assert_eq!(loaded.config.server.shutdown_grace_ms, 20000);
        assert_eq!(
            loaded.config.server.auth_file.as_deref(),
            Some(Path::new("/run/secrets/policy.json"))
        );
        assert_eq!(
            loaded.config.server.checkpoint_dir.as_deref(),
            Some(Path::new("/backups"))
        );
        assert_eq!(loaded.config.memory.cache.0, 1 << 30);
        assert_eq!(loaded.config.memory.memtables.0, 64 << 20);
        assert_eq!(loaded.config.memory.query.0, 128 << 20);
        for key in KEYS {
            assert!(
                matches!(&loaded.sources[key], Source::Env(_)),
                "key {key} was not sourced from env"
            );
        }
    }

    // Fix round 1 (reviewer findings): unknown TOML keys must be refused,
    // and `config show`'s key/value/source listing must come from one
    // function in this module with no panicking indexing, rather than a
    // second hand-copied key list in main.rs.

    #[test]
    fn an_unknown_key_in_a_known_section_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[memory]\nchace = \"4GiB\"\n").unwrap();
        let err = load(Some(&file), &env(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(ref msg) if msg.contains("chace")));
    }

    #[test]
    fn an_unknown_top_level_section_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[bogus]\nx = 1\n").unwrap();
        let err = load(Some(&file), &env(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn describe_returns_one_row_per_key_in_order() {
        let loaded = load(None, &env(&[])).unwrap();
        let rows = describe(&loaded);
        let keys: Vec<&str> = rows.iter().map(|(k, _, _)| *k).collect();
        assert_eq!(keys, KEYS.to_vec());
    }

    #[test]
    fn describe_reports_value_and_source_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[memory]\ncache = \"4GiB\"\n").unwrap();
        let loaded = load(
            Some(&file),
            &env(&[("BRAINWIRE_MEMORY_MEMTABLES", "512MiB")]),
        )
        .unwrap();
        let rows: BTreeMap<&str, (String, String)> = describe(&loaded)
            .into_iter()
            .map(|(k, v, s)| (k, (v, s)))
            .collect();
        assert_eq!(
            rows["memory.cache"],
            ("4 GiB".to_string(), format!("file {}", file.display()))
        );
        assert_eq!(
            rows["memory.memtables"],
            (
                "512 MiB".to_string(),
                "env BRAINWIRE_MEMORY_MEMTABLES".to_string()
            )
        );
        assert_eq!(
            rows["memory.query"],
            ("2 GiB".to_string(), "default".to_string())
        );
    }
}
