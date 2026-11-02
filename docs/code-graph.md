# Build a graph from Python syntax

Brainwire can extract Python definitions and imports locally, without importing
or executing the source. The extractor records where each relationship came
from: logical source path, start/end line and SHA-256 of the source text.

Extract one source string or a bounded local Python directory. Neither path
executes source, sends code to a model, resolves runtime imports or infers a call
graph by default. The optional static-call mode below emits conservative,
explicitly inferred candidates. Your application chooses which source may be read
and stored.

## Map a directory from the command line

After installing the Python helper, inspect an approved source directory with no
running database or model:

```sh
python -m brainwire.code_graph ./approved-source --namespace demo
```

The default prints a small JSON summary with file/entity/relationship counts,
manifest hash and `EXTRACTED` evidence. To inspect or redirect the full artifact:

```sh
python -m brainwire.code_graph ./approved-source --namespace demo --json > code-graph.json
```

To inspect the most connected syntax entities:

```sh
python -m brainwire.code_graph ./approved-source --namespace demo --top-degree 10
```

This adds at most 20 entities to the summary, ranked by descending total directed
degree and then stable entity ID. The helper `degree_summary(artifact, limit=10)`
provides the same operation in Python. Each distinct `(source, relationship,
target)` contributes one outgoing edge to its source and one incoming edge to its
target; duplicate edges are counted once, different relationship types separately,
and self-loops contribute to both directions. Names/paths in this bounded preview
may be shortened with an ellipsis. By default, degree counts syntactic definition/import
links only: a large file may rank first simply because it declares many symbols.
It is not semantic importance, relevance, influence or model confidence.
`--top-degree` and `--json` are separate, mutually exclusive output modes.

`--json` explicitly emits the full bounded artifact, including logical paths and
symbol names. `--max-files` and `--max-entities` lower the directory bounds.
Extraction errors exit with status 2 and no artifact on stdout; syntax errors do
not print source excerpts. The command does not connect to a database, execute
source, write graph records or automatically choose an output filename. Shell
redirection is managed by your shell.

## Declare the ontology

Generate the catalog and create a dedicated graph with the normal CLI:

```sh
python -c 'import json; from brainwire.code_graph import ontology; print(json.dumps(ontology()))' > code-ontology.json
brainwire db create code --ontology code-ontology.json --config brainwire.toml
```

For a running server, use the existing supported catalog-management path rather
than opening its data files from a second process. Graph creation and access
must follow the deployment's authentication and operational configuration.

The catalog has one `CodeEntity` type and `DEFINES` / `IMPORTS` relationships,
plus `LOCAL_MODULE_CANDIDATE` for the opt-in local-module feature below and
`HAS_CALL` / `STATIC_CALL_CANDIDATE` for the optional static-call slice.
Default entity kinds are `file`, `class`, `function` (including async) and
`import`; static-call mode additionally creates `call` occurrences.
An import entity is a syntactic occurrence with its module/name spelling and
optional alias. It is not proof that a module exists or was executed.

## Extract and load

Install the [Python helper](python-package.md). Then:

```python
from pathlib import Path
from brainwire import connect
from brainwire.code_graph import extract_python, load_python

# Read only a source file approved by your application's access policy.
source = Path("app.py").read_text(encoding="utf-8")
artifact = extract_python(source, source_path="src/app.py", namespace="demo")
print(artifact["edges"])  # JSON-ready rows with source evidence

with connect("bolt://127.0.0.1:7687", "code", auth=service_credentials) as graph:
    receipt = load_python(
        graph, source, source_path="src/app.py", namespace="demo"
    )
    print(receipt)
```

`source_path` is a normalized relative POSIX identity, not a path opened by the
extractor. Absolute paths, traversal components, backslashes and control
characters are rejected. Reading the source in the example remains the caller's
responsibility; perform any filesystem/symlink policy checks before reading it.

Query the evidence:

```cypher
MATCH (a:CodeEntity)-[r:DEFINES]->(b:CodeEntity)
RETURN a.qualified_name AS parent, b.qualified_name AS definition,
       r.source_path AS file, r.start_line AS line,
       r.source_hash AS sourceHash, r.evidence AS evidence
```

`EXTRACTED` means a definition or import is present in Python syntax. Conditional
and nested declarations are recorded even when they would never execute.
Relationships do not establish runtime behavior, successful imports or trust in
source text. Default extraction produces no inferred relationships or calibrated
confidence. The opt-in directory mode below labels its path-based candidates
as inferred.
Only names and source coordinates are stored; code bodies and docstrings are
not included in the generated properties.

## Import a local Python directory

On Linux and macOS, descriptor-relative traversal reads regular `.py` files
under a chosen root without following symlinks:

```python
from brainwire.code_graph import extract_python_directory, load_python_directory

artifact = extract_python_directory("./approved-source", namespace="demo")
with connect("bolt://127.0.0.1:7687", "code", auth=service_credentials) as graph:
    receipt = load_python_directory(
        graph, "./approved-source", namespace="demo", max_files=100,
        max_entities=10000,
    )
```

The directory reader holds directory descriptors, opens descendants relative to
them with no-follow flags, and validates regular files before reading. Descendant
symlinks, including a file replaced by a symlink between scanning and opening,
fail extraction. Choose a trusted root path: its ancestors are resolved by the
OS. No-follow applies to the final root path component only; spelling a root as
`link/` or `link/.` can make a symlink an ancestor and resolve its target. Passing
`link` directly as the final component rejects that symlink. Ordinary source
changes may still happen concurrently; traversal is not an atomic filesystem
snapshot. Use an immutable checkout when reproducibility matters. This is not a filesystem sandbox for
arbitrary untrusted native programs or a protection against privileged host
modification.

Fixed exclusions are `.git`, `.venv`, `venv`, `__pycache__`, `node_modules`,
`target`, `build` and `dist`. Excluded names are neither traversed nor inspected;
a symlink under an excluded name is ignored. Other symlinks cause failure, even
when they do not end in `.py`. Only lowercase `.py` regular files are parsed;
source must be UTF-8. No include hooks or source plugins are executed.

Directory extraction permits at most 100 files (configurable downward), 10,000
scanned directory entries, 16 directory levels, 8 MiB total source bytes and
10,000 total extracted entities (configurable downward). The final artifact is
also capped at 8 MiB. Per-file bounds below still apply. All extraction and
validation completes before the first graph write, so a later invalid source
file does not leave an earlier file partially imported. Write batches themselves
remain independently committed once loading begins.

The directory artifact includes relative `files` with per-file hashes. Its
`source_hash` is a deterministic manifest hash, not a graph transaction snapshot.
Individual file identities continue to use each file's content hash: unchanged
files replay into the same nodes, changed files create separate nodes, and no
old snapshot is deleted. Edges stay inside the same file snapshot and namespace;
default extraction does not infer cross-file connections. Empty source trees
produce no writes. This directory helper requires POSIX descriptor/no-follow support and
fails before reading on unsupported platforms; single-string extraction remains
portable.

## Infer limited local-module candidates

For an immutable directory checkout, explicitly enable path-based import
candidates:

```sh
python -m brainwire.code_graph ./approved-source --namespace demo --resolve-imports --json > code-imports.json
```

```python
receipt = load_python_directory(
    graph, "./approved-source", namespace="demo", resolve_imports=True,
)
generation = receipt["generation_id"]
```

Apply the current `ontology()` catalog before loading this mode into an existing
graph: it declares the new relationship and generation/resolution properties.
The mode is opt-in; `extract_python_directory` and `load_python_directory`
default to the original independent file snapshots.

An import occurrence may link to a local file through `LOCAL_MODULE_CANDIDATE`,
with `evidence="INFERRED"` and `resolution="STATIC_LOCAL_PATH"`. Its source path,
line and source hash refer to the importing file. This is a deterministic
candidate based on the directory layout, not a guarantee about Python runtime
resolution: `sys.path`, import hooks, installed packages, built-in/frozen modules
and environment state can select something else. No code is imported or run.

Supported targets are unambiguous root modules (`helper.py`) and regular
packages/modules with `__init__.py` in every package directory. Absolute imports
match from the chosen source root. Explicit relative imports are considered
only inside an unambiguous regular package and cannot climb above it. Missing
modules, implicit namespace packages and collisions such as `pkg.py` alongside
`pkg/__init__.py` remain unlinked, including ambiguous package prefixes.

For `from pkg import name`, the candidate is the explicit module `pkg`, not the
attribute or a guessed `pkg.name` submodule. `from . import name` can therefore
link to the current package's `__init__.py`, without claiming what `name` means.
Module-only mode does not resolve calls or bind imported attributes to symbols.
It does not handle dynamic imports. The separate conservative call-candidate
mode below is explicitly opt-in.

**Generation isolation:** every node ID and every edge in this mode is scoped
to a SHA-256 generation of the complete manifest plus namespace. This includes
unchanged importer files and their `DEFINES`/`IMPORTS` links. Changing or removing
only a target file creates an entirely separate generation, so an unchanged
import occurrence cannot accidentally accumulate targets from old and new
snapshots. The IDs are also separate from default independent-file mode.
Identical replay is idempotent. Use distinct namespaces for distinct logical
repositories, even if their files happen to be identical.

All nodes and edges carry `generation_id`; the returned generation should be
bound as a query parameter to select the intended graph view:

```cypher
MATCH (a:CodeEntity)-[r:LOCAL_MODULE_CANDIDATE]->(b:CodeEntity)
WHERE r.generation_id = $generation
RETURN a.name AS importSpelling, b.source_path AS localCandidate,
       r.evidence AS evidence, r.resolution AS resolution
```

Old generations remain until explicitly maintained by the application. Every
changed checkout may add another full set of entities and edges, increasing disk
usage. There is no automatic latest-generation pointer, deletion, retention or
whole-load atomicity: batches may make an incomplete generation visible during
loading or after failure. A generation ID identifies content; it is not a
transaction snapshot or proof that every batch was committed. Retrying requires
immutable source and reconciliation of any uncertain commit. Readers that omit
the generation predicate can see multiple generations.

The same input/output bounds still apply; at most 20,000 relationships are
possible with local candidates enabled. CLI summaries explicitly distinguish
`EXTRACTED_AND_INFERRED` when such edges exist. Degree summaries count these
additional inferred edges too and remain structural counts, not importance.

## Inspect conservative imported-function call candidates

```sh
python -m brainwire.code_graph ./approved-source --namespace demo --resolve-calls --json > code-calls.json
```

```python
receipt = load_python_directory(
    graph, "./approved-source", namespace="demo", resolve_calls=True,
)
```

`resolve_calls=True` implies local-module candidates and uses its own complete
manifest/namespace generation, distinct from both default and module-only modes.
Apply the current `ontology()` before loading it. All source metadata is derived
from the **same read and parsed AST** used to compute each source hash; the call
analyzer does not reopen source files.

For example, `import helper as h` followed by a top-level function containing
`h.run()` can link to a unique top-level undecorated `run` definition in the local
`helper.py`. `from helper import run as go` followed by `go()` is also considered.
A `call` occurrence stores its source line range, UTF-8 **byte** columns and
stable per-file traversal ordinal, so two calls on the same line have distinct
IDs. `HAS_CALL` links the enclosing function to this occurrence with
`evidence="EXTRACTED"`. `STATIC_CALL_CANDIDATE` links the occurrence to the target
function with `evidence="INFERRED"` and `resolution="STATIC_IMPORT_BINDING"`.
Call arguments and code bodies are not persisted by this feature.

This initial analyzer intentionally omits many valid calls:

- Imports must be direct, unconditional statements in the module body. Aliases
  must be unique; dotted module imports require `as` to avoid confusing a root
  package binding with the explicitly imported submodule.
- Callers and targets must be unique top-level functions without decorators.
  Calls in nested functions, classes/methods, lambdas, decorators and default
  arguments are omitted. Dynamic imports, reexports, methods, callable objects,
  indirect aliases and longer attribute chains are unsupported.
- A conservative whole-file binding pass considers names regardless of order
  and across every scope. Parameters, assignment/walrus/for/with targets,
  exception and pattern bindings, delete, global/nonlocal declarations and
  duplicate/local imports or definitions can suppress a candidate. Thus a local
  binding in an unrelated function may omit an otherwise valid call.
- Observed attribute/subscript mutations of an alias or function suppress it.
  Names used as values outside a direct function/module call are treated as
  escaped and omitted; this includes passing a module/function to a mutator or
  assigning it to another name. Explicit reflection calls such as `exec`,
  `globals` and `setattr` conservatively disable call candidates in that file.

These are **static candidates**, never proof of runtime dispatch, execution,
reachability or return behavior. Python import hooks, monkeypatching, native code,
external state and other dynamic behavior can invalidate a lexical match even
when no mutation is visible to this analysis. Do not use the graph as a security
or formal-verification guarantee. Missing edges are not evidence of no call.

The analyzer caps retained direct-call observations at 10,000 across the tree,
and call occurrences count toward the existing total-entity and output limits.
All files/calls are analyzed and all bounds are checked before loading starts.
Limits failing during extraction cause no graph writes; subsequent load batches
remain individually committed and may expose incomplete generations. Replay,
target-only changes and target removal follow complete-generation isolation;
old call edges remain in their old generation until explicitly maintained.

## Explain entities and trace paths offline

Inspect a saved artifact without reopening its source files, running a model or
connecting to the database:

```sh
python -m brainwire.code_inspect code-graph.json explain App
python -m brainwire.code_inspect code-graph.json path App.run app.py --max-hops 8
```

These example selectors assume a class `App` with method `run` in `app.py`.
Selectors accept an exact `entity_id` or a **unique** `qualified_name`; missing or
ambiguous names fail instead of choosing an arbitrary match. The bounded
`--top-degree` preview can help discover IDs. For cross-file paths, first build
an artifact with `--resolve-imports`; default file-only syntax graphs have no
cross-file candidate links.

The Python API accepts an already loaded artifact:

```python
from brainwire.code_inspect import load_artifact, explain_node, find_path

artifact = load_artifact("code-graph.json")
explanation = explain_node(artifact, "App", max_edges=20)
path = find_path(artifact, "App.run", "app.py", max_hops=8, max_visited=1000)
```

An explanation contains a sanitized entity, incident evidence, the full incident
count and a `truncated` flag when `max_edges` intentionally selects a shorter
preview. It does not return arbitrary extra properties embedded in an input JSON
object. `max_edges` is 1–100, default 50.

Path search uses deterministic breadth-first traversal. It is undirected by
default so connections can be explored in either direction; add `--directed`
(or `directed=True`) to follow outgoing edges only. The node sequence follows
traversal order, while each edge retains its actual `source_id`, `target_id`,
relationship, source coordinates and evidence. `direction` is `outgoing` or
`incoming` relative to that traversal step. A path containing any
`LOCAL_MODULE_CANDIDATE` or `STATIC_CALL_CANDIDATE` reports
`evidence="INFERRED"`; an entirely extracted path
reports `EXTRACTED`. This tag describes the weakest declared evidence, not a
confidence probability or proof that the code would execute that path.

The default `max_hops` is 16 (allowed range 0–16), and `max_visited` is 10,000
(allowed range 1–10,000). The first deterministic shortest path within those
bounds is returned. Reaching the visit cap raises `InspectionLimitError` without
returning a partial path. Exhausting the permitted hop frontier returns
`status="not_found_within_bounds"`, which does not assert that the graph is
unconnected beyond that bound. A source equal to its target has zero hops.
Sorting and adjacency construction may inspect the entire bounded artifact;
the visit cap does not limit all preparatory work.

The file loader reads at most 8 MiB plus one byte **before JSON parsing**, rejects
oversize input, duplicate JSON keys, floating-point values and nonfinite JSON
constants, and validates the supported contract, entity identities, endpoints,
reported provenance, namespace and generation consistency. It opens a regular file
with nonblocking/no-follow flags on POSIX; direct file symlinks and special files
are rejected. The caller must still trust the chosen path's ancestors. This
loader/CLI currently requires POSIX; the dictionary inspection API is portable.
No `source_path` in the artifact is opened or executed.
The Python dictionary API receives an object that has already been materialized;
the 8 MiB loader cap does not bound allocations made before that call or establish
a process RSS ceiling.

There are at most 10,000 entities and 20,000 relationships. JSON output is limited
to 65,536 bytes by default; `--max-bytes` / `max_bytes` may reduce that bound.
Oversized output fails as a whole. CLI failures exit with status 2 and no partial
JSON on stdout. Input, validation and output caps are not parser sandboxing or a
process RSS guarantee; use an isolated process for adversarial input.

**Artifact access is the application's responsibility.** Offline inspection has
no server credentials, tenant authentication or per-user ACL evaluation. Treat
an artifact as a disclosure of all its source names, paths and relationships:
only distribute an approved export to an authorized recipient. Structural
validation does not authenticate its producer or prove its evidence tags;
inspection reports declared provenance and does not recompute hashes from source
files or validate a manifest's contents against those sources. SHA-256
fingerprints are not signatures. An untrusted JSON artifact can claim
false facts even when its structure is valid. Do not expose a shared company
artifact to users expecting the database's read-time ACL filtering, and do not
turn retrieved strings into agent instructions or permissions.

## Snapshots, retries and limits

- IDs incorporate namespace, logical path and source hash. Replaying identical
  input uses the same node/edge identities. Modified source creates a separate
  snapshot; old nodes and edges remain. There is no automatic reconciliation,
  deletion, watch mode or history retention.
- Extraction completes and validates before the first write. Syntax or bound
  errors produce no writes. Loading commits bounded node batches before edge
  batches; a failed later batch does not roll back earlier batches. Edge loads
  require endpoints. Reconcile an uncertain commit before retrying immutable
  input; the helper does not add automatic write retries.
- Use a dedicated authorized graph. The loader uses the supplied GraphClient
  identity and does not assign per-node ACLs. It does not make multi-user access
  safe merely by attaching source paths or namespaces.
- Source limit: 1 MiB UTF-8; AST limit: 100,000 nodes; output entities: at most
  10,000 (configurable downward with `max_entities`); final JSON artifact: 8 MiB.
  Logical paths are at most 1,024 bytes, namespaces 256 bytes and names 4,096
  bytes. Parsing and JSON validation allocate before some checks; these bounds
  are not a process RSS ceiling. Python's own parser can reject deeply nested
  syntax before extraction completes. `ast.parse` is a parser, not a sandbox;
  these size checks do not qualify adversarial parser inputs or hard CPU/RSS
  isolation. Use process/container limits for untrusted repositories.
- `schema_version="1"` and `extractor="brainwire-python-ast-v1"` identify the
  artifact contract. Review these when building an importer for stored artifacts.

Reproduce the real Bolt test with synthetic source:

```sh
python crates/server/tests/code_graph.py --binary target/debug/brainwire
```

It verifies replayed node/edge counts, relationship evidence and directory
loading with modified-source/namespace coexistence. It also checks inferred
candidate loads, same-line call positions, target-only changes/removal and
absence of cross-generation links.
It then stops its own temporary server and removes its temporary data.
