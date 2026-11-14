# Synthetic uncertain-commit recovery

On 2026-11-14, a locally built v0.20.0-beta.2 candidate was tested as a single authenticated native server. Each cycle sent `COMMIT` without reading its response, killed the server process, and reconciled the entire transaction after restart. The [receipt](2026-11-14-uncertain-commit-020b2.json) identifies the binary and driver. It predates the later cgroup-reservation changes; it is not evidence for another build.

All 16 cycles recovered either all 100 nodes and 100 relationships or none: 10 complete, six absent. Previously acknowledged data, indexed values and overwrites survived; the offline audit passed. This is a small synthetic process-kill test, not a power-loss test, a test of failure at a known fsync boundary, or distributed failover qualification. An unacknowledged commit can succeed: reconcile it before retrying.

To reproduce with an already built binary and the Python Bolt driver installed, from the repository root:

```sh
python - <<'PY'
from pathlib import Path
import importlib.util
import json
import sys

sys.path.insert(0, 'crates/server/tests')
spec = importlib.util.spec_from_file_location('uncertain_commit', 'crates/server/tests/uncertain_commit.py')
test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(test)
delays = (0, .00005, .0002, .0005, .001, .002, .005, .01)
print(json.dumps(test.run(Path('target/debug/brainwire').resolve(strict=True), 100,
                          delays + tuple(reversed(delays))), indent=2))
PY
```

The versioned runner's SHA-256 for the receipt is `880a6247f060b39c6475ad50acbb64f8e351ecb4a807b90c44f6474eecec20ee`. Timing and outcomes will vary by machine.
