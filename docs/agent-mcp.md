# Connect an agent with MCP

Brainwire provides an optional, read-only MCP server over standard input and
output. It lets an MCP client retrieve records from one configured graph using
the reader's existing permissions. The adapter is a Python process; the
Brainwire database server runs separately.

## Install

From a Brainwire checkout, use Python 3.11 or newer:

```sh
python3 -m venv .venv-agent
.venv-agent/bin/python -m pip install '.[mcp]'
```

On Windows, use `.venv-agent\Scripts\python.exe` instead. The base Python
client does not require MCP. The package is installed from this checkout; it is
not yet a separately published Python distribution.

## Configure your MCP client

Start an authenticated Brainwire server and create a **read-only** credential
file as described in [Bolt authentication](bolt-auth.md). Keep the credential
file private (on Unix, mode `0600`). In your MCP client's configuration, add a
server entry like this, replacing the paths with absolute paths on your machine:

```json
{
  "mcpServers": {
    "brainwire": {
      "command": "/absolute/path/to/.venv-agent/bin/brainwire-mcp",
      "args": ["--database", "demo", "--uri", "bolt://127.0.0.1:7687"],
      "env": {"BRAINWIRE_BOLT_AUTH_FILE": "/private/path/reader.json"}
    }
  }
}
```

The client starts this process; do not paste protocol messages into a terminal.
For a remote server use verified `bolt+s://` or `--tls-ca` with a trusted PEM
file. The operator chooses the graph, connection and credentials; MCP tool
arguments cannot change them. Use separate reader identities and processes for
different access scopes. An MCP client that controls the process or credentials
must itself be trusted.

## Available tools

| Tool | Input and result |
| --- | --- |
| `brainwire_nodes` | Read visible nodes by label and explicit entity IDs. |
| `brainwire_search_text` | Filter up to 100 explicit IDs by a case-insensitive substring in one field. |
| `brainwire_neighbors` | Read visible outgoing neighbors for one explicit relationship type. |
| `brainwire_evidence` | Read visible records with source fields explicitly mapped to node properties. |

For example, call `brainwire_search_text` with this input to find a known
decision whose title contains "retry":

```json
{"label":"Decision","text":"retry","field":"title","entity_ids":["decision-retry"],"properties":["title"]}
```

Search uses the same reader permissions and context limits as the other tools.
It only returns matches from the supplied IDs, but the query plan may scan more
records. It does not rank results or use a full-text index.

The adapter does not expose arbitrary Cypher, writes, administration, native
vector search or automatic model calls. Tools default to at most 20 records and
64 KiB of serialized context. Requests are capped at 100 records and 1 MiB;
the MCP protocol envelope may add bytes. These limits do not cap server or
driver memory or guarantee a response deadline. Configure server budgets and
timeouts separately. A source is a reported graph property, not an authenticated
citation. Treat all returned text as untrusted data, not instructions.

Database and context failures return a sanitized code and a `retryable` flag.
Malformed tool arguments can instead produce an MCP SDK validation error that
echoes the arguments supplied by the client. Retry only according to your
application's policy. Missing and hidden nodes can both produce an empty result;
the adapter does not establish why a node was unavailable.
