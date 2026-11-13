# Company context example

A small synthetic graph links a team, project, decision and source. The example
queries decisions linked to the project, narrows a fixture provider's ranked
IDs to that scope, reads authorized records and their reported source URL, and
traces a stored source link. It is a starting point for agent context.

From a checkout with the Brainwire CLI and Python helper installed, create a
**new disposable graph while the server is stopped**:

```sh
brainwire db create company_demo --ontology examples/company_brain/ontology.json
brainwire serve
```

In a second terminal, from the checkout:

```sh
.venv-brainwire/bin/python -m examples.company_brain.demo --seed
```

`--seed` upserts fixed synthetic IDs and overwrites their properties. Use only
a fresh evaluation graph. Without `--seed`, the example only reads; select a
specific Bolt endpoint or graph with `--uri` and `--database`. Configure
credentials as described in the [Python guide](../../docs/python-client.md)
when the server requires them.

The fixture provider returns two predefined IDs and performs no semantic or
keyword search. Replace it with an application-managed provider. Scope and
context reads use separate snapshots, and the scope does not replace ACLs.
The sample scope is unordered and limited to 100 records; the output flags a
possibly truncated scope when that limit is reached.
Source URLs are reported by graph records; they are not verified citations.
Brainwire does not implement graph branching or historical revisions here.

## Run the example with Docker

From the repository root, set `BRAINWIRE_IMAGE` to the versioned image used in
the [main quickstart](../../README.md#quickstart), then create a separate volume
and container for this example. The host port is assigned automatically.

```sh
BW_DEMO_ID="bw-company-$(date +%s)-$$"
docker volume create "$BW_DEMO_ID"
docker run --rm \
  --mount "type=volume,source=$BW_DEMO_ID,target=/data" \
  --mount "type=bind,source=$PWD/examples/company_brain/ontology.json,target=/tmp/ontology.json,readonly" \
  "$BRAINWIRE_IMAGE" db create company_demo --ontology /tmp/ontology.json

docker run -d --name "$BW_DEMO_ID" --publish 127.0.0.1::7687 \
  --mount "type=volume,source=$BW_DEMO_ID,target=/data" \
  "$BRAINWIRE_IMAGE" serve --allow-unauthenticated-remote
```

Wait for readiness, then run the Python example from the same shell:

```sh
docker exec "$BW_DEMO_ID" brainwire probe --mode ready
BW_PORT="$(docker port "$BW_DEMO_ID" 7687/tcp | head -1)"
.venv-brainwire/bin/python -m examples.company_brain.demo \
  --uri "bolt://$BW_PORT" --seed
```

If the readiness check reports that startup is still in progress, retry it.
This example allows unauthenticated access inside the container and binds its
published port to host loopback. Use [protected deployment](../../docs/protected-containers.md)
for shared environments.

To remove the example container and **delete its example volume** afterward:

```sh
docker rm -f "$BW_DEMO_ID"
docker volume rm "$BW_DEMO_ID"
```
