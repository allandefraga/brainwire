# Scalar indexes through Cypher

Ontology `indexed_fields` and `er_fields` declare string, Boolean and finite numeric equality
indexes. `entity_id` has a separate primary lookup. Node writes, overwrites,
NULL removal and deletion update index keys in the same RocksDB transaction;
commit syncs the WAL. Ready declarations have been atomically backfilled.
Legacy catalogs without the v2 ready marker scan until reapplied.

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

`state` is `scalar_equality_ready` or `declared_only`. This reads metadata rather
than scanning entries. Readiness does not imply support for every property
type: list/vector/map and nonfinite numeric values still scan. The built-in primary lookup
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

Append `number` to compare the same unique integer in indexed `email` and
unindexed `name`; the field names are synthetic. Both runs use an explicit
256 MiB allocation guard.
Open time and network latency are excluded; temporary data are removed.

## Numeric encoding and migration

Integers preserve all 64 bits. Integral floats within the signed 64-bit range
use the same key as their equal integer, including `-0.0` and `0`. Other finite
floats use their exact bit representation. This agrees with Cypher equality
at the 2^53 precision boundary and the signed 64-bit endpoints; no lossy cast
of every integer to float is used. These are equality indexes, not range indexes.

Older string/Boolean-only stores have a v1 marker. The new engine ignores that
marker and scans all declared fields until `db apply-ontology NAME SCHEMA.json`
rebuilds the active indexes atomically and publishes the v2 marker. Reapply the
same appropriate ontology, not the sample schema. The existing 100,000-key-change
cap counts both deletions and insertions: a large migration can exceed it and
fail without changing the catalog or publishing readiness. Resumable builds
remain pending. Do not downgrade a v2 store to an old binary; that binary cannot
maintain numeric entries correctly.

The migration regression uses 50,001 existing v1 string entries: deleting and
reinserting them exceeds the 100,000-change cap. The failed application must
leave the old catalog, readiness marker and every index entry unchanged; scans
still return all nodes. A malformed v2 marker also cannot enable a partial index.
