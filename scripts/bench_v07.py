"""Run the existing synthetic engine benchmark and preserve exact receipts.

Build the baseline executable first. This script never builds while measuring.
It uses a new output directory and creates its disposable graph beneath it.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import time

GIB = 1 << 30
GAPS = [
    "No hard process-RSS budget is enforced; the optional RSS watchdog samples and can overshoot.",
    "The existing baseline read cursor uses pages of 1000, not the requested ACL cursor pages of 200.",
    "Delegated/restricted ACL reads, Bolt transfer, and concurrent load plus reads are not measured by this engine harness.",
    "The baseline fixture fixes RocksDB cache at 1 GiB, memtables at 256 MiB, and per-query accounting at 2 GiB.",
    "Host load is recorded, but unrelated builds/tests are not detected automatically; run in an idle measurement window.",
]


def command_output(args):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=5)
        return {"exit_code": result.returncode, "stdout": result.stdout.strip(),
                "stderr": result.stderr.strip()}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"error": str(error)}


def host_receipt():
    host = {"system": platform.system(), "release": platform.release(),
            "machine": platform.machine(), "logical_cpus": os.cpu_count(),
            "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
            "power_qualified": False, "power_reason": "AC power and low-power state were not verified"}
    if platform.system() == "Darwin":
        host["power_source"] = command_output(["pmset", "-g", "batt"])
        host["power_settings"] = command_output(["pmset", "-g"])
        host["power_profiles"] = command_output(["pmset", "-g", "custom"])
        source = host["power_source"].get("stdout", "")
        settings = host["power_settings"].get("stdout", "")
        modes = [parts[1] for line in settings.splitlines()
                 if len(parts := line.split()) == 2 and parts[0] == "lowpowermode"]
        host["power_qualified"] = (host["power_source"].get("exit_code") == 0
                                   and host["power_settings"].get("exit_code") == 0
                                   and "'AC Power'" in source and modes == ["0"])
        host["power_reason"] = ("AC power and active low-power mode disabled" if host["power_qualified"]
                                else "AC power or active low-power-off was not confirmed")
    return host


def executable_receipt(path):
    path = Path(path).resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError("baseline must be an executable file")
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return {"path": str(path), "sha256": digest.hexdigest(), "bytes": path.stat().st_size,
            "version": None, "version_reason": "baseline executable has no version command"}


def read_rss(pid):
    result = command_output(["ps", "-o", "rss=", "-p", str(pid)])
    value = result.get("stdout", "").strip()
    return int(value) * 1024 if result.get("exit_code") == 0 and value.isdigit() else None


def stop(process):
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def run_phase(name, args, output, timeout, rss_watchdog_bytes, sample_seconds):
    stdout_path, stderr_path = output / (name + ".jsonl"), output / (name + ".stderr.log")
    result = {"name": name, "command": [str(arg) for arg in args],
              "stdout_file": stdout_path.name, "stderr_file": stderr_path.name,
              "rss_scope": "direct benchmark process, sampled ps RSS; threads included, child processes excluded",
              "rss_sample_interval_s": sample_seconds, "rss_peak_bytes": None, "rss_samples": 0,
              "rss_watchdog_bytes": rss_watchdog_bytes, "hard_rss_limit_enforced": False}
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    env["BENCH_READ_FIRST"] = "1"
    started = time.monotonic()
    with stdout_path.open("x") as stdout, stderr_path.open("x") as stderr:
        process = subprocess.Popen([str(arg) for arg in args], stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=stderr, env=env, start_new_session=True)
        try:
            while process.poll() is None:
                rss = read_rss(process.pid)
                if rss is not None:
                    result["rss_samples"] += 1
                    result["rss_peak_bytes"] = max(result["rss_peak_bytes"] or 0, rss)
                    if rss_watchdog_bytes and rss > rss_watchdog_bytes:
                        result["stopped_reason"] = "sampled RSS exceeded watchdog threshold"
                        stop(process)
                        break
                if time.monotonic() - started > timeout:
                    result["stopped_reason"] = "phase timeout"
                    stop(process)
                    break
                time.sleep(sample_seconds)
            result["exit_code"] = process.wait()
        finally:
            stop(process)
    result["seconds"] = time.monotonic() - started
    records, invalid = [], []
    for line in stdout_path.read_text().splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            invalid.append(line[:200])
        else:
            if isinstance(record, dict):
                records.append(record)
            else:
                invalid.append(line[:200])
    result["records"] = records
    result["status"] = "ok" if result["exit_code"] == 0 and records and not invalid and "stopped_reason" not in result else "failed"
    if invalid:
        result["unexpected_stdout_lines"] = invalid[:10]
    if name == "check" and not any(row.get("phase") == "check" and row.get("status") == "ok" for row in records):
        result["status"] = "failed"
        result["validation_error"] = "baseline check did not report status=ok"
    return result


def run(args):
    binary = executable_receipt(args.baseline)
    args.baseline = Path(binary["path"])
    host = host_receipt()
    if not host["power_qualified"] and not args.allow_unqualified_host:
        raise ValueError(host["power_reason"] + "; use --allow-unqualified-host only for an explicitly unqualified diagnostic")
    output = Path(args.output).resolve()
    # Never load an existing graph, overwrite receipts, or attach to a running server.
    output.mkdir(parents=True, exist_ok=False)
    database = output / "data" / "bench"
    database.parent.mkdir()
    phases = [
        ("load", [args.baseline, "load", database, args.rows_per_label, args.load_sessions]),
        ("read", [args.baseline, "read", database]),
        ("write", [args.baseline, "write", database, args.write_rows, *args.sessions]),
        ("check", [args.baseline, "check", database]),
    ]
    receipt = {"format_version": 1, "kind": "embedded synthetic benchmark", "status": "running",
               "baseline_executable": binary, "source_revision": args.source_revision,
               "source_revision_provenance": "caller supplied; binary-source binding not independently verified",
               "host_before": host,
               "dataset": {"rows_per_label": args.rows_per_label,
                           "fixture_nodes": 4 * args.rows_per_label, "fixture_edges": 4 * args.rows_per_label,
                           "labels": ["Usuario", "Carro", "Endereco", "Casa"],
                           "load_sessions": args.load_sessions, "write_rows_per_run": args.write_rows,
                           "write_sessions": args.sessions, "write_read_first": True,
                           "note": "write phase adds Pessoa nodes; fixture counts describe load phase only"},
               "memory": {"hard_rss_limit_enforced": False, "rss_watchdog_bytes": args.rss_watchdog_bytes,
                          "query_budget_bytes": 2 * GIB, "rocksdb_cache_bytes": GIB,
                          "memtable_bytes": 256 << 20},
               "qualification_gaps": GAPS, "phases": []}
    path = output / "receipt.json"
    def save():
        path.write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        for name, command in phases:
            phase = run_phase(name, command, output, args.phase_timeout, args.rss_watchdog_bytes, args.sample_seconds)
            receipt["phases"].append(phase)
            save()
            if phase["status"] != "ok":
                receipt["status"] = "failed"
                break
        else:
            receipt["status"] = "completed_embedded_only"
    except BaseException as error:
        receipt["status"] = "interrupted" if isinstance(error, KeyboardInterrupt) else "failed"
        receipt["error"] = type(error).__name__ + ": " + str(error)
        raise
    finally:
        receipt["host_after"] = host_receipt()
        peaks = [phase["rss_peak_bytes"] for phase in receipt["phases"] if phase["rss_peak_bytes"] is not None]
        receipt["rss_peak_bytes"] = max(peaks) if peaks else None
        save()
    print(path)
    return 0 if receipt["status"] == "completed_embedded_only" else 1


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--baseline", type=Path, required=True, help="already-built baseline bench executable")
    result.add_argument("--output", type=Path, required=True, help="new directory; reuse is refused")
    result.add_argument("--source-revision", required=True, help="exact source revision used to build the executable")
    result.add_argument("--rows-per-label", type=int, default=1_000_000)
    result.add_argument("--load-sessions", type=int, default=8)
    result.add_argument("--write-rows", type=int, default=200_000)
    result.add_argument("--sessions", type=int, nargs="+", default=[1, 4, 8, 16])
    result.add_argument("--phase-timeout", type=float, default=3600)
    result.add_argument("--sample-seconds", type=float, default=0.1)
    result.add_argument("--rss-watchdog-bytes", type=int, default=4 * GIB,
                        help="sampled stop threshold, not an enforced RSS ceiling; 0 disables")
    result.add_argument("--allow-unqualified-host", action="store_true")
    return result


def main():
    args = parser().parse_args()
    if args.rows_per_label < 1 or args.write_rows < 1 or args.load_sessions < 1 or any(n < 1 for n in args.sessions):
        raise SystemExit("rows and session counts must be positive")
    if args.phase_timeout <= 0 or not 0.01 <= args.sample_seconds <= 5 or args.rss_watchdog_bytes < 0:
        raise SystemExit("invalid timeout, sampling interval or RSS watchdog")
    try:
        return run(args)
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    raise SystemExit(main())
