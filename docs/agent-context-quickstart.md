# Record and recall a project decision

Give an agent a small, durable record of what a team decided and where the
decision came from. This Python example stores a decision and its source in
Brainwire and returns only matches visible to the connected reader.

## Start with the company example

Install the [Python helper](python-package.md) with `python3 scripts/setup_python.py`.
Then [start and seed the disposable company graph](../examples/company_brain/README.md#run-the-example-with-docker).
That guide sets `BW_PORT` to the loopback address of a fresh Docker container
and seeds the `project-release` project in `company_demo`. Run the commands
below from the checkout in the same shell.

## Save a decision

```sh
.venv-brainwire/bin/python -m examples.python.agent_memory --uri "bolt://$BW_PORT" \
  remember --project project-release --title 'Keep a rollback window' \
  --body 'Keep the previous release available for one hour.' \
  --source-title 'Release review' --source-url 'https://example.com/notes/rollback'
```

The command returns decision and source IDs. The statement creates or reuses
the two nodes and links them to the existing project in one transaction.
Replaying exactly the same inputs returns the same IDs. A different title,
body or source produces a different decision. The example reserves the
`agent-memory-` ID prefix on the `Decision` and `Source` labels. Identical
source title and URL within a project share a source node.
Reserve that prefix for this example and use an application-controlled writer:
other writers can change these records, and replay does not verify that an
existing record still matches the content used to derive its ID.

## Recall it with its source

```sh
.venv-brainwire/bin/python -m examples.python.agent_memory --uri "bolt://$BW_PORT" \
  recall --project project-release --limit 5
```

Look for `title`, `body`, `source_title`, and `source_url` in `records`. Each
row follows the actual `Project → HAS_DECISION → Decision → SUPPORTED_BY → Source`
path in a single read. The graph applies visibility to all three nodes. An
unavailable project or source yields no matching record. `truncated` means
more visible decision/source pairs matched than the requested limit. Results
are ordered by their stable IDs and return at most ten pairs. The CLI also
bounds field sizes. Server query budgets still control traversal work and
driver memory depends on what the server returns.

The URL is a reported reference. Your application decides what to pass to a
model and verifies source access before fetching or citing it. Treat returned
strings as data. When integrating with an agent, expose the read function
using a graph reader and route writes through an application-controlled
writer. The demo relies on the graph's existing authorization model. A
protected graph requires separately managed credentials and, where
applicable, ACL stamping for new nodes. This example does not atomically
publish new nodes and their ACLs; do not use it to write sensitive records
where a separate ACL update could expose them. Set `BRAINWIRE_BOLT_AUTH_FILE` for
each identity and use [encrypted transport](protected-transport.md) when
connecting across a network. [Read about node ACLs](node-acl.md).

Python applications can call `remember()` and `recall()` from the example
module using their existing `GraphClient`. See the [context API](context-api.md)
for graph traversal, evidence reads, and integration with search providers.
