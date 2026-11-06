# Relationship property index: synthetic standalone test

On a local shared host, a beta.2 candidate ran 100,000 synthetic relationships
in one container limited to 3 GiB and two CPUs. Each timing is the median of
nine warm Bolt queries; it is not a production latency guarantee.

| Query | Before index | Index ready | Forced scan after index |
| --- | ---: | ---: | ---: |
| Timestamp range | 132.79 ms | 1.35 ms | 174.83 ms |
| Timestamp equality | 83.88 ms | 1.24 ms | 113.01 ms |

The test checked reads during population, writes, property changes, deletes,
restart after SIGKILL, integrity and results after dropping the index. The
previous beta refused the upgraded volume; the candidate reopened it. This is
one synthetic standalone test, not evidence of distributed index support or
performance for other query shapes. EXPLAIN and PROFILE are unavailable, so
index usage is inferred from readiness, equivalent results and timings.

[Raw receipt](2026-11-06-relationship-index.json) ·
[Index setup and format limitations](../relationship-property-indexes.md)

To repeat with locally available Docker images and the Python `neo4j` driver:

```sh
python scripts/bench_relationship_index.py --image YOUR_CANDIDATE_IMAGE \
  --older-image ghcr.io/allandefraga/brainwire:v0.16.0-beta.1 \
  --rows 100000 --samples 9 --output /tmp/brainwire-index-receipt.json
```

The optional old image tests downgrade refusal. The runner creates and removes
its own labeled containers and volume. Use a fresh output path.
