//! Canonical replicated commands; application rejections are deterministic replies.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Cursor};

pub type NodeId = u64;
pub const MAX_COMMAND_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
pub use openraft::BasicNode;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Bootstrap {
        request_id: String,
        graphs: BTreeMap<String, Vec<u8>>,
    },
    CreateGraph {
        request_id: String,
        graph: String,
        schema: Vec<u8>,
    },
    Write {
        request_id: String,
        graph: String,
        expected_revision: u64,
        batch: Vec<u8>,
    },
}
// Consensus tracing must not expand multi-megabyte graph payloads.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bootstrap { request_id, graphs } => f
                .debug_struct("Bootstrap")
                .field("request_id", request_id)
                .field("graph_count", &graphs.len())
                .finish(),
            Self::CreateGraph {
                request_id,
                graph,
                schema,
            } => f
                .debug_struct("CreateGraph")
                .field("request_id", request_id)
                .field("graph", graph)
                .field("schema_bytes", &schema.len())
                .finish(),
            Self::Write {
                request_id,
                graph,
                expected_revision,
                batch,
            } => f
                .debug_struct("Write")
                .field("request_id", request_id)
                .field("graph", graph)
                .field("expected_revision", expected_revision)
                .field("batch_bytes", &batch.len())
                .finish(),
        }
    }
}
impl Command {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Bootstrap { request_id, .. }
            | Self::CreateGraph { request_id, .. }
            | Self::Write { request_id, .. } => request_id,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let id = self.request_id();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err("request_id must be 1..128 ASCII identifier characters".into());
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_COMMAND_BYTES {
            return Err("serialized command exceeds 2 MiB".into());
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<[u8; 32], serde_json::Error> {
        Ok(Sha256::digest(serde_json::to_vec(self)?).into())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Applied { revision: u64 },
    Conflict,
    Initialized,
}

/// Only committed state-machine calls may invoke this trusted blocking adapter.
/// Err means a physical/unexpected failure; ordinary command rejection is Conflict.
pub trait Application: Send + Sync + 'static {
    fn apply(&self, command: &Command) -> Result<Reply, String>;
}

openraft::declare_raft_types!(pub TypeConfig: D = Command, R = Reply, NodeId = NodeId);
pub type Raft = openraft::Raft<TypeConfig>;
