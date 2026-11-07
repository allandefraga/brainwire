//! Canonical replicated commands; application rejections are deterministic replies.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, BufWriter, Cursor, Write},
};

pub type NodeId = u64;
pub const MAX_COMMAND_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
pub use openraft::BasicNode;

struct CommandSize {
    bytes: usize,
    exceeded: bool,
}

impl Write for CommandSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.bytes.checked_add(bytes.len()) {
            Some(total) if total <= MAX_COMMAND_BYTES => self.bytes = total,
            _ => {
                self.exceeded = true;
                return Err(io::Error::other("serialized command exceeds 2 MiB"));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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
pub(crate) fn validate_request_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err("request_id must be 1..128 ASCII identifier characters".into());
    }
    Ok(())
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
        validate_request_id(self.request_id())?;
        let mut size = CommandSize {
            bytes: 0,
            exceeded: false,
        };
        let serialized = serde_json::to_writer(&mut size, self);
        if size.exceeded {
            return Err("serialized command exceeds 2 MiB".into());
        }
        serialized.map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn digest(&self) -> Result<[u8; 32], serde_json::Error> {
        let mut writer = BufWriter::new(HashWriter(Sha256::new()));
        serde_json::to_writer(&mut writer, self)?;
        writer.flush().map_err(serde_json::Error::io)?;
        let hash = writer
            .into_inner()
            .map_err(|error| serde_json::Error::io(error.into_error()))?;
        Ok(hash.0.finalize().into())
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
