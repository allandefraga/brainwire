"""Summarize three comparable 1M-node run.py receipts and draw an ingest chart.

Read latency is the median of the three p95 values calculated by run.py, not
an aggregate p95 across requests. The receipts do not store the raw requests.
"""
import argparse
import json
from math import isfinite
from pathlib import Path
from statistics import median


SYSTEMS = {"brainwire": "Brainwire", "falkordb": "FalkorDB", "neo4j": "Neo4j"}
METRICS = (
    ("Create nodes", "node_create", "seconds", "s"),
    ("Create edges", "edge_create", "seconds", "s"),
    ("Update rows", "property_update", "seconds", "s"),
    ("Point p95", "indexed_point_read", "p95_ms", "ms"),
    ("One-hop p95", "outgoing_one_hop", "p95_ms", "ms"),
    ("Ready after SIGKILL", "sigkill_recovery", "ready_seconds", "s"),
)
COMPARABLE_FIELDS = ("parameters", "runner_sha256", "fixture", "host", "docker",
                     "clients", "configured_engine_memory_mib", "durability_scope")


def summarize(receipts):
    if len(receipts) != 3:
        raise ValueError("exactly three receipts are required")
    first = receipts[0]
    run_ids = set()
    timings = {system: [] for system in SYSTEMS}
    engine_settings = {}
    for i, receipt in enumerate(receipts, 1):
        if receipt["schema_version"] != 1 or receipt["status"] != "passed":
            raise ValueError(f"receipt {i} must be a passed schema-version-1 run")
        run_id = receipt["run_id"]
        if not isinstance(run_id, str) or not run_id or run_id in run_ids:
            raise ValueError(f"receipt {i} must have a distinct run_id")
        run_ids.add(run_id)
        params = receipt["parameters"]
        if params["nodes"] != 1_000_000:
            raise ValueError(f"receipt {i} must contain 1,000,000 nodes")
        for field in COMPARABLE_FIELDS:
            if receipt[field] != first[field]:
                raise ValueError(f"receipt {i} has different {field}")
        results = {result["system"]: result for result in receipt["results"]}
        if results.keys() != SYSTEMS.keys() or len(receipt["results"]) != len(SYSTEMS):
            raise ValueError(f"receipt {i} must record each of the three systems once")
        updates = min(params["nodes"], params["batch"] * params["update_batches"])
        for system, result in results.items():
            for field in ("runtime_settings", "durability"):
                key = (system, field)
                if result[field] != engine_settings.setdefault(key, result[field]):
                    raise ValueError(f"receipt {i} has different {system} {field}")
            image_id = receipt["images"][system]["id"]
            if (not isinstance(image_id, str) or not image_id
                    or image_id != first["images"][system]["id"]
                    or image_id != result["image"]["id"]):
                raise ValueError(f"receipt {i} has a different {system} image ID")
            if (result["node_create"]["records"] != params["nodes"]
                    or result["edge_create"]["records"] != params["nodes"] * 2
                    or result["property_update"]["records"] != updates
                    or result["indexed_point_read"]["count"] != params["reads"]
                    or result["outgoing_one_hop"]["count"] != params["reads"]):
                raise ValueError(f"receipt {i} has inconsistent {system} workload counts")
            recovery = result["sigkill_recovery"]
            if (recovery["nodes"] != params["nodes"]
                    or recovery["edges"] != params["nodes"] * 2
                    or recovery["verified_update_rows"] != min(updates, 2)):
                raise ValueError(f"receipt {i} has inconsistent {system} recovery counts")
            values = [result[section][field] for _, section, field, _ in METRICS]
            if any(type(value) not in (int, float) or not isfinite(value) or value < 0 for value in values):
                raise ValueError(f"receipt {i} has invalid {system} measurements")
            timings[system].append(values)
    return {system: [median(samples) for samples in zip(*runs)] for system, runs in timings.items()}


def markdown(medians):
    header = "| Engine | " + " | ".join(label for label, _, _, _ in METRICS) + " |"
    rows = [header, "| --- |" + " ---: |" * len(METRICS)]
    for system, values in medians.items():
        formatted = [f"{value:.3f} {metric[3]}" for value, metric in zip(values, METRICS)]
        rows.append(f"| {SYSTEMS[system]} | " + " | ".join(formatted) + " |")
    rows.append("\nRead values are medians of per-run p95s, not pooled request percentiles.")
    return "\n".join(rows)


def svg(medians):
    """Draw each ingestion stage independently because medians need not add."""
    largest = max(max(values[:2]) for values in medians.values()) or 1
    description = " ".join(f"{SYSTEMS[system]}: {values[0]:.2f} seconds for nodes and "
                           f"{values[1]:.2f} for edges." for system, values in medians.items())
    lines = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="860" height="310" viewBox="0 0 860 310"'
        ' role="img" aria-labelledby="title desc">',
        '<title id="title">Creating 1M nodes and 2M edges</title>',
        f'<desc id="desc">Median creation times across three synthetic runs. {description}</desc>',
        '<rect width="860" height="310" rx="12" fill="#f3f5fa"/>',
        '<g font-family="system-ui, sans-serif" fill="#142137">',
        '<text x="28" y="39" font-weight="700" font-size="23">Creating 1M nodes and 2M edges</text>',
        '<text x="28" y="64" fill="#45536a" font-size="14">Median times across three synthetic runs</text>',
        '<rect x="28" y="83" width="14" height="14" rx="2" fill="#2349d8"/>',
        '<text x="50" y="95" font-size="13">Node creation</text>',
        '<rect x="165" y="83" width="14" height="14" rx="2" fill="#176752"/>',
        '<text x="187" y="95" font-size="13">Edge creation</text>',
        '<path d="M165 111v169" stroke="#c5cdde"/>',
    ]
    for i, (system, values) in enumerate(medians.items()):
        y = 120 + i * 57
        lines.append(f'<text x="28" y="{y+21}" font-size="15" font-weight="600">{SYSTEMS[system]}</text>')
        for j, (value, color) in enumerate(zip(values, ("#2349d8", "#176752"))):
            width = 590 * (value / largest)
            top = y + j * 19
            lines.append(f'<rect x="165" y="{top}" width="{width:.1f}" height="15" fill="{color}"/>')
            lines.append(f'<text x="{173+width:.1f}" y="{top+12}" font-size="12">{value:.2f} s</text>')
    return "\n".join(lines + ["</g>", "</svg>", ""])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipts", nargs=3, type=Path)
    parser.add_argument("--svg", required=True, type=Path, help="output SVG path")
    args = parser.parse_args()
    try:
        if args.svg.exists() and any(args.svg.samefile(path) for path in args.receipts):
            raise ValueError("the SVG output must differ from the input receipts")
        receipts = [json.loads(path.read_text(encoding="utf-8")) for path in args.receipts]
        medians = summarize(receipts)
        args.svg.write_text(svg(medians), encoding="utf-8")
    except (KeyError, OSError, OverflowError, TypeError, ValueError) as error:
        parser.error(str(error))
    print(markdown(medians))


if __name__ == "__main__":
    main()
