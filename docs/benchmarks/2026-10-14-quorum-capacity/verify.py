"""Verify source-bound native quorum qualification and reject false readiness evidence."""
import copy
import hashlib
import json
from pathlib import Path

EVIDENCE = Path(__file__).resolve().parent
ROOT = EVIDENCE.parents[2]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_receipt(r):
    assert r["result"] == "PASS" and r["synthetic"] and len(r["checks"]) == 15
    assert all(r[k] for k in ("childProcessesStopped", "faultProxiesStopped", "ownedResourcesRemoved"))
    assert r["inspection_healthy"]["complete"] is True
    assert r["inspection_healthy"]["retained_log_cap_agreement"] is True
    assert r["inspection_incomplete"]["complete"] is False
    assert r["inspection_incomplete"]["retained_log_cap_agreement"] is None
    assert r["inspection_incomplete_exit_code"] != 0
    for name, success in (("leader_readiness", True), ("follower_liveness", True),
                          ("follower_readiness", False), ("no_quorum_readiness", False)):
        assert r["local_probes"][name]["success"] is success
    for s in r["post_heal_status"]:
        assert s["max_log_bytes"] == 1 << 30
        assert s["log_remaining_bytes"] == s["max_log_bytes"] - s["log_bytes"]
    assert set(r["offline_audits"]) == {"1", "2", "3"}
    for audit in r["offline_audits"].values():
        assert audit["result"] == "PASS"
        assert {n: s["logical_sha256"] for n, s in audit["graphs"].items()} == r["logical_graph_digests"]
        assert all(s["status"] == "ok" for s in audit["graphs"].values())
    assert len(r["missing_storage_refusals"]) == 4
    assert all(s["exit_code"] != 0 for s in r["missing_storage_refusals"].values())


def verify():
    candidate = json.loads((EVIDENCE / "candidate.json").read_text())
    for path, sha in candidate["sources"].items():
        assert digest(ROOT / path) == sha, f"source changed: {path}"
    for native in candidate["binaries"]:
        assert digest(ROOT / native["path"]) == native["sha256"]
        r = json.loads((EVIDENCE / native["receipt"]).read_text())
        assert r["binary_sha256"] == native["sha256"] and r["rows"] == native["rows"]
        check_receipt(r)
    for check in candidate["checks"].values():
        assert check["exit_code"] == 0
        assert digest(EVIDENCE / check["log"]) == check["sha256"]
    r = json.loads((EVIDENCE / candidate["binaries"][0]["receipt"]).read_text())
    for mutate in (lambda x: x.update(result="FAIL"),
                   lambda x: x.update(ownedResourcesRemoved=False),
                   lambda x: x["local_probes"]["no_quorum_readiness"].update(success=True),
                   lambda x: x["inspection_incomplete"].update(retained_log_cap_agreement=True),
                   lambda x: x.update(inspection_incomplete_exit_code=0),
                   lambda x: x["post_heal_status"][0].update(log_remaining_bytes=0)):
        invalid = copy.deepcopy(r)
        mutate(invalid)
        try:
            check_receipt(invalid)
        except AssertionError:
            pass
        else:
            raise AssertionError("invalid evidence accepted")
    print("PASS: sources, binaries, checks, 15 fault assertions, matching replicas and cleanup; 6 false receipts rejected")


if __name__ == "__main__":
    verify()
