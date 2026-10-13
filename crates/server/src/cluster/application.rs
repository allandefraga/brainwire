//! The committed graph registry and concrete per-graph state machine.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use brainwire_core::schema::{Catalog, GraphMapping};
use brainwire_replication::{Application, Command, Reply};
use brainwire_store::replicated_apply::{self, ApplyLimits, Batch, Mutation};

use crate::databases::{valid_name, Databases};

pub(crate) fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|_| "cannot read cluster file")?;
    let mut bytes = Vec::new();
    file.take((limit as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read cluster file")?;
    if bytes.len() > limit {
        return Err("cluster file exceeds limit".into());
    }
    Ok(bytes)
}

pub(super) fn durable_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(value).map_err(|_| "cannot encode cluster metadata")?;
    let temporary = path.with_extension("pending");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "cannot stage cluster metadata")?;
    file.write_all(&bytes)
        .map_err(|_| "cannot write cluster metadata")?;
    file.sync_all()
        .map_err(|_| "cannot sync cluster metadata")?;
    fs::rename(&temporary, path).map_err(|_| "cannot publish cluster metadata")?;
    fs::File::open(path.parent().ok_or("metadata parent missing")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync metadata directory")?;
    Ok(())
}

pub(crate) fn validate_graphs(graphs: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    if graphs.is_empty()
        || graphs.len() > 64
        || graphs.values().map(Vec::len).sum::<usize>() > (1 << 20)
    {
        return Err("registry requires 1..64 graphs and at most 1 MiB of schemas".into());
    }
    for (name, bytes) in graphs {
        if !valid_name(name) {
            return Err("invalid graph name".into());
        }
        let mapping: GraphMapping =
            serde_json::from_slice(bytes).map_err(|_| "invalid ontology JSON")?;
        Catalog::apply(&mapping, None).map_err(|_| "invalid initial ontology")?;
    }
    Ok(())
}

pub(crate) struct GraphApplication {
    databases: Arc<Databases>,
    registry_path: PathBuf,
    graphs: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl GraphApplication {
    pub(crate) fn open(databases: Arc<Databases>, root: &Path) -> Result<Self, String> {
        let registry_path = root.join(".brainwire-graphs.json");
        let graphs = if registry_path.exists() {
            let bytes = bounded_read(&registry_path, 8 << 20)?;
            if bytes.len() > 8 << 20 {
                return Err("graph registry exceeds limit".into());
            }
            let graphs =
                serde_json::from_slice(&bytes).map_err(|_| "invalid committed graph registry")?;
            validate_graphs(&graphs)?;
            graphs
        } else {
            BTreeMap::new()
        };
        let app = Self {
            databases,
            registry_path,
            graphs: Mutex::new(graphs),
        };
        let graphs = app.graphs.lock().map_err(|_| "registry lock poisoned")?;
        // A published registry contains fully initialized stores. Pending creation
        // before publication is recovered by replay, never by guessing on disk.
        for (name, bytes) in graphs.iter() {
            let path = root.join(name);
            if !path.join("CURRENT").is_file() {
                return Err("committed graph store missing; rebuild this replica from retained consensus history".into());
            }
            let database = app.databases.create(name)?;
            let mapping: GraphMapping =
                serde_json::from_slice(bytes).map_err(|_| "invalid committed ontology")?;
            let expected =
                Catalog::apply(&mapping, None).map_err(|_| "invalid initial ontology")?;
            if database
                .catalog()
                .map_err(|_| "cannot recover graph catalog")?
                .as_ref()
                != Some(&expected)
            {
                return Err("committed graph catalog missing or changed; refusing recovery".into());
            }
        }
        drop(graphs);
        Ok(app)
    }

    fn materialize(&self, graphs: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
        for (name, bytes) in graphs {
            let database = self.databases.create(name)?;
            // Only a committed entry may initialize an unpublished store. Publish
            // the registry afterwards, so startup never recreates a lost graph.
            if database
                .catalog()
                .map_err(|_| "cannot inspect graph ontology")?
                .is_none()
            {
                let mapping =
                    serde_json::from_slice(bytes).map_err(|_| "invalid committed ontology")?;
                database
                    .apply_ontology(&mapping)
                    .map_err(|_| "cannot initialize committed ontology")?;
            }
        }
        Ok(())
    }
}

impl Application for GraphApplication {
    fn apply(&self, command: &Command) -> Result<Reply, String> {
        match command {
            Command::Bootstrap {
                graphs: requested, ..
            } => {
                if validate_graphs(requested).is_err() {
                    return Ok(Reply::Conflict);
                }
                let mut graphs = self.graphs.lock().map_err(|_| "registry lock poisoned")?;
                if !graphs.is_empty() && *graphs != *requested {
                    return Ok(Reply::Conflict);
                }
                self.materialize(requested)?;
                durable_json(&self.registry_path, requested)?;
                *graphs = requested.clone();
                Ok(Reply::Initialized)
            }
            Command::CreateGraph { graph, schema, .. } => {
                let mut graphs = self.graphs.lock().map_err(|_| "registry lock poisoned")?;
                if let Some(existing) = graphs.get(graph) {
                    return Ok(if existing == schema {
                        Reply::Initialized
                    } else {
                        Reply::Conflict
                    });
                }
                let mut next = graphs.clone();
                next.insert(graph.clone(), schema.clone());
                if validate_graphs(&next).is_err() {
                    return Ok(Reply::Conflict);
                }
                self.materialize(&BTreeMap::from([(graph.clone(), schema.clone())]))?;
                durable_json(&self.registry_path, &next)?;
                *graphs = next;
                Ok(Reply::Initialized)
            }
            Command::Write {
                request_id,
                graph,
                expected_revision,
                batch,
            } => {
                let Some(database) = self.databases.get(graph) else {
                    return Ok(Reply::Conflict);
                };
                let limits = ApplyLimits {
                    max_bytes: 1 << 20,
                    ..ApplyLimits::default()
                };
                let Ok(mut batch) = Batch::from_canonical_bytes(batch, limits) else {
                    return Ok(Reply::Conflict);
                };
                if expected_revision.checked_add(1) != Some(batch.index) {
                    return Ok(Reply::Conflict);
                }
                const REQUEST_KEY: &[u8] = b"raft/command/v1";
                if batch
                    .mutations
                    .iter()
                    .any(|mutation| mutation.cf == "meta" && mutation.key == REQUEST_KEY)
                {
                    return Ok(Reply::Conflict);
                }
                // Bind the atomic graph ledger to this global command, so a
                // distinct stale request cannot masquerade as crash replay.
                batch.mutations.push(Mutation {
                    cf: "meta".into(),
                    key: REQUEST_KEY.to_vec(),
                    value: Some(request_id.as_bytes().to_vec()),
                });
                if batch.digest(limits).is_err() {
                    return Ok(Reply::Conflict);
                }
                let current = replicated_apply::applied_state(&database)
                    .map_err(|_| "invalid graph ledger")?;
                let revision = current.map_or(0, |state| state.index);
                if revision == batch.index {
                    if current.is_some_and(|state| batch.digest(limits).ok() == Some(state.digest))
                    {
                        return Ok(Reply::Applied { revision });
                    }
                    return Ok(Reply::Conflict);
                }
                if revision != *expected_revision {
                    return Ok(Reply::Conflict);
                }
                replicated_apply::apply(&database, &batch, limits)
                    .map_err(|_| "committed graph application failed")?;
                Ok(Reply::Applied {
                    revision: batch.index,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use brainwire_cypher::QueryTransaction;

    fn config(root: &Path) -> Config {
        let mut config = Config::default();
        config.server.data_dir = root.into();
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        config
    }
    fn schema() -> Vec<u8> {
        br#"{"version":"1","nodes":[{"label":"Page","properties":["title"],"indexed_fields":["title"]}],"edges":[]}"#.to_vec()
    }
    fn write(databases: &Databases, graph: &str, request: &str) -> Command {
        let database = databases.get(graph).unwrap();
        let mut tx = QueryTransaction::begin(&database)
            .unwrap()
            .with_replicated_ids();
        tx.run(
            "MERGE (n:Page {entity_id:'p-1'}) SET n.title='committed' RETURN n",
            &BTreeMap::new(),
        )
        .unwrap();
        let prepared = tx.prepare_replication().unwrap().unwrap();
        Command::Write {
            request_id: request.into(),
            graph: graph.into(),
            expected_revision: prepared.expected_revision,
            batch: prepared.batch,
        }
    }
    #[test]
    fn graph_apply_replays_only_same_global_command_and_registry_ignores_orphans() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let databases = Arc::new(Databases::open_replica(&config).unwrap());
        let app = GraphApplication::open(databases.clone(), temp.path()).unwrap();
        let bootstrap = Command::Bootstrap {
            request_id: "initial".into(),
            graphs: BTreeMap::from([("alpha".into(), schema()), ("beta".into(), schema())]),
        };
        assert_eq!(app.apply(&bootstrap).unwrap(), Reply::Initialized);
        let first = write(&databases, "alpha", "first");
        assert_eq!(app.apply(&first).unwrap(), Reply::Applied { revision: 1 });
        assert_eq!(app.apply(&first).unwrap(), Reply::Applied { revision: 1 });
        let Command::Write {
            graph,
            expected_revision,
            batch,
            ..
        } = first
        else {
            unreachable!()
        };
        let stale = Command::Write {
            request_id: "different-command".into(),
            graph,
            expected_revision,
            batch,
        };
        assert_eq!(app.apply(&stale).unwrap(), Reply::Conflict);
        assert_eq!(
            app.apply(&write(&databases, "beta", "second-graph"))
                .unwrap(),
            Reply::Applied { revision: 1 }
        );
        for name in ["alpha", "beta"] {
            brainwire_store::graph::check_invariants(&databases.get(name).unwrap()).unwrap();
        }
        databases.create("orphan").unwrap();
        drop(app);
        drop(databases);
        let databases = Arc::new(Databases::open_replica(&config).unwrap());
        let _restored = GraphApplication::open(databases.clone(), temp.path()).unwrap();
        assert_eq!(databases.names(), vec!["alpha", "beta"]);
    }
    #[test]
    fn missing_acknowledged_store_is_refused_and_unpublished_creation_replays() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let databases = Arc::new(Databases::open_replica(&config).unwrap());
        let app = GraphApplication::open(databases.clone(), temp.path()).unwrap();
        let schemas = BTreeMap::from([("pages".into(), schema())]);
        // Physical initialization can finish before registry publication; a
        // committed Bootstrap replay safely adopts that unpublished directory.
        app.materialize(&schemas).unwrap();
        drop(app);
        drop(databases);
        let databases = Arc::new(Databases::open_replica(&config).unwrap());
        let app = GraphApplication::open(databases.clone(), temp.path()).unwrap();
        assert!(databases.names().is_empty());
        app.apply(&Command::Bootstrap {
            request_id: "initialize".into(),
            graphs: schemas,
        })
        .unwrap();
        app.apply(&write(&databases, "pages", "acked-write"))
            .unwrap();
        drop(app);
        drop(databases);
        std::fs::remove_dir_all(temp.path().join("pages")).unwrap();
        let databases = Arc::new(Databases::open_replica(&config).unwrap());
        assert!(GraphApplication::open(databases, temp.path()).is_err());
        assert!(!temp.path().join("pages").exists());
    }
    #[test]
    fn invalid_schema_or_batch_is_a_deterministic_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let databases = Arc::new(Databases::open_replica(&config(temp.path())).unwrap());
        let app = GraphApplication::open(databases, temp.path()).unwrap();
        assert_eq!(
            app.apply(&Command::Bootstrap {
                request_id: "bad".into(),
                graphs: BTreeMap::from([("../escape".into(), schema())])
            })
            .unwrap(),
            Reply::Conflict
        );
        assert_eq!(
            app.apply(&Command::Write {
                request_id: "bad".into(),
                graph: "absent".into(),
                expected_revision: 0,
                batch: vec![1]
            })
            .unwrap(),
            Reply::Conflict
        );
        assert!(!temp.path().join(".brainwire-graphs.json").exists());
    }
}
