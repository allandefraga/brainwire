"""Summarize recorded process percentiles; does not pool trial samples."""
import json
from pathlib import Path
from statistics import median

root = Path(__file__).resolve().parent
result = {}
for group, trials in [("initial", range(1, 6)), ("repeat", range(6, 16))]:
    result[group] = {}
    for fixture in ["empty", "payload", "large"]:
        data = {
            engine: [json.loads((root / f"{engine}-{fixture}-{n}.json").read_text())
                     for n in trials]
            for engine in ["before", "after"]
        }
        result[group][fixture] = {}
        for query in data["before"][0]["measurements"]:
            points = {engine: [d["measurements"][query] for d in documents]
                      for engine, documents in data.items()}
            summary = {
                f"{engine}{percentile}": median(p[percentile] for p in values)
                for engine, values in points.items()
                for percentile in ["p50Ms", "p95Ms"]
            }
            summary["pairedP50ReductionPercent"] = [
                100 * (1 - a["p50Ms"] / b["p50Ms"])
                for b, a in zip(points["before"], points["after"])
            ]
            result[group][fixture][query] = summary
(root / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
