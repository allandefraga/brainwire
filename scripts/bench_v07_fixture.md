# Prepare the synthetic Bolt ACL fixture

This helper copies a recognized **completed synthetic embedded baseline** into
an exclusive new directory, stamps ACLs on its Usuario and Carro nodes, verifies
known results, stops its preparation server, and runs an offline consistency
check before writing a frozen manifest. The source graph is unchanged. It is not
a performance run and does not establish a process memory limit.

Build the intended Brainwire binary first. Use a completed receipt created by
`bench_v07.py`; that receipt's sibling `data/bench` must still exist and its owner
must be stopped. Explicitly confirm that this source is synthetic and disposable:

```sh
python3 scripts/bench_v07_fixture.py prepare \
  --source-receipt /private/tmp/embedded-baseline/receipt.json \
  --confirm-source-synthetic \
  --binary /absolute/path/to/brainwire \
  --source-revision EXACT_40_CHARACTER_COMMIT \
  --output /private/tmp/bolt-acl-fixture
```

The source receipt must identify the expected four-label benchmark schema and a
successful `check` phase. Copying holds the source RocksDB POSIX `LOCK`; an active
owner, hard-linked LOCK, symlink, missing file or reused output directory is refused. This is an
explicit synthetic-data workflow, not cryptographic proof of dataset provenance.
The helper chooses an unused loopback port and uses its own server child only;
if preparation fails, it stops that child and records failure without a frozen
manifest. Its main process handles termination signals to run this cleanup.

For one million rows per label, the original four labels contain 4M nodes and
4M declared edges. The baseline's later write phase may add `Pessoa` nodes;
those are retained and explicitly counted in `prepared.json`. They are not
included in the four-label count or queried by this diagnostic. No exact 4M-total
claim is made when these extra nodes exist.

ACL stamping is sequential preparation in batches of at most 10,000 (default
2,000), not a write-throughput measurement. In the default balanced mode, for every Usuario/Carro identity,
indices `i % 8 < 4` allow the synthetic reader and the rest allow a separate
synthetic principal. The one-to-one Usuario→Carro pair is visible only when both
endpoints are visible. The helper checks actual unrestricted label/edge counts,
then delegated allow-all and restricted count/top-N results against computed
expectations before freezing. At 1M rows, the restricted slice has 500,000 users,
500,000 visible pairs and four brands with 125,000 pairs each. It stamps ACLs for
Usuario and Carro only; other labels are outside the restricted workload.

Use `--acl-mode full-cursor` when qualifying the requested walk over one million
**visible** users: this mode makes every Usuario visible and keeps the same
half-visible Carro pattern. At one million rows it expects a 1M-row restricted
cursor, 500,000 visible pairs, and four brands at 125,000 pairs each. The default
`--acl-mode balanced` keeps the original half-visible pattern on both labels and
therefore has a 500,000-row restricted cursor. The selected mode is recorded in
both `prepared.json` and `manifest.json`; compare benchmarks using the same mode.

The output directory is mode `0700`; policy, client environment, manifest and
receipt files are mode `0600`. Tokens are random, appear only in private memory
or files, and are never printed. The final policy removes the preparation writer
and permits only a read-only visibility delegate. The server is stopped when
`prepare` returns. Keep the whole directory private and use it only for synthetic
benchmarks.

Start the prepared read-only server in one terminal:

```sh
python3 scripts/bench_v07_fixture.py serve --fixture /private/tmp/bolt-acl-fixture
```

`serve` validates the binary hash, isolated data directory, loopback listener and
reader-only policy, then replaces its process with Brainwire, so normal stop
signals reach the database directly. The chosen port may have been taken after
preparation; in that case startup fails instead of contacting or stopping another
server. The helper never prints credentials or exports them to the shell.

In another terminal, load the private environment directly into the benchmark
child without printing it:

```python
import json, os, subprocess, sys
from pathlib import Path
folder = Path('/private/tmp/bolt-acl-fixture')
env = {**os.environ, **json.loads((folder/'client-env.json').read_text())}
subprocess.run([sys.executable, 'scripts/bench_v07_bolt.py',
                '--manifest', str(folder/'manifest.json'),
                '--output', '/private/tmp/bolt-acl-measurement'],
               env=env, check=True)
```

The manifest uses the integer version returned by `db.schema()`. Its binary hash
is computed from the actual executable, while the source revision is supplied by
the caller. The fixture is unchanged by the read-only diagnostic; cursor pages
are separate transactions, so do not attach other writers. Stop the server after
measurement and run the separate post-benchmark check:

```sh
python3 scripts/bench_v07_fixture.py check --fixture /private/tmp/bolt-acl-fixture
```

That command opens only the prepared graph, fails if a server still owns it, and
writes an exclusive `post-benchmark-check.json` only when the audit reports
`status: ok`. Existing check receipts are not overwritten. It never stops another
process.

The fake-driver tests use small synthetic files and no real network or database:

```sh
python3 -m unittest scripts.test_bench_v07_fixture -v
```

This helper has not been run against a live server as part of its implementation.
It does not qualify concurrent ingestion, production ACL integration, TLS,
process RSS, a hard 4 GiB ceiling or multi-machine recovery. Native loopback Bolt
is used only for this isolated local fixture.
