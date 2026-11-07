"""Python convenience API for Brainwire using the Neo4j Bolt driver."""

from .connection import connect
from .context import ContextClient, ContextLimitError
from .rerank import rerank_context
from .search import ContextProviderError, search_context
from .semantic import schema_paths
from .paths import trace_paths

from .client import (
    BrainwireError,
    CommitOutcomeUnknownError,
    GraphClient,
    MemoryPressureError,
    request_key,
)

__all__ = [
    "GraphClient", "connect", "ContextClient", "ContextLimitError", "ContextProviderError", "rerank_context", "search_context", "schema_paths", "trace_paths", "request_key", "BrainwireError", "MemoryPressureError",
    "CommitOutcomeUnknownError",
]
