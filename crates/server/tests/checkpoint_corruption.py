"""Fail-closed local checkpoint corruption rehearsal using only disposable stores.

A checkpoint can hard-link immutable SST files. Copy its files into an independent
owned directory before corruption so neither original nor checkpoint is mutated.
This tests deterministic footer corruption, not power loss or every media fault.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]


def corruption_diagnostic(text):
    # The database name itself contains "corrupt"; require the actual storage
    # error class and deterministic footer failure, not a generic open error.
    return "Corruption: Bad table magic number:" in text


def self_check():
    assert corruption_diagnostic("cannot open database corrupt: Corruption: Bad table magic number: expected 1, found 2")
    for message in ("cannot open database corrupt: Permission denied", "cannot open database corrupt: IO error", "database corrupt is locked", ""):
        assert not corruption_diagnostic(message)
    return {"diagnostic_check": "PASS", "runtime_exercised": False}


def run(binary):
    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "synthetic": True, "rows": 1000,
              "scope": "offline local checkpoint footer corruption; not power loss, object storage or cross-version restore"}
    with tempfile.TemporaryDirectory(prefix="brainwire-checkpoint-corruption-") as temporary:
        directory = Path(temporary)
        data = directory / "data"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(data), BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_MEMORY_QUERY="64MiB")

        def command(*arguments, expect_success=True):
            result = subprocess.run([str(binary), *arguments], cwd=ROOT, env=env,
                                    capture_output=True, text=True, timeout=60)
            if expect_success and result.returncode:
                raise RuntimeError(result.stderr)
            return result

        command("db", "create", "original", "--ontology", str(ROOT / "examples/python/schema.json"))
        parameters = directory / "rows.json"
        parameters.write_text(json.dumps({"rows": [{"id": f"corruption-{i}", "email": f"corruption-{i}@example.test"} for i in range(1000)]}))
        command("cypher", "--database", str(data / "original"), "--params-file", str(parameters),
                "--query", "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email")
        checkpoint = data / "checkpoint"
        created = json.loads(command("db", "checkpoint", "original", "--destination", str(checkpoint)).stdout)
        assert created["status"] == "checkpoint_created"
        independent = data / "corrupt"
        shutil.copytree(checkpoint, independent, copy_function=shutil.copy2)
        candidates = sorted(independent.glob("*.sst"))
        assert candidates, "native checkpoint contains no SST file"
        selected = candidates[0]
        original_sst = checkpoint / selected.name
        before = hashlib.sha256(original_sst.read_bytes()).hexdigest()
        # Never edit a hard link shared with the original immutable store.
        assert selected.stat().st_ino != original_sst.stat().st_ino
        with selected.open("r+b") as stream:
            stream.seek(-1, os.SEEK_END)
            byte = stream.read(1)
            stream.seek(-1, os.SEEK_END)
            stream.write(bytes([byte[0] ^ 0xFF]))
        failure = command("db", "check", "corrupt", "--json", expect_success=False)
        assert failure.returncode != 0, "corrupted checkpoint was accepted by integrity audit"
        assert corruption_diagnostic(failure.stderr), "failure did not identify RocksDB footer corruption"
        assert hashlib.sha256(original_sst.read_bytes()).hexdigest() == before
        # Offline catalog opening eagerly discovers every named store. A bad
        # neighboring store fails closed too; do not misreport per-graph isolation.
        blocked = command("db", "check", "original", "--json", expect_success=False)
        assert blocked.returncode != 0 and corruption_diagnostic(blocked.stderr)
        isolated_corruption = directory / "isolated-corrupt-copy"
        shutil.move(str(independent), str(isolated_corruption))
        audits = {}
        for name in ("original", "checkpoint"):
            audits[name] = json.loads(command("db", "check", name, "--json").stdout)
            assert audits[name]["status"] == "ok"
            stats = json.loads(command("db", "stats", name, "--json").stdout)
            assert stats["nodes"] == 1000
        report.update(result="PASS", audit=audits["original"], checkpoint_audit=audits["checkpoint"],
                      corrupted_audit_exit_code=failure.returncode,
                      corrupted_error_class="RocksDB Corruption: Bad table magic number",
                      neighboring_corruption_blocks_catalog_open=True,
                      corrupted_error_sha256=hashlib.sha256(failure.stderr.encode()).hexdigest(),
                      intact_sst_sha256=before,
                      checks=["independent copy breaks checkpoint SST hard links before mutation",
                              "deterministic SST footer corruption fails offline graph/index audit",
                              "neighboring corrupted store fails offline catalog opening closed",
                              "after isolating owned corrupt copy, original and checkpoint retain 1000 nodes and pass audits"])
    report["ownedResourcesRemoved"] = True
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    parser.add_argument("--self-check", action="store_true")
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.binary.resolve()), indent=2))
