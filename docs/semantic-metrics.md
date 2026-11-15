# Named metrics over a graph

This Python example gives an agent or application a small set of named,
read-only metrics. It is inspired by semantic-layer tools: define the meaning
once, then ask for a metric by name instead of assembling a query from user
input. It is an example built on Brainwire's existing Bolt client.

Start with the [company graph](agent-context-quickstart.md), then inspect the
sample definitions without connecting:

```sh
.venv-brainwire/bin/python -m examples.semantic_metrics.metrics describe
```

With the company graph and its reader credential configured as described in the
quickstart, query a metric:

```sh
.venv-brainwire/bin/python -m examples.semantic_metrics.metrics query decisions \
  --uri "bolt://$BW_PORT" --group-by title
```

The sample catalog lives in [catalog.json](../examples/semantic_metrics/catalog.json).
Each metric names a node label, `count`, `sum` or `avg`, and optional scalar
dimensions. `count` counts visible nodes; `sum` and `avg` require a declared
property and reject incompatible values. Grouping accepts one declared field.
Use `--catalog` before `query` to load another JSON catalog, and `--tls-ca` after
`query` to verify a custom CA. The reader's graph and node permissions apply.

Results include the catalog version observed during validation. Schema
validation and the aggregate are separate reads, so this is not a guarantee
that both used the same snapshot. File `version: 1` identifies the catalog
format, not a versioned history of metric definitions. Dimension types in the
metric catalog check returned group values; they do not themselves constrain
writes. A new standalone graph may separately declare and enforce scalar types
in its [ontology](typed-properties.md).
Numeric averages and mixed numeric sums use floating-point arithmetic.

At most 50 groups are returned; exceeding the configured group limit raises an
error rather than returning a partial metric. The limit does not cap the work or
memory needed by the server to scan and group records. Configure query budgets
and timeouts for the workload. No automatic joins, time grains, derived or
cumulative metrics, native indexes, or MetricFlow compatibility are provided.
Do not treat a user-supplied catalog as a trusted authorization policy.
