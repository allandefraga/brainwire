"""Own a Bolt driver for the lifetime of a Brainwire client."""
from contextlib import contextmanager

from .client import GraphClient


@contextmanager
def connect(uri, database, *, auth, batch_size=250, batch_bytes=1 << 20,
            **driver_config):
    """Yield a GraphClient after checking server connectivity; always close its driver.

    Pass ``auth=None`` only for a trusted local unauthenticated demo. Credentials,
    TLS, connection timeouts and pooling use the Neo4j driver's configuration.
    This verifies connectivity, not permissions for the selected named graph.
    It does not retry writes or claim a failed commit was rolled back.
    """
    from neo4j import GraphDatabase

    with GraphDatabase.driver(uri, auth=auth, **driver_config) as driver:
        graph = GraphClient(driver, database, batch_size=batch_size,
                            batch_bytes=batch_bytes)
        driver.verify_connectivity()
        yield graph
