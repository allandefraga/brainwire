"""Run the final four synthetic source/recovery scenarios after the one-hour soak.

Use uv with duckdb==1.5.5 and neo4j==6.3.0. Each child owns disposable data and
credentials. This runner only publishes a receipt after successful child exit.
"""
import argparse
import hashlib
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time

DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[2]
NATIVE_SHA = "1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150"


def publish(path, value):
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, indent=2)
        stream.write("\n")
    try:
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def run(binary):
    verdict = DIRECTORY / "rehearsal.json"
    verdict.unlink(missing_ok=True)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == NATIVE_SHA, "candidate changed"
    load = json.loads((DIRECTORY / "mixed-100000-3600.json").read_text())
    assert load["result"] == "PASS" and load["load_seconds"] >= 3600 and load["binary_sha256"] == NATIVE_SHA
    tasks = (
        ("ducklake-100000", "docs/benchmarks/2026-10-12-mvp/acceptance.py",
         ["--tickets", "100000", "--ducklake", "--captures", str(DIRECTORY / "source-captures")]),
        ("recovery-100000", "docs/benchmarks/2026-10-10-client/acceptance.py", ["--tickets", "100000"]),
        ("multigraph-100000", "crates/server/tests/multigraph_acceptance.py", ["--rows-per-graph", "100000"]),
        ("typed-neighborhood-100000", "crates/server/tests/typed_neighborhood_acceptance.py",
         ["--nodes", "100000", "--iterations", "1000", "--cli-iterations", "20"]),
    )
    summary = {"binary_sha256": NATIVE_SHA, "scope": "four local synthetic scenarios; not real client or replication", "phases": {}}
    for name, script, options in tasks:
        output = DIRECTORY / (name + ".json")
        output.unlink(missing_ok=True)
        print("Starting " + name, flush=True)
        began = time.monotonic()
        process = subprocess.Popen([sys.executable, str(ROOT / script), "--binary", str(binary), *options],
                                   cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=600)
        except subprocess.TimeoutExpired:
            # Interrupt only this owned process group, including its native server.
            # Python can then run its context/finally cleanup; force-stop the same
            # group if it fails to exit. Never signal a default/existing server.
            try:
                os.killpg(process.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.communicate(timeout=5)
            raise RuntimeError(f"{name} timed out; no passing receipt published") from None
        if process.returncode:
            sys.stderr.write(stderr)
            raise RuntimeError(f"{name} failed with exit code {process.returncode}; no passing receipt published")
        receipt = json.loads(stdout)
        assert receipt["binary_sha256"] == NATIVE_SHA, "child candidate changed"
        if name == "ducklake-100000":
            # Its original harness returns after checks/audit/context cleanup;
            # promote only a successfully exited complete report, never failed output.
            assert receipt.get("result") in (None, "PASS") and receipt["audit"]["status"] == "ok"
            receipt["result"] = "PASS"
        else:
            assert receipt["result"] == "PASS"
        publish(output, receipt)
        summary["phases"][name] = {"exit_code": process.returncode, "seconds": round(time.monotonic() - began, 6),
                                  "receipt_sha256": hashlib.sha256(output.read_bytes()).hexdigest()}
        print("Completed " + name + ": PASS", flush=True)
    summary["result"] = "PASS"
    publish(verdict, summary)
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    options = parser.parse_args()
    run(options.binary.resolve(strict=True))
