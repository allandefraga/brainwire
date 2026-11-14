# Import an Obsidian vault

Turn a small, local Markdown vault into a graph you can query with Cypher.
Each note becomes a `Note` with its vault-relative path as `entity_id`,
filename stem as `title`, and Markdown as `body`. A resolved `[[wikilink]]`
creates a directed `LINKS_TO` relationship. The importer runs locally and
only sends the prepared rows to the Brainwire endpoint you select.

On Linux or macOS, preview the included synthetic vault from the repository
root. The preview prints counts and opens no Bolt connection.

```sh
python3 -m examples.obsidian.import_vault examples/obsidian/vault
# {"notes": 2, "links": 2, "written": false}
```

Install the [Python client](../../docs/python-client.md), then create a new
database with the included ontology while the server is stopped.

```sh
brainwire db create vault_demo --ontology examples/obsidian/ontology.json
brainwire serve
```

In another terminal, import the sample into that dedicated, empty database.
For a protected server, set `BRAINWIRE_BOLT_AUTH_FILE` as described in the
[Python client guide](../../docs/python-client.md) and use an unrestricted
writer for this graph.

```sh
.venv-brainwire/bin/python -m examples.obsidian.import_vault \
  examples/obsidian/vault --database vault_demo --uri bolt://127.0.0.1:7687
```

Query the note relationships with a Bolt client.

```cypher
MATCH (a:Note)-[:LINKS_TO]->(b:Note)
RETURN a.entity_id AS source, b.entity_id AS target
ORDER BY source
```

| source | target |
| --- | --- |
| Atlas launch.md | Retry policy.md |
| Retry policy.md | Atlas launch.md |

## Run with Docker

Set `BRAINWIRE_IMAGE` to the published tag from the [quickstart](../../README.md#quickstart)
and install the [Python client](../../docs/python-client.md) on the host. From
the repository root, create the example graph in a separate volume and start
it on an automatically assigned loopback port.

```sh
BW_VAULT_ID="bw-vault-$(date +%s)-$$"
docker volume create "$BW_VAULT_ID"
docker run --rm -i --mount "source=$BW_VAULT_ID,target=/data" \
  --entrypoint sh "$BRAINWIRE_IMAGE" -ec \
  'cat >/tmp/ontology.json; brainwire db create vault_demo --ontology /tmp/ontology.json' \
  < examples/obsidian/ontology.json
docker run -d --name "$BW_VAULT_ID" --publish 127.0.0.1::7687 \
  --mount "source=$BW_VAULT_ID,target=/data" \
  -e BRAINWIRE_SERVER_DEFAULT_DATABASE=vault_demo \
  "$BRAINWIRE_IMAGE" serve --allow-unauthenticated-remote
docker exec "$BW_VAULT_ID" brainwire probe --mode ready
```

Retry the readiness check if startup is still in progress. The container demo
allows unauthenticated access and is intended for a trusted host and network.

```sh
BW_VAULT_ADDRESS="$(docker port "$BW_VAULT_ID" 7687/tcp | head -1)"
env -u BRAINWIRE_BOLT_AUTH_FILE .venv-brainwire/bin/python -m examples.obsidian.import_vault \
  examples/obsidian/vault --database vault_demo --uri "bolt://$BW_VAULT_ADDRESS"
```

To stop the demo and delete its example data, remove only the container and
volume created above.

```sh
docker rm -f "$BW_VAULT_ID"
docker volume rm "$BW_VAULT_ID"
```

## Supported vaults

The example supports case-sensitive links by an unambiguous filename or
exact vault-relative path. `.md`, heading fragments and aliases are optional.
Basic fenced and same-line inline code, escaped links, embeds, heading-only
links, and hidden entries are skipped. Missing and ambiguous note links stop
the preview or import. The parser handles this subset of Markdown and does
not implement all of Obsidian's link-resolution rules.

The scanner accepts up to 128 UTF-8 notes, 32 KiB per note, 256 KiB of total
Markdown, and 512 wikilink references. Use a physical path without symlinks
and keep the vault unchanged during the scan. Visible symlinks and Markdown
hard links are rejected. Vault access permissions do not become graph ACLs.
Use only a vault you are authorized to copy to the chosen graph.

The example targets one initial load on a standalone server. It checks graph
emptiness, creates the notes and relationships in one explicit transaction
and refuses to rerun against an occupied graph. Use a dedicated graph with
no other writers. The emptiness check does not lock out inserts with other
IDs. If the connection drops during commit, inspect the graph before retrying.
For a changed vault, import a new snapshot into another empty graph.
