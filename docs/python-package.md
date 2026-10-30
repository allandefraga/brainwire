# Install the Python helper locally

From a checkout of the desired Brainwire tag:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install .
```

This installs the `brainwire` Python package and the Neo4j Bolt driver. It does
not install or start the database server. This is a repository-local package;
no public PyPI publication or ownership of the PyPI name is claimed. Do not
substitute `pip install brainwire` until an official distribution is published.

```python
from brainwire import GraphClient
from neo4j import GraphDatabase

# Local synthetic demo. Shared/remote servers need authenticated access
# and a verified encrypted boundary described in the deployment guides.
with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    graph = GraphClient(driver, "demo", batch_size=100)
    graph.upsert_node("Customer", "customer-1", {"name": "Ada"})
```

The caller creates and closes the driver, chooses credentials and the named
graph, and handles transport errors. This package wraps the existing synchronous
helper; it is not an independent Bolt implementation or an async client.
Supported driver versions are Neo4j Python 5.28 through 6.x. Compatibility with
every minor driver release is not implied by that dependency range.

Every bounded batch commits separately. `None` removes a property; omitted
properties remain unchanged. A lost commit acknowledgment is an unknown outcome:
the helper does not blindly replay the callback. Reconcile stable source keys
before retrying. Explicit retained request IDs require the experimental quorum path or the
opt-in authenticated [standalone request ledger](standalone-requests.md). See
[the helper contract](python-client.md) for all mutation and security limits.

Known server memory refusals and unknown-commit codes raise
`MemoryPressureError` and `CommitOutcomeUnknownError`, preserving the driver
error as their cause. Other errors keep their original type. The optional
`request_status(seed)` helper uses the standalone experimental endpoint in a
fresh autocommit session; an absent receipt never means a safe retry. See the
[error and status contract](python-client.md#typed-server-failures-and-standalone-status).

For example, Python release version `0.10.0b1` corresponds to Rust/tag
`0.10.0-beta.1` under PEP 440. The release helper updates both versions together. Existing
`examples.python.client` imports re-export the same implementation.
