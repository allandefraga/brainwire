# Typed properties on a new standalone graph

Use `property_types` in the ontology to enforce scalar types on individual
node and relationship properties. Include each typed field in `properties`.
For example, save this ontology as `decisions.json`:

```json
{
  "version": "1",
  "nodes": [
    {"label": "Project", "properties": ["priority"], "property_types": {"priority": "integer"}},
    {"label": "Decision", "properties": ["approved"], "property_types": {"approved": "boolean"}}
  ],
  "edges": [
    {
      "type": "HAS_DECISION",
      "from": {"label": "Project"},
      "to": {"label": "Decision"},
      "properties": ["confidence"],
      "property_types": {"confidence": "float"}
    }
  ]
}
```

From a stopped standalone instance, create a database with a new name:

```sh
brainwire db create decisions --ontology decisions.json
brainwire db schema decisions --json
```

The engine accepts `boolean`, `integer`, finite `float`, and `string`. It
checks the result of a property update without coercion, so `1` is an integer
and `1.0` is a float. Missing properties are allowed, and `NULL` removes a
property. A mismatched value fails the write with a constraint error, and
the transaction can be rolled back. Properties without a declaration retain
the existing behavior. A declared type on a relationship merge key must be
`string`.

`CALL db.schema()` includes `propertyTypes` for node types and relationship
signatures that have declared scalar types. Applying the same declarations
again is allowed. A partial ontology or ordinary catalog update preserves
the existing types. Changing, adding, or dropping a type rule requires a new
graph and reingestion. `db check` also checks stored values against the rules.

Creation supports Linux and macOS with a filesystem that supports atomic
no-replace directory rename. A dedicated RocksDB column family causes older
Brainwire binaries to refuse the volume, including its physical checkpoints.
Keep a compatible backup and do not open a typed checkpoint on an older
server. Use the [checkpoint guide](checkpoints.md) to copy and verify a backup.
In a [local Linux arm64 container diagnostic](benchmarks/2026-11-15-typed-format-linux.json),
the previous published image could read an ordinary graph but refused the new
format, its checkpoint and a restored copy. The candidate reopened and
validated the restored graph. This test used synthetic data and a normal
restart; it does not qualify a power loss or distributed failover.
Typed graphs require standalone storage. Raft and the experimental request
ledger cannot open them. This feature covers scalar types. It has no required
properties, coercion, enum or vector contracts.
