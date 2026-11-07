# Run your first graph

This guide uses a local synthetic graph called `demo`. You need Docker with
Compose and a checkout of the Brainwire release you want to run. Choose a
published version from [GitHub Releases](https://github.com/allandefraga/brainwire/releases),
then use the matching source tag and image. The [README quickstart](../README.md#quickstart)
contains the current release commands.

The local demo disables authentication and publishes Bolt only on host
loopback. Use [protected containers](protected-containers.md) and an
[encrypted connection boundary](protected-transport.md) before connecting
shared or real data.

## 1. Start the database

From the matching checkout, set `BRAINWIRE_IMAGE` to that release's image as
shown in the README, then run:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --no-build --pull always --wait --wait-timeout 900
```

Compose creates the `demo` graph once and starts the server. Its named volume
keeps the graph across container restarts. Public images need no Docker login.
If port 7687 is busy, set `BRAINWIRE_BIND_PORT=17687` before starting and use
that port when connecting.

Check that the database is ready:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec -T brainwire brainwire probe --mode ready
```

Your connection is **`bolt://127.0.0.1:7687`**, database **`demo`**. For startup
errors, run `docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs --tail 100`.
See [container troubleshooting](container.md) for image and port problems.

## 2. Install the Python helper

With Python 3.9 or newer, run this from the checkout:

```sh
python3 scripts/setup_python.py
```

It creates a new `.venv-brainwire` environment and installs the local helper and
its Bolt driver. It needs Python's `venv` and `pip`, plus internet access to
download dependencies. Use `--venv /new/path` if that environment already exists.
The helper is installed from source; a public `pip install brainwire` package
is not available. See [Python installation](python-package.md) for Windows.

## 3. Write connected data

Save this as `first_graph.py` and run `.venv-brainwire/bin/python first_graph.py`:

```python
from brainwire import connect

with connect("bolt://127.0.0.1:7687", "demo", auth=None) as graph:
    graph.upsert_node("Customer", "customer-1", {"name": "Ada"})
    graph.upsert_node("Segment", "segment-1", {"name": "beta"})
    graph.upsert_edge(
        ("Customer", "customer-1"), "IN_SEGMENT", ("Segment", "segment-1")
    )
    rows = graph.run_cypher("""
        MATCH (c:Customer)-[:IN_SEGMENT]->(s:Segment)
        RETURN c.name AS customer, s.name AS segment
    """)
    print(rows)
```

Expected result:

```text
[{'customer': 'Ada', 'segment': 'beta'}]
```

The demo ontology declares these types and relationship. For your own data,
[define its catalog](online-catalog.md) and use stable `entity_id` values.
You can add properties, remove properties, update edges and load bounded
batches through the [Python operations](python-client.md).

Each helper operation or batch has its own transaction. A lost commit response
can have an unknown outcome: reconcile it before retrying a write. A stable
identity helps identify a record but does not make every replay safe.

## 4. Inspect the graph in your terminal

Open another terminal:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --node-label Customer --entity-id customer-1 \
  --edge-type IN_SEGMENT --target-label Segment
```

Use `Tab` or `1`–`4` to move through Ontology, Memory, Indexes and Graph.
Press `?` for help, `r` to refresh and `q` to leave. Add `--lang pt-BR` for
Portuguese. This workspace reads the running server; it does not edit data.
The graph view samples one configured outgoing relationship signature.
See [the workspace guide](connected-status.md) for controls and boundaries.

## 5. Keep or stop the data

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml restart brainwire
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

`down` removes the containers and keeps the named volume. Starting again reopens
the graph. Adding `--volumes` deletes the demo's data. For anything you need to
keep, create and verify an independent [checkpoint](checkpoints.md).

## Where to go next

- [Architecture](architecture.md): clients, queries, storage and recovery.
- [Cypher](local-cypher.md): supported clauses, functions and examples.
- [Agent context](context-api.md): bounded evidence reads and candidate reranking.
- [Explicit timelines](temporal-data.md): events, cursors and current history limits.
- [Protected deployment](protected-containers.md): credentials and application access.
