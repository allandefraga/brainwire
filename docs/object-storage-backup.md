# Keep a checkpoint in object storage

Use a private S3-compatible bucket to keep a standalone graph checkpoint off
the database machine. The Brainwire server keeps its active RocksDB files on a
local disk or Docker volume. A restore downloads a full checkpoint to local
disk before the graph opens.

The experimental helper runs on macOS or Linux with Python 3.9+ and an optional
S3 client. Run the commands from a Brainwire source checkout.

```sh
python -m pip install 'boto3>=1.36,<2'
```

## Upload a verified checkpoint

Create a checkpoint and a local copy with a SHA-256 manifest by following the
[checkpoint guide](checkpoints.md). If you used the offline CLI, its receipt
does not include the checkpoint directory name. Add the name when creating the
local copy:

```sh
umask 077
brainwire db checkpoint research --destination /backups/nightly > nightly-receipt.json
python - <<'PY'
import json
from pathlib import Path
from examples.python.checkpoint_backup import copy_checkpoint

source = Path("/backups/nightly")
receipt = json.loads(Path("nightly-receipt.json").read_text())
copy_checkpoint(source, source.with_name("nightly-verified"), {**receipt, "name": source.name})
PY
```

Configure credentials with a standard boto3 profile, role, or environment
variables. Use a private bucket and a fresh prefix for every
backup. The helper targets the S3 API; this version has been tested with
MinIO, not AWS S3. Supply the HTTPS endpoint of your S3-compatible service.
Use `--ca-bundle` for a private CA. Plain HTTP is available only on numeric
loopback addresses for disposable local tests.

```sh
python -m examples.python.s3_checkpoint --endpoint https://minio.example.test \
  upload /backups/nightly-verified s3://graph-backups/research/2026-11-15
```

The helper reserves a `_brainwire/` subprefix using a conditional PUT, then uploads
one object per file with a SHA-256 checksum. It writes the manifest last.
Readers require the manifest, its listed objects, and the reservation object.
A failed upload may leave a reserved, incomplete
prefix. Use a new prefix for a retry. If the last PUT timed out, first check
whether it succeeded by downloading and verifying the copy in scratch storage,
then compare its manifest with the intended local copy.

## Download and verify before use

Download into an unused local directory outside the active graph data root.

```sh
python -m examples.python.s3_checkpoint --endpoint https://minio.example.test \
  download s3://graph-backups/research/2026-11-15 /backups/recovered
```

The helper checks the remote file list, byte counts and checksums. It writes
private files to a staging directory and only publishes the destination
after validation. It refuses to replace an existing destination. Run
`brainwire db check` on the restored store, then try a representative query
before handing it to the server. Follow the [restore procedure](checkpoints.md)
for removing the local manifest and moving the graph into the data root with
appropriate ownership. Retain a separate immutable copy of important backups.

The helper supports flat native checkpoint filenames, up to 20,000 files,
5 GB per file and 64 GiB total. It scans up to 100 listing pages and uses
ordinary PUT requests. Interrupted transfers must start again in a fresh prefix.
The service must support S3 conditional PUT, SHA-256 checksums and listing by
prefix. The caller needs scoped list, get and put
rights for the operations they perform. Configure encryption, retention and
restricted delete access on the bucket.

The checkpoint contains the graph's complete stored data, including visibility
metadata. Treat access to the bucket as access to the full graph. Checksums
detect corruption. They do not authenticate a backup against someone with
permission to change its manifest and objects. Keep the local checkpoint
immutable and use trusted directories for backups and restores.

## Test against MinIO locally

The [acceptance script](../examples/python/s3_checkpoint_acceptance.py) uses a
locally available MinIO image, a unique container, an ephemeral loopback
port and synthetic data. It creates a real Brainwire checkpoint, uploads it,
tests conditional overwrite protection, downloads and checks the graph, then
verifies that a corrupted remote object is refused. It checks that the scalar
index and type validation remain available on the restored graph. The test
uses temporary storage inside the container.

```sh
python -m examples.python.s3_checkpoint_acceptance \
  --binary /path/to/brainwire --image MINIO_IMAGE --docker-context default
```

For images that wrap the MinIO executable, provide its absolute path as
`--minio-entrypoint`.
