# Use Brainwire from Python

You need Python 3.9 or newer and a Brainwire checkout. From the checkout, run:

```sh
python3 scripts/setup_python.py
```

On Windows, the Python launcher can run the same installer:

```powershell
py -3 scripts/setup_python.py
```

This creates a fresh `.venv-brainwire` environment and installs the Python
client with its Bolt driver, using a current pip inside that new environment. No activation, Rust compiler or additional package
manager is needed. Downloading dependencies may need internet access. The
installer leaves existing environments untouched; use `--venv /new/path` to
choose another location. If setup fails, read the command error and retry with
a new path after fixing the cause. Python must include `venv` and `pip`.

The database runs separately: follow [Run in Docker](container.md) first.
The client package is installed from this checkout; a public
`pip install brainwire` distribution is not available.

Save this as `example.py` and run `.venv-brainwire/bin/python example.py`:

```python
from brainwire import connect

with connect("bolt://127.0.0.1:7687", "demo", auth=None) as graph:
    graph.upsert_node("Customer", "customer-1", {"name": "Ada"})
    print(graph.run_cypher("MATCH (c:Customer) RETURN c.name AS name"))
```

Use `auth=None` only for the local synthetic demo. Shared servers need
[authentication](bolt-auth.md) and a [protected connection](protected-transport.md).
`connect` checks connectivity and closes its driver when the block ends. It does
not prove access to a named graph before the first graph operation.

On Windows, use `.venv-brainwire\Scripts\python.exe` to run your file.
Existing virtual environments can install this checkout with `python -m pip install .`.
The helper uses the Neo4j Bolt driver; it is a synchronous client.

## Next steps

- [Write, patch, remove and stream records](python-client.md).
- [Read bounded context and rerank candidates for agents](context-api.md).
- [Build a graph from Python code](code-graph.md).
- [Import application data](source-ingestion.md).

Each batch commits separately. `None` removes a property; omitted properties
stay unchanged. A lost commit response has an unknown outcome: reconcile before
retrying. Read the [error and status contract](python-client.md#typed-server-failures-and-standalone-status)
before adding retry logic.
