"""Verify the final local quorum candidate, source binding and fault receipts."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EVIDENCE = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify():
    candidate = json.loads((EVIDENCE / "candidate.json").read_text())
    checks = json.loads((EVIDENCE / "checks.json").read_text())
    receipt = json.loads((EVIDENCE / "acceptance-200-source-final.json").read_text())
    assert candidate["mode"] == "debug" and candidate["scope"] == "local-native-three-process"
    assert digest(ROOT / candidate["binary"]) == candidate["binary_sha256"]
    assert candidate["sources"]
    for name, expected in candidate["sources"].items():
        assert digest(ROOT / name) == expected, f"source changed: {name}"
    assert set(checks) == {"workspace_tests", "clippy", "fmt", "diff"}
    for check in checks.values():
        assert check["exit_code"] == 0
        assert digest(EVIDENCE / check["log"]) == check["sha256"]
    tests = (EVIDENCE / checks["workspace_tests"]["log"]).read_text()
    assert "test result: FAILED" not in tests
    assert "test result: ok." in tests
    assert receipt["binary_sha256"] == candidate["binary_sha256"]
    assert receipt["result"] == "PASS" and receipt["rows"] == 200 and receipt["synthetic"]
    assert len(receipt["checks"]) == 11
    assert all(receipt[key] for key in ("childProcessesStopped", "faultProxiesStopped", "ownedResourcesRemoved"))
    for name in ("pages", ".brainwire-raft", ".brainwire-graphs.json", ".brainwire-cluster.json"):
        assert receipt["missing_storage_refusals"][name]["exit_code"] != 0
    expected = receipt["logical_graph_digests"]
    assert set(expected) == {"pages", "aux"}
    assert set(receipt["offline_audits"]) == {"1", "2", "3"}
    for audit in receipt["offline_audits"].values():
        assert audit["result"] == "PASS"
        assert {name: info["logical_sha256"] for name, info in audit["graphs"].items()} == expected
        assert all(info["status"] == "ok" for info in audit["graphs"].values())
    for status in receipt["after_full_restart"]:
        assert status["membership"] == [1, 2, 3] and status["running"]
        assert status["committed_index"] == status["last_applied"]
    print("PASS: source/binary/checks bound; 11 fault checks; three matching graph copies; cleanup verified")


if __name__ == "__main__":
    verify()
