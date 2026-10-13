#!/usr/bin/env python3
"""Validate this frozen candidate's local evidence; never infer missing acceptance."""
import argparse
import copy
import hashlib
import json
import math
from pathlib import Path
import sys
import tempfile

NATIVE_SHA = "1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150"
IMAGE_ID = "sha256:d08c2e5af98ea237b568b86bf58af99af7d80ae5425ad7aaecdc37f620d821d3"
PROXY_ID = "sha256:1aca32ee932b96d2a75f1882909325839dd23c138ba9c6fd26e4d88179f30f13"
DRIVERS = {"5": "5.28.6", "6": "6.3.0"}
RECEIPTS = tuple(f"{kind}-driver{driver}.json" for kind in ("auth", "connection-limits", "adversarial", "tls") for driver in DRIVERS) + (
    "termination.json", "transport.json", "static-checks.json", "ui-file-checks.json", "deploy-render.json", "mixed-100000-3600.json", "ducklake-100000.json", "recovery-100000.json", "checkpoint-corruption.json", "multigraph-100000.json", "typed-neighborhood-100000.json")
CAPTURES = (
    "workspace-ontology-en-120x30", "workspace-memory-en-120x30", "workspace-memory-stale-en-120x30",
    "workspace-memory-recovered-en-120x30", "workspace-indexes-en-120x30", "workspace-graph-en-120x30",
    "workspace-graph-scrolled-en-120x30", "workspace-ontology-pt-BR-80x24", "workspace-memory-pt-BR-80x24",
    "workspace-indexes-pt-BR-80x24", "workspace-graph-pt-BR-80x24", "workspace-reader-memory-forbidden-en-120x30",
    "workspace-reader-indexes-after-forbidden-en-120x30")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def number(value, minimum):
    return type(value) in (int, float) and math.isfinite(value) and value >= minimum


def audit(receipt, key="audit"):
    require(isinstance(receipt.get(key), dict) and receipt[key].get("status") == "ok", f"{key} must be ok")


def validate(name, receipt):
    require(isinstance(receipt, dict), "receipt must be an object")
    if name == "ui-file-checks.json":
        validate_image_proof(receipt)
        return
    if name == "deploy-render.json":
        require(receipt.get("mode") == "local-render-only", "deployment evidence must be classified as local render only")
        checks = receipt.get("checks")
        require(isinstance(checks, list) and len(checks) == 6 and all(isinstance(item, str) and item for item in checks), "six deployment render checks required")
        return
    require(receipt.get("result") == "PASS", "result must be PASS")
    if name not in ("transport.json", "static-checks.json", "multigraph-100000.json"):
        audit(receipt)
    if name.startswith("tls-"):
        require(receipt.get("imageId") == IMAGE_ID, "Linux candidate image mismatch")
        require(receipt.get("proxyImageId") == PROXY_ID, "pinned proxy image mismatch")
        require(receipt.get("image") == "brainwire:beta-20261013", "candidate image tag mismatch")
    else:
        require(receipt.get("binary_sha256") == NATIVE_SHA, "native candidate hash mismatch")
    if "-driver" in name:
        version = DRIVERS[name.split("-driver")[1][0]]
        require(receipt.get("driverVersion" if name.startswith("tls-") else "driver") == version, "driver version mismatch")
    if name.startswith(("connection-limits", "adversarial", "tls")) or name == "termination.json":
        require(receipt.get("ownedResourcesRemoved") is True, "owned resources cleanup missing")
    if name.startswith("connection-limits"):
        require(receipt.get("default_idle") is True, "default idle profile required")
        require(number(receipt.get("default_idle_observed_seconds"), 300), "default 300-second idle observation missing")
    if name.startswith("adversarial"):
        require(receipt.get("default_idle") is False, "adversarial short IO profile required")
        require(receipt.get("decoded_cap_bytes") == 65536, "adversarial decoded cap mismatch")
        require(number(receipt.get("amplified_encoded_bytes"), 1) and receipt["amplified_encoded_bytes"] < 65536, "encoded amplification proof missing")
    if name.startswith("tls"):
        require(receipt.get("rows") == 1000 and receipt.get("tlsVersion") == "TLSv1.3", "TLS workload/protocol missing")
        idle = receipt.get("idlePool", {})
        require(idle.get("first_reuse") == "PASS" and idle.get("new_frontend_peer_observed") is True, "TLS pool reconnect missing")
        proxy_timeout = idle.get("proxy_idle_timeout_seconds")
        liveness_timeout = idle.get("liveness_check_timeout_seconds")
        require(number(proxy_timeout, 1) and proxy_timeout == 60, "TLS proxy idle timeout must be 60 seconds")
        require(number(idle.get("idle_seconds"), 0) and idle["idle_seconds"] > proxy_timeout, "TLS idle observation must cross proxy expiry")
        require(number(liveness_timeout, 1) and liveness_timeout == 30 and liveness_timeout < proxy_timeout, "TLS liveness timeout must be positive and below proxy expiry")
    if name == "transport.json":
        require(receipt.get("production_handshake_timeout_seconds") == 10 and receipt.get("production_authentication_timeout_seconds") == 10, "production transport deadlines missing")
        require(receipt.get("silent_sockets") == 64 and receipt.get("unauthenticated_sockets") == 64 and receipt.get("request_payload_cap_bytes") == 32, "transport peer/cap profile mismatch")
    if name == "static-checks.json":
        require(receipt.get("rust_tests_passed") == 541 and receipt.get("rust_tests_failed") == 0 and receipt.get("rust_tests_ignored") == 0, "Rust test counts mismatch")
        require(receipt.get("python_examples_passed") == 17 and receipt.get("client_python_passed") == 23, "Python test counts mismatch")
        require(receipt.get("clippy_passed") is True and receipt.get("fmt_passed") is True, "Clippy/format verification missing")
        logs = receipt.get("logs")
        require(isinstance(logs, dict) and len(logs) >= 5, "static verification log digests missing")
        for log in logs.values():
            digest = log.get("sha256", "") if isinstance(log, dict) else ""
            require(isinstance(digest, str) and len(digest) == 64 and all(c in "0123456789abcdef" for c in digest) and log.get("exit_code") == 0, "invalid static log proof")
    if name == "checkpoint-corruption.json":
        require(receipt.get("rows") == 1000, "checkpoint corruption fixture must have 1000 rows")
        audit(receipt, "checkpoint_audit")
        exit_code = receipt.get("corrupted_audit_exit_code")
        require(type(exit_code) is int and exit_code != 0, "corrupted checkpoint audit must fail")
        require(receipt.get("corrupted_error_class") == "RocksDB Corruption: Bad table magic number", "corrupted checkpoint requires the verified RocksDB corruption diagnostic")
        require(receipt.get("neighboring_corruption_blocks_catalog_open") is True, "neighboring corruption fail-closed boundary missing")
        require(receipt.get("ownedResourcesRemoved") is True, "corruption fixture cleanup missing")
    if name == "multigraph-100000.json":
        graphs = ["graph_a", "graph_b", "graph_c"]
        require(receipt.get("graphs") == graphs and receipt.get("rows_per_graph") == 100000, "three exact 100k graph profiles required")
        require(receipt.get("ownedResourcesRemoved") is True and receipt.get("childServerStopped") is True and receipt.get("privateCredentialsAndStorageRemoved") is True, "multigraph cleanup missing")
        require(receipt.get("kill_exit_code") == -9, "multigraph SIGKILL recovery missing")
        for key in ("audits", "before_restart", "after_restart", "offline_counts", "concurrent_writers", "concurrent_readers", "tui_readonly_snapshots"):
            values = receipt.get(key)
            require(isinstance(values, dict) and set(values) == set(graphs), f"{key} must include only all three graphs")
        for graph in graphs:
            require(receipt["audits"][graph].get("status") == "ok", f"{graph} audit failed")
            require(receipt["offline_counts"][graph].get("nodes") == 100000, f"{graph} offline count mismatch")
            for key in ("before_restart", "after_restart"):
                stats = receipt[key][graph]
                require(stats.get("nodes") == 100000 and stats.get("all_active") is True and stats.get("foreign_graph_email_rows") == 0, f"{graph} {key} isolation/count proof missing")
            writer = receipt["concurrent_writers"][graph]
            require(writer.get("initial_rows") == 100000 and writer.get("overwritten_rows") == 100000 and number(writer.get("acknowledged_batches"), 1), f"{graph} acknowledged insert/overwrite workload missing")
            reader = receipt["concurrent_readers"][graph]
            require(number(reader.get("read_cycles"), 1) and reader.get("foreign_rows") == 0, f"{graph} concurrent reader isolation missing")
    if name == "typed-neighborhood-100000.json":
        require(receipt.get("driver") == "6.3.0", "typed neighborhood driver mismatch")
        require(receipt.get("nodes_requested") == 100000 and receipt.get("edges_requested") == 99999, "typed neighborhood requires 100k nodes and 99999 edges")
        stats = receipt.get("offline_stats", {})
        require(stats.get("nodes") == 100000 and stats.get("edges") == 99999, "typed neighborhood offline counts mismatch")
        require(receipt.get("ontology_sha256") == "ee19d596b483e2a9861281b3433d2e2b18be0529f73a7616881e53fa64845a29", "typed neighborhood ontology provenance mismatch")
        require(receipt.get("ownedResourcesRemoved") is True, "typed neighborhood cleanup missing")
        require(receipt.get("query") == "MATCH (n:Ticket {entity_id:$id})-[:HAS_COMMENT]->(m:TicketComment) RETURN m.entity_id AS entity_id LIMIT 20", "typed bounded query provenance mismatch")
        for key, required_requests in (("python_full_request", 1000), ("cli_process_snapshot", 20)):
            timing = receipt.get(key, {})
            require(type(timing.get("requests")) is int and timing["requests"] == required_requests, f"{key} iteration profile mismatch")
            metrics = [timing.get(field) for field in ("min_us", "p50_us", "p95_us", "p99_us", "max_us")]
            require(all(number(value, 0) for value in metrics) and metrics == sorted(metrics), f"{key} finite ordered latency metrics missing")
        require(receipt["python_full_request"].get("errors") == 0, "typed neighborhood query errors")
        snapshot = receipt.get("snapshot", {})
        text = snapshot.get("text")
        require(snapshot.get("sample_rows") == 20 and snapshot.get("more_available") is True, "typed native snapshot bound/more proof missing")
        require(isinstance(text, str) and "20 outgoing rows / 20" in text and "MORE AVAILABLE" in text, "typed native snapshot text missing")
        require(hashlib.sha256(text.encode("utf-8")).hexdigest() == snapshot.get("sha256"), "typed native snapshot hash mismatch")
    if name == "termination.json":
        require(receipt.get("signal") == "SIGTERM", "SIGTERM proof missing")
        cases = receipt.get("termination", [])
        require(len(cases) == 4 and all(isinstance(c, dict) and c.get("exit_code") == -15 and number(c.get("exit_seconds"), 0) and c["exit_seconds"] < 5 for c in cases), "bounded SIGTERM cases missing")
    if name == "mixed-100000-3600.json":
        require(receipt.get("rows") == 100000 and receipt.get("requested_seconds") == 3600, "100k / one-hour load profile required")
        require(number(receipt.get("load_seconds"), 3600), "continuous load shorter than one hour")
        require(receipt.get("hot_point_reads") is True, "overlapping hot point reads required")
        require(number(receipt.get("acknowledged_latest_updates_verified"), 1), "acknowledged updates were not verified")
        for kind in ("point", "scalar", "writes"):
            stats = receipt.get(kind, {})
            require(isinstance(stats, dict) and number(stats.get("requests"), 1), f"{kind} requests missing")
            require(all(number(stats.get(k), 0) for k in ("p50_ms", "p95_ms", "p99_ms")), f"{kind} latency missing")
        observations = receipt.get("observations")
        require(isinstance(observations, list) and len(observations) >= 2, "memory observations missing")
        previous_seconds = -1
        for observation in observations:
            require(isinstance(observation, dict), "memory observation must be an object")
            elapsed = observation.get("seconds")
            require(number(elapsed, 0) and elapsed >= previous_seconds and elapsed <= receipt["load_seconds"], "memory observation timestamps must be finite, monotonic and inside the load")
            previous_seconds = elapsed
            for counter in ("rss_kib", "poolReservedBytes", "poolPeakBytes", "poolRejections"):
                value = observation.get(counter)
                require(type(value) is int and value >= 0, f"memory observation {counter} must be a nonnegative integer")
        # The harness samples every five seconds; allow one extra interval for host contention.
        require(observations[0]["seconds"] <= 10 and previous_seconds >= receipt["requested_seconds"] - 10, "memory observations must span the full requested hour")
    if name in ("ducklake-100000.json", "recovery-100000.json"):
        fixture = receipt.get("fixture", {})
        require(fixture.get("tickets") == 100000 and fixture.get("comments") == 200000 and fixture.get("relationships") == 200000, "source fixture counts mismatch")
        require(number(receipt.get("import_seconds"), 0.001), "source import duration missing")
        limits = receipt.get("limits", {})
        require(all(limits.get(key) == expected for key, expected in {"encoded_message_mib": 8,
            "decoded_message_mib": 16, "io_timeout_ms": 300000, "cache_mib": 64,
            "memtables_mib": 16, "query_mib": 64}.items()), "source bounded pilot limits mismatch")
        if name.startswith("ducklake"):
            export = receipt.get("export", {})
            require(export.get("tickets") == 100000 and export.get("comments") == 200000, "DuckLake export counts mismatch")
            require(export.get("source_type") == "ducklake-sqlite" and type(export.get("snapshot_version")) is int, "pinned SQLite DuckLake provenance missing")
        else:
            audit(receipt, "validation_audit")
            audit(receipt, "restore_audit")
    if name != "mixed-100000-3600.json":
        checks = receipt.get("checks")
        require(isinstance(checks, list) and checks and all(isinstance(item, str) and item for item in checks), "completed check list missing")


def validate_image_proof(receipt, directory=None):
    require(isinstance(receipt, dict) and receipt.get("result") == "PASS" and receipt.get("binary_sha256") == NATIVE_SHA, "UI image proof must pass for the frozen native candidate")
    require(receipt.get("capture_count") == 13 and isinstance(receipt.get("pillow"), str) and receipt["pillow"], "UI image decode tool/count proof missing")
    checks = receipt.get("checks")
    require(isinstance(checks, dict) and set(checks) == {name + ".png" for name in CAPTURES}, "UI image proof must cover exactly all 13 captures")
    for name, proof in checks.items():
        require(isinstance(proof, dict) and proof.get("fully_decoded") is True, f"full PNG decode proof missing: {name}")
        expected = (880, 576) if "80x24" in name else (1320, 720)
        require((proof.get("width"), proof.get("height")) == expected, f"PNG dimensions mismatch: {name}")
        digest = proof.get("sha256", "")
        require(isinstance(digest, str) and len(digest) == 64 and all(c in "0123456789abcdef" for c in digest), f"invalid PNG digest: {name}")
        if directory is not None:
            path = directory / name
            require(path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == digest, f"PNG bytes differ from full decode proof: {name}")


def validate_ui(receipt, directory):
    require(isinstance(receipt, dict) and receipt.get("binary_sha256") == NATIVE_SHA, "UI native hash mismatch")
    require(receipt.get("actual_nodes") == 100000 and receipt.get("actual_edges") == 99999 and receipt.get("nodes_requested") == 100000, "UI 100k counts mismatch")
    require(receipt.get("offline_stats", {}).get("nodes") == 100000 and receipt["offline_stats"].get("edges") == 99999, "UI offline counts mismatch")
    require(receipt.get("graph_sample_max_rows") == 20 and receipt.get("default_workspace_without_view_flag") is True, "bounded default workspace proof missing")
    require(receipt.get("all_child_processes_stopped") is True and receipt.get("temporary_credentials_and_storage_removed_on_context_exit") is True, "UI cleanup proof missing")
    require(receipt.get("captures") == list(CAPTURES), "UI must contain the expected 13 unique captures")
    require(receipt.get("languages") == ["en", "pt-BR"] and receipt.get("dimensions") == [[120, 30], [80, 24]], "UI language/dimension coverage missing")
    for capture in CAPTURES:
        png, text = directory / (capture + ".png"), directory / (capture + ".txt")
        require(png.is_file() and png.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"), f"missing/invalid PNG: {png.name}")
        require(text.is_file() and text.stat().st_size > 0, f"missing text: {text.name}")


def read(path):
    require(path.is_file() and 0 < path.stat().st_size <= 32 * 1024 * 1024, f"missing, empty or oversized receipt: {path}")
    def pairs(items):
        values = {}
        for key, value in items:
            require(key not in values, f"duplicate JSON key: {key}")
            values[key] = value
        return values
    def invalid_constant(value):
        raise ValueError(f"non-finite JSON constant: {value}")
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=pairs, parse_constant=invalid_constant)


def collect(directory, ui_directory):
    digests = {}
    for name in RECEIPTS:
        path = directory / name
        try:
            receipt = read(path)
            validate(name, receipt)
            if name == "ui-file-checks.json":
                validate_image_proof(receipt, ui_directory)
            if name == "static-checks.json":
                for log in receipt["logs"].values():
                    relative = Path(log.get("path", ""))
                    require(not relative.is_absolute() and relative.parts[:1] == ("logs",) and ".." not in relative.parts, "invalid static log path")
                    log_path = directory / relative
                    require(log_path.is_file(), f"missing static log: {relative}")
                    require(hashlib.sha256(log_path.read_bytes()).hexdigest() == log["sha256"], f"static log hash mismatch: {relative}")
        except (ValueError, TypeError, KeyError, AttributeError) as error:
            raise ValueError(f"{name}: {error}") from error
        digests[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    path = ui_directory / "receipt.json"
    validate_ui(read(path), ui_directory)
    digests["ui/receipt.json"] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"result": "PASS", "scope": "local synthetic frozen-candidate evidence; not a production or distributed beta approval",
            "native_binary_sha256": NATIVE_SHA, "linux_image_id": IMAGE_ID, "deployment_qualification": "local-render-only; Kubernetes/Swarm runtime remains unqualified", "receipt_count": len(digests), "receipt_sha256": digests,
            "ui_asset_sha256": {capture + suffix: hashlib.sha256((ui_directory / (capture + suffix)).read_bytes()).hexdigest()
                                for capture in CAPTURES for suffix in (".png", ".txt")}}


def self_check():
    base = {"result": "PASS", "binary_sha256": NATIVE_SHA, "audit": {"status": "ok"}, "driver": "5.28.6", "checks": ["verified"]}
    validate("auth-driver5.json", base)
    for field, bad in (("result", "FAIL"), ("binary_sha256", "0" * 64), ("audit", {"status": "failed"}), ("driver", "6.3.0"), ("checks", [])):
        mutated = copy.deepcopy(base); mutated[field] = bad
        try:
            validate("auth-driver5.json", mutated)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid {field}")
    with tempfile.TemporaryDirectory() as folder:
        directory = Path(folder)
        for contents in (None, "", "{", "{}"):
            path = directory / RECEIPTS[0]
            if contents is not None:
                path.write_text(contents)
            try:
                collect(directory, directory)
            except ValueError:
                pass
            else:
                raise AssertionError("accepted incomplete receipt set")
    load = {**copy.deepcopy(base), "rows": 100000, "requested_seconds": 3600, "load_seconds": 3601,
        "hot_point_reads": True, "acknowledged_latest_updates_verified": 200,
        "observations": [{"seconds": elapsed, "rss_kib": 1, "poolReservedBytes": 0, "poolPeakBytes": 0, "poolRejections": 0} for elapsed in (0, 3595)]}
    for kind in ("point", "scalar", "writes"):
        load[kind] = {"requests": 1, "p50_ms": 1, "p95_ms": 2, "p99_ms": 3}
    validate("mixed-100000-3600.json", load)
    for field, bad in (("load_seconds", 3599), ("rows", 99999), ("hot_point_reads", False), ("acknowledged_latest_updates_verified", 0), ("observations", []),
                       ("observations", [{"seconds": 0, "rss_kib": -1}]),
                       ("observations", [{"seconds": 10, "rss_kib": 1, "poolReservedBytes": 0, "poolPeakBytes": 0, "poolRejections": 0},
                                         {"seconds": 9, "rss_kib": 1, "poolReservedBytes": 0, "poolPeakBytes": 0, "poolRejections": 0}]),
                       ("observations", [{"seconds": float("nan"), "rss_kib": 1}])):
        changed = copy.deepcopy(load); changed[field] = bad
        try:
            validate("mixed-100000-3600.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid hour-load {field}")
    tls = {**copy.deepcopy(base), "imageId": IMAGE_ID, "proxyImageId": PROXY_ID,
        "image": "brainwire:beta-20261013", "driverVersion": "5.28.6", "ownedResourcesRemoved": True,
        "rows": 1000, "tlsVersion": "TLSv1.3", "idlePool": {"first_reuse": "PASS", "new_frontend_peer_observed": True,
                     "idle_seconds": 65, "proxy_idle_timeout_seconds": 60, "liveness_check_timeout_seconds": 30}}
    validate("tls-driver5.json", tls)
    for field in ("imageId", "proxyImageId"):
        changed = copy.deepcopy(tls); changed[field] = "sha256:" + "0" * 64
        try:
            validate("tls-driver5.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid {field}")
    for field, bad in (("idle_seconds", 0), ("liveness_check_timeout_seconds", 60)):
        changed = copy.deepcopy(tls); changed["idlePool"][field] = bad
        try:
            validate("tls-driver5.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid TLS idle {field}")
    with tempfile.TemporaryDirectory() as folder:
        directory = Path(folder)
        ui = {"binary_sha256": NATIVE_SHA, "actual_nodes": 100000, "actual_edges": 99999,
            "nodes_requested": 100000, "offline_stats": {"nodes": 100000, "edges": 99999},
            "graph_sample_max_rows": 20, "default_workspace_without_view_flag": True,
            "all_child_processes_stopped": True, "temporary_credentials_and_storage_removed_on_context_exit": True,
            "captures": list(CAPTURES), "languages": ["en", "pt-BR"], "dimensions": [[120, 30], [80, 24]]}
        for capture in CAPTURES:
            (directory / (capture + ".png")).write_bytes(b"\x89PNG\r\n\x1a\n")
            (directory / (capture + ".txt")).write_text("synthetic self-check")
        validate_ui(ui, directory)
        (directory / (CAPTURES[0] + ".png")).unlink()
        try:
            validate_ui(ui, directory)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted missing capture")
    corruption = {**copy.deepcopy(base), "rows": 1000, "checkpoint_audit": {"status": "ok"},
        "corrupted_audit_exit_code": 1, "corrupted_error_class": "RocksDB Corruption: Bad table magic number",
        "neighboring_corruption_blocks_catalog_open": True, "ownedResourcesRemoved": True}
    validate("checkpoint-corruption.json", corruption)
    for field, bad in (("corrupted_audit_exit_code", 0), ("corrupted_error_class", ""), ("corrupted_error_class", "PermissionDenied"), ("checkpoint_audit", {"status": "failed"}), ("neighboring_corruption_blocks_catalog_open", False), ("ownedResourcesRemoved", False)):
        changed = copy.deepcopy(corruption); changed[field] = bad
        try:
            validate("checkpoint-corruption.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid corruption {field}")
    multigraph = {**copy.deepcopy(base), "graphs": ["graph_a", "graph_b", "graph_c"], "rows_per_graph": 100000,
        "ownedResourcesRemoved": True, "childServerStopped": True, "privateCredentialsAndStorageRemoved": True,
        "kill_exit_code": -9}
    profiles = {
        "audits": {"status": "ok"}, "offline_counts": {"nodes": 100000},
        "before_restart": {"nodes": 100000, "all_active": True, "foreign_graph_email_rows": 0},
        "after_restart": {"nodes": 100000, "all_active": True, "foreign_graph_email_rows": 0},
        "concurrent_writers": {"initial_rows": 100000, "overwritten_rows": 100000, "acknowledged_batches": 800},
        "concurrent_readers": {"read_cycles": 1, "foreign_rows": 0}, "tui_readonly_snapshots": {}}
    for key, profile in profiles.items():
        multigraph[key] = {graph: copy.deepcopy(profile) for graph in multigraph["graphs"]}
    validate("multigraph-100000.json", multigraph)
    for field, bad in (("rows_per_graph", 1000), ("graphs", ["graph_a"]), ("privateCredentialsAndStorageRemoved", False)):
        changed = copy.deepcopy(multigraph); changed[field] = bad
        try:
            validate("multigraph-100000.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid multigraph {field}")
    changed = copy.deepcopy(multigraph); changed["audits"]["graph_b"]["status"] = "failed"
    try:
        validate("multigraph-100000.json", changed)
    except ValueError:
        pass
    else:
        raise AssertionError("accepted failed graph audit")
    text = "20 outgoing rows / 20 MORE AVAILABLE"
    typed = {**copy.deepcopy(base), "driver": "6.3.0", "nodes_requested": 100000, "edges_requested": 99999,
        "offline_stats": {"nodes": 100000, "edges": 99999},
        "ontology_sha256": "ee19d596b483e2a9861281b3433d2e2b18be0529f73a7616881e53fa64845a29",
        "ownedResourcesRemoved": True,
        "query": "MATCH (n:Ticket {entity_id:$id})-[:HAS_COMMENT]->(m:TicketComment) RETURN m.entity_id AS entity_id LIMIT 20",
        "snapshot": {"text": text, "sha256": hashlib.sha256(text.encode()).hexdigest(), "sample_rows": 20, "more_available": True}}
    for key, count in (("python_full_request", 1000), ("cli_process_snapshot", 20)):
        typed[key] = {"requests": count, "errors": 0, "min_us": 1, "p50_us": 2, "p95_us": 3, "p99_us": 4, "max_us": 5}
    validate("typed-neighborhood-100000.json", typed)
    for field, bad in (("nodes_requested", 1000), ("edges_requested", 999), ("snapshot", {})):
        changed = copy.deepcopy(typed); changed[field] = bad
        try:
            validate("typed-neighborhood-100000.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted invalid typed profile {field}")
    for key in ("python_full_request", "cli_process_snapshot"):
        changed = copy.deepcopy(typed); changed[key]["requests"] -= 1
        try:
            validate("typed-neighborhood-100000.json", changed)
        except ValueError:
            pass
        else:
            raise AssertionError(f"accepted insufficient typed iterations {key}")
    with tempfile.TemporaryDirectory() as folder:
        directory = Path(folder)
        data = b"synthetic image bytes for proof-binding self-check"
        proof = {"result": "PASS", "binary_sha256": NATIVE_SHA, "pillow": "self-check", "capture_count": 13, "checks": {}}
        for name in CAPTURES:
            (directory / (name + ".png")).write_bytes(data)
            width, height = (880, 576) if "80x24" in name else (1320, 720)
            proof["checks"][name + ".png"] = {"sha256": hashlib.sha256(data).hexdigest(), "width": width, "height": height, "fully_decoded": True}
        validate_image_proof(proof, directory)
        name = CAPTURES[0] + ".png"
        for field, bad in (("fully_decoded", False), ("sha256", "0" * 64), ("width", 1)):
            changed = copy.deepcopy(proof); changed["checks"][name][field] = bad
            try:
                validate_image_proof(changed, directory)
            except ValueError:
                pass
            else:
                raise AssertionError(f"accepted invalid PNG proof {field}")
        changed = copy.deepcopy(proof); del changed["checks"][name]
        try:
            validate_image_proof(changed, directory)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted missing PNG file proof")
    require(not number(True, 0) and not number(float("nan"), 0), "invalid numeric acceptance")
    return {"self_check": "PASS", "scope": "collector negative checks only; no acceptance result generated"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        print(json.dumps(self_check(), indent=2)); return
    directory = Path(__file__).resolve().parent
    ui = directory.parents[1] / "design/assets/2026-10-13-beta"
    output = directory / "validation.json"
    # Remove a previous verdict before checking so failed reruns cannot leave stale PASS evidence.
    output.unlink(missing_ok=True)
    try:
        report = collect(directory, ui)
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        print(f"Evidence incomplete or invalid: {error}", file=sys.stderr)
        raise SystemExit(1) from error
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=directory, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(report, stream, indent=2); stream.write("\n")
    try:
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
