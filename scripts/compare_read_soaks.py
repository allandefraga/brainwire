"""Compare RSS time windows from two completed bench_v09_container receipts.

Only allowlisted numeric diagnostics and image digests are printed. This is a
descriptive comparison of local synthetic runs, not a leak or capacity test.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import statistics


ROLES = {"scan", "aggregation", "neighborhood", "cursor"}
COUNTS = {"Usuario", "Carro", "Endereco", "Casa", "TEM_CARRO", "TEM_ENDERECO",
          "TEM_CASA", "CASA_FICA_EM"}
MAX_RECEIPT_BYTES = 64 << 20
SETTINGS = {"batch_size", "neighborhood_rows", "sample_seconds", "reader_pause", "query_timeout"}


def _integer(value, *, positive=False):
    if type(value) is not int or not (1 if positive else 0) <= value < (1 << 64):
        raise ValueError("invalid integer diagnostic")
    return value


def _seconds(value):
    if type(value) not in (int, float) or value < 0:
        raise ValueError("invalid time diagnostic")
    try:
        finite = math.isfinite(value)
    except OverflowError:
        finite = False
    if not finite:
        raise ValueError("invalid time diagnostic")
    return value


def _mapping(value):
    if not isinstance(value, dict):
        raise ValueError("invalid structured diagnostic")
    return value


def _settings(report):
    recorded = _mapping(report.get("workload_settings", {}))
    values = {}
    for name in SETTINGS & recorded.keys():
        value = recorded[name]
        if name in ("batch_size", "neighborhood_rows"):
            values[name] = _integer(value, positive=True)
        else:
            values[name] = _seconds(value)
            if value == 0:
                raise ValueError("invalid workload setting")
    return values


def _difference(last, first):
    return None if last is None or first is None else last - first


def _summarize(report, window_seconds):
    if not isinstance(report, dict) or report.get("status") != "passed-diagnostic":
        raise ValueError("receipt is not a completed passing diagnostic")
    if report.get("synthetic") is not True or _mapping(report.get("integrity")).get("status") != "ok":
        raise ValueError("receipt is missing synthetic integrity verification")
    if any(key in report for key in ("error_type", "cleanup_error_type", "client_threads_alive", "host_power_changed")):
        raise ValueError("receipt contains an incomplete or failed phase")
    rows = _integer(report.get("rows_per_label"), positive=True)
    memory_limit = _integer(report.get("memory_limit_bytes"), positive=True)
    writers = _integer(report.get("writers"), positive=True)
    counts, cursor, oom = report.get("counts"), report.get("cursor"), report.get("oom_evidence")
    if (not isinstance(counts, dict) or set(counts) != COUNTS
            or any(type(value) is not int or value != rows for value in counts.values())
            or type(report.get("ack_rows_per_label")) is not int or report["ack_rows_per_label"] != rows):
        raise ValueError("receipt graph counts were not fully verified")
    if (not isinstance(cursor, dict) or type(cursor.get("rows")) is not int or cursor["rows"] != rows
            or type(cursor.get("delegated")) is not bool):
        raise ValueError("receipt cursor verification is incomplete")
    page_size = _integer(cursor.get("page_size"), positive=True)
    if (not isinstance(oom, dict) or oom.get("observed") is not False
            or type(oom.get("kernel_log_verified")) is not bool
            or type(oom.get("restart_count")) is not int or oom["restart_count"] != 0):
        raise ValueError("receipt OOM or restart evidence is incomplete")
    if (not isinstance(report.get("readers"), list) or len(report["readers"]) != len(ROLES)
            or any(not isinstance(role, str) for role in report["readers"]) or set(report["readers"]) != ROLES):
        raise ValueError("receipt reader roles are incomplete")
    if not isinstance(report.get("driver"), str) or not report["driver"]:
        raise ValueError("receipt driver version is missing")
    results = report.get("client_results")
    if not isinstance(results, list) or any(not isinstance(item, dict) or "error_type" in item for item in results):
        raise ValueError("receipt client results are incomplete")
    for role in ROLES:
        matching = [item for item in results if item.get("role") == role]
        if len(matching) != 1 or _integer(_mapping(matching[0].get("latency")).get("requests")) < 1:
            raise ValueError("receipt has insufficient successful reads")
    duration = _seconds(report.get("read_soak_seconds"))
    if duration < 2 * window_seconds:
        raise ValueError("read soak is too short for disjoint windows")
    image = _mapping(report.get("image_identity")).get("image_id")
    if not isinstance(image, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise ValueError("receipt image identity is missing")
    if not isinstance(report.get("samples"), list):
        raise ValueError("receipt samples are missing")
    samples = []
    for sample in report["samples"]:
        if not isinstance(sample, dict):
            raise ValueError("invalid sample")
        if sample.get("phase") != "read-soak":
            continue
        timestamp = _seconds(sample.get("monotonic_seconds"))
        if samples and timestamp <= samples[-1][0]:
            raise ValueError("read samples must have strictly increasing timestamps")
        process = _mapping(sample.get("process"))
        rss = _integer(process.get("VmRSS_bytes"))
        anon = _integer(process["RssAnon_bytes"]) if "RssAnon_bytes" in process else None
        file = _integer(process["RssFile_bytes"]) if "RssFile_bytes" in process else None
        current = _integer(sample.get("current_bytes"))
        peak = _integer(sample.get("peak_bytes"))
        events = sample.get("events")
        if (sample.get("max_bytes") != memory_limit or current > memory_limit or peak > memory_limit
                or not isinstance(events, dict) or type(events.get("oom")) is not int or events["oom"] != 0
                or type(events.get("oom_kill")) is not int or events["oom_kill"] != 0):
            raise ValueError("read sample has invalid capacity or OOM evidence")
        samples.append((timestamp, rss, current, anon, file))
    if len(samples) < 6 or samples[-1][0] - samples[0][0] < 2 * window_seconds:
        raise ValueError("insufficient time coverage for disjoint windows")
    if samples[-1][0] - samples[0][0] > duration:
        raise ValueError("sample coverage exceeds the recorded read soak")
    gaps = [b[0] - a[0] for a, b in zip(samples, samples[1:])]
    coverage_tolerance = min(30, max(1, 2 * statistics.median(gaps)))
    if duration - (samples[-1][0] - samples[0][0]) > coverage_tolerance:
        raise ValueError("samples do not cover the recorded read soak")
    first = [s for s in samples if s[0] < samples[0][0] + window_seconds]
    last = [s for s in samples if s[0] > samples[-1][0] - window_seconds]

    def window(values):
        if len(values) < 3:
            raise ValueError("insufficient readings in a time window")
        def optional_median(index):
            readings = [s[index] for s in values]
            return statistics.median(readings) if all(value is not None for value in readings) else None
        return {"samples": len(values), "observed_span_seconds": values[-1][0] - values[0][0],
                "median_process_rss_bytes": statistics.median(s[1] for s in values),
                "median_cgroup_current_bytes": statistics.median(s[2] for s in values),
                "median_process_rss_anon_bytes": optional_median(3),
                "median_process_rss_file_bytes": optional_median(4)}

    first, last = window(first), window(last)
    workload = (rows, memory_limit, writers, page_size, cursor["delegated"], report.get("driver"))
    return workload, duration, {
        "image_id": image, "read_soak_seconds": duration,
        "sampled_span_seconds": samples[-1][0] - samples[0][0],
        "max_sample_gap_seconds": max(gaps),
        "coverage_tolerance_seconds": coverage_tolerance,
        "first_window": first, "last_window": last,
        "window_change_bytes": last["median_process_rss_bytes"] - first["median_process_rss_bytes"],
        "window_change_anon_bytes": _difference(last["median_process_rss_anon_bytes"], first["median_process_rss_anon_bytes"]),
        "window_change_file_bytes": _difference(last["median_process_rss_file_bytes"], first["median_process_rss_file_bytes"]),
        "sampled_peak_process_rss_bytes": max(s[1] for s in samples),
        "sampled_peak_cgroup_current_bytes": max(s[2] for s in samples),
        "kernel_log_verified": oom["kernel_log_verified"],
    }


def compare_reports(baseline, candidate, *, window_seconds=300):
    _seconds(window_seconds)
    if window_seconds == 0:
        raise ValueError("window must be positive")
    before_workload, before_duration, before = _summarize(baseline, window_seconds)
    after_workload, after_duration, after = _summarize(candidate, window_seconds)
    if before_workload != after_workload or abs(before_duration - after_duration) > max(10, .05 * min(before_duration, after_duration)):
        raise ValueError("recorded workload or soak duration differs")
    before_settings, after_settings = _settings(baseline), _settings(candidate)
    common = before_settings.keys() & after_settings.keys()
    if any(before_settings[name] != after_settings[name] for name in common):
        raise ValueError("recorded workload settings differ")
    return {
        "status": "descriptive-comparison", "window_seconds": window_seconds,
        "scope": "completed synthetic four-label container read soaks; recorded workload fields match",
        "baseline": before, "candidate": after,
        "difference_in_window_changes_bytes": after["window_change_bytes"] - before["window_change_bytes"],
        "difference_in_anon_window_changes_bytes": _difference(after["window_change_anon_bytes"], before["window_change_anon_bytes"]),
        "difference_in_file_window_changes_bytes": _difference(after["window_change_file_bytes"], before["window_change_file_bytes"]),
        "kernel_logs_verified_for_both": before["kernel_log_verified"] and after["kernel_log_verified"],
        "workload_settings": {
            "status": "complete-match" if common == SETTINGS else "partial-match" if common else "unavailable",
            "matched_values": {name: before_settings[name] for name in sorted(common)},
            "unavailable_fields": sorted(SETTINGS - common),
        },
        "limitations": [
            "A change in sampled RSS does not identify a leak or establish its cause.",
            "Process RSS and cgroup memory account for different things; neither is a global RSS guarantee.",
            "Missing RssAnon or RssFile readings produce null diagnostics; process file RSS is not cgroup page cache.",
            "Matching recorded fields does not prove identical hosts, workload settings or external load.",
            "Settings absent in either receipt remain unverified; older receipts omit several workload settings.",
            "Receipt validation does not independently verify image provenance or kernel logs.",
            "Reader success counts cover load and soak together; they do not prove successful reads in each window.",
            "Writes stop before the read soak; this is not sustained mixed-write qualification.",
            "This comparison does not establish production capacity or multi-machine recovery.",
        ],
    }


def _load(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(MAX_RECEIPT_BYTES + 1)
    if len(raw) > MAX_RECEIPT_BYTES:
        raise ValueError("receipt exceeds the local input size limit")
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline")
    parser.add_argument("candidate")
    parser.add_argument("--window-seconds", type=float, default=300)
    args = parser.parse_args(argv)
    try:
        before, before_hash = _load(args.baseline)
        after, after_hash = _load(args.candidate)
        result = compare_reports(before, after, window_seconds=args.window_seconds)
        result["baseline"]["receipt_sha256"] = before_hash
        result["candidate"]["receipt_sha256"] = after_hash
    except (OSError, ValueError, TypeError, KeyError, AttributeError, OverflowError, RecursionError):
        print(json.dumps({"status": "invalid", "error": "receipt validation failed"}))
        return 1
    print(json.dumps(result, indent=2, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
