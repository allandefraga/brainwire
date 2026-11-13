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
