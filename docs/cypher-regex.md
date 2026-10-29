# Cypher regular-expression predicate candidate

`text =~ pattern` matches the entire string. Null or non-string operands
produce null, consistent with the existing Cypher string predicates. The
implementation supports the Rust regex syntax subset, including common
`(?i)`, `(?m)`, `(?s)` and extended-mode flags. It does not implement Java
look-around or backreferences; invalid, unsupported and oversized patterns
return a semantic client error and roll back staged writes. Unicode case
folding and regex syntax can differ from Neo4j's Java dialect.

The original expression is parsed as a bounded regex-syntax AST before
normalization and absolute anchoring. This prevents unmatched group delimiters
or extended-mode trailing comments from changing whole-string matching.

Pattern source is limited to 4,096 UTF-8 bytes and syntax nesting to 64. Meta
regex configuration sets a 64 KiB limit per NFA, a 64 KiB one-pass size limit
and an approximate 64 KiB lazy-DFA cache capacity; full DFA and bounded
backtracking engines are disabled. Engine fallback remains enabled. These are
component controls, not a complete allocation or RSS limit. Compilation/search
carry a conservative 1 MiB query-memory admission estimate. Deadline checks
surround compilation and matching and remain cooperative during native calls.

A cache retains at most the first compiled pattern within one RUN/worker,
with its estimate, key and metadata charged before compilation. Repeated use
of that pattern compiles once. A different pattern drops the native cache and
returns its exact retained credit before compilation, then disables refill
for the rest of that RUN. Varying-pattern queries therefore still compile
per row and can be expensive. New RUNs and workers start with empty caches;
reservation high-water accounting follows the existing query-pool contract.

Verification: 51 Cypher library tests and 12 dedicated integration tests
passed; one opt-in diagnostic is ignored during ordinary tests. All-targets
Clippy with warnings denied and formatting passed. Tests cover malformed
wrapper escapes, extended comments, Unicode, null/types, scope/pool admission,
rollback, first-pattern compilation count, worker isolation and RUN lifetime.

The developer-build synthetic diagnostic used the same constant predicate
for three samples at 1,000 and 10,000 UNWIND rows, comparing regex with a simple
STARTS WITH reference. Pre-cache regex median was 256.594/2,620.415 ms; cached
regex was 2.141/17.211 ms. STARTS WITH reference was 1.221/11.883 ms before and
1.178/11.460 ms afterward. The host was on AC with low-power mode disabled;
background load and code versions differed between diagnostics. Three samples
are insufficient for a meaningful p99 or release performance guarantee. This
shows the cost of recompilation in this path, not general graph throughput.

Reproduce the cached diagnostic after compiling, with no other builds or tests
running:

```sh
cargo test --locked --offline -p brainwire-cypher --test regex_match \
  regex_recompile_diagnostic -- --ignored --nocapture
```

See the [uncached](benchmarks/2026-10-29-regex/uncached.log) and
[cached](benchmarks/2026-10-29-regex/cached.log) receipts. These are narrow
synthetic measurements of a single repeated pattern and not an audited
performance comparison.
