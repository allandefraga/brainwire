# Validate a typed ingestion batch

Use a small JSON contract to validate node and relationship writes before sending them over Bolt. Preview the synthetic batch locally from the repository root.

```sh
python -m examples.typed_ingest.ingest examples/typed_ingest/schema.json examples/typed_ingest/batch.json
```

To write the sample, start a local Brainwire server with the bundled ontology and the Python Bolt dependency installed. Use a disposable directory and graph.

```sh
export BRAINWIRE_SERVER_DATA_DIR="$(mktemp -d)"
export BRAINWIRE_SERVER_LISTEN=127.0.0.1:17687
export BRAINWIRE_SERVER_DEFAULT_DATABASE=typed_demo
brainwire db create typed_demo --ontology examples/typed_ingest/ontology.json
brainwire serve
```

In another terminal at the repository root, run:

```sh
python -m examples.typed_ingest.ingest examples/typed_ingest/schema.json examples/typed_ingest/batch.json \
  --database typed_demo --uri bolt://127.0.0.1:17687
```

The contract declares one signature for each directed relationship type. It accepts these scalar types as written: JSON booleans, signed 64-bit integers, finite floating-point numbers, and UTF-8 strings. Write `1.0` for a float. Set `required: true` to require a property in every row. Set `nullable: true` to accept JSON `null`, which removes the property on write. Omitting an optional property preserves its prior value. Extra properties and duplicate identities in a batch are rejected. Node identity uses label and `entity_id`. The example supports one relationship per type and endpoint pair.

The example caps each file and the validated JSON payload at 256 KiB, the batch at 256 total rows, the schema at 64 total types, and each record type at 32 properties. Strings and identities have separate UTF-8 byte limits. A failed validation opens no Bolt session. A valid batch uses one explicit transaction and checks every group count before committing. If a relationship endpoint is missing, the transaction closes without committing. After a connection failure during commit, inspect the graph before retrying.

The batch contract runs in Python. Maintain it alongside the graph ontology. An independent writer can bypass its required-field and extra-property checks, and omitted properties already stored in the graph are not revalidated. The supplied ontology also declares [native scalar types](../../docs/typed-properties.md) on a new standalone graph; the engine enforces those types for independent writers. This utility does not install constraints or compare its contract against an existing graph, and does not publish nodes and their ACLs together. For writes that require ACLs, use the client's `load_nodes(..., with_acl=True)` workflow instead. The CLI connects only to numeric loopback endpoints. For a protected local server, set `BRAINWIRE_BOLT_AUTH_FILE` to a valid credentials file.
