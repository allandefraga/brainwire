# Scalar indexes through Cypher

Ontology `indexed_fields` and `er_fields` declare string/Boolean equality
indexes. `entity_id` has a separate primary lookup. Node writes, overwrites,
NULL removal and deletion update index keys in the same RocksDB transaction;
commit syncs the WAL. Ready declarations have been atomically backfilled.
Legacy catalogs without the ready marker scan until reapplied.

Both forms now use a ready equality index:

```cypher
MATCH (c:Customer {email:$email}) RETURN c.entity_id
MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id
```

The executor extracts equality conjuncts on the pattern's starting node when
the other operand is a literal or parameter. Reversed equality works too.
Primary/pattern constraints remain preferred, existing bindings are retained,
and the full WHERE predicate is still checked on each candidate. One-hop
patterns can seek their starting node before expanding.

```cypher
MATCH (c:Customer)
WHERE $email=c.email AND c.segment=true
RETURN c.entity_id
```

Standalone OR/NOT, ranges, row-dependent expressions, unsupported values and
unready declarations scan. A safe equality outside an OR can still narrow an
AND: `c.segment=true AND (c.email=$a OR c.email=$b)`. Selection uses the first
usable equality; there are no selectivity statistics, intersections, ending-node
index plans or cost-based optimization yet.

Inspect actual snapshot/catalog readiness over Bolt or embedded Cypher:

```cypher
CALL db.indexes() YIELD label, property, state
RETURN label, property, state
```

`state` is `string_boolean_ready` or `declared_only`. This reads metadata rather
than scanning entries. Readiness does not imply support for every property
type: numeric/list/vector/map values still scan. The built-in primary lookup
is not listed. `db check NAME` separately audits full graph/index invariants.

The TUI's `shell --overview --view query` shows the selected graph's real
ready state, indexed fields, write path and query/CSR guidance. It remains an
explanation panel and does not execute the displayed queries.

Reproduce a warm comparison with temporary synthetic data, correctness
assertions, five warmups and 25 measured samples after reopening:

```sh
cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 10000
cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 100000
```

It compares the same unique string in indexed `email` and unindexed `name`.
Open time and network latency are excluded; temporary data are removed.
