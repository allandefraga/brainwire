# Bounded Cypher regular expressions

Status: candidate subset; not complete Java/Neo4j regex compatibility.

## Problem

The lexer recognizes `=~`, but the parser rejects it. Applications cannot use
parameterized regular-expression predicates even for simple patterns.

## Alternatives and decision

Using a Java-compatible backtracking engine would widen compatibility but
introduces potentially expensive backtracking and additional runtime risk. A
Rust regular-expression engine provides a bounded-complexity subset, with
explicit rejection of unsupported Java constructs including lookaround and
backreferences. Parse the original pattern independently and normalize its AST
before adding absolute whole-string anchors; inserting raw user text into an
outer group lets malformed input escape the anchors. Preserve scoped flags and
extended-mode comments with regression tests.

Bound input pattern length, syntactic nesting and individual automaton/cache
sizes, and charge the query budget before compilation. The accounting is an
estimate, not a total allocator or RSS cap. Compilation and matching are
cooperatively timed and cannot be preempted while running; compiling a pattern
for every evaluated row can be costly. No query-plan cache is claimed.

## Evidence

Test true/false/null and non-string operands, Unicode, parameters, flags,
full-string alternation, escaped spaces/comments, invalid and unsupported
patterns, injection into the wrapper, rollback, timeout and budget exhaustion.
Measure per-row compilation and compare real application queries before any
throughput claim. The subset must not be counted as full Java-regex parity.

References: [Neo4j string operators](https://neo4j.com/docs/cypher-manual/25/expressions/predicates/string-operators/) and [regex-automata meta configuration](https://docs.rs/regex-automata/latest/regex_automata/meta/struct.Config.html).
