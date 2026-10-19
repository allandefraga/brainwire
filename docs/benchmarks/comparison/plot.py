#!/usr/bin/env python3
"""Render a compact SVG from one comparison run's raw JSON receipt."""
import argparse
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
COLORS = {"brainwire": "#36d399", "neo4j": "#67a9ff", "falkordb": "#c49bff"}
NAMES = {"brainwire": "Brainwire", "neo4j": "Neo4j", "falkordb": "FalkorDB"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("receipt", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, default=HERE / "comparison.svg")
    args = parser.parse_args()
    data = json.loads(args.receipt.read_text())
    runs = {r["system"]: r for r in data["results"]}
    systems = [s for s in ("brainwire", "neo4j", "falkordb") if s in runs]
    if len(systems) != 3:
        raise SystemExit("receipt must contain Brainwire, Neo4j and FalkorDB")
    params = data["parameters"]
    nodes = params["nodes"]
    edges = nodes * 2
    repeats = params["repeats"]
    batch = params["batch"]
    update_batches = params["update_batches"]

    parts = [f'''<svg xmlns="http://www.w3.org/2000/svg" width="1120" height="760" viewBox="0 0 1120 760" role="img" aria-labelledby="title desc">
<title id="title">Local graph database comparison</title><desc id="desc">Synthetic workload comparison. Lower latency is better; higher ingestion throughput is better.</desc>
<rect width="1120" height="760" rx="20" fill="#0b1220"/><text x="44" y="56" fill="#f2f6fc" font-family="system-ui,sans-serif" font-size="25" font-weight="700">Same synthetic graph · local Docker run</text>
<text x="44" y="84" fill="#9eacc0" font-family="system-ui,sans-serif" font-size="14">{nodes:,} nodes · {edges:,} directed edges · 4 CPU / 8 GiB cap · {repeats} read/update repeats</text>
<text x="44" y="131" fill="#d9e5f5" font-family="system-ui,sans-serif" font-size="17" font-weight="700">Read latency · p95 (ms) · lower is better</text>''']
    latency = [("Indexed point read", "point_read"), ("1-hop, LIMIT 20", "neighborhood_1hop_limit20")]
    max_ms = max(runs[s][key]["p95_ms"] for s in systems for _, key in latency) * 1.15
    x0, chart_w = 270, 790
    for row, (label, metric) in enumerate(latency):
        y = 178 + row * 88
        parts.append(f'<text x="44" y="{y+20}" fill="#d1d8e2" font-family="system-ui,sans-serif" font-size="15">{label}</text>')
        for i, system in enumerate(systems):
            val = runs[system][metric]["p95_ms"]
            bar_y = y + i * 21
            width = val / max_ms * chart_w
            parts.append(f'<rect x="{x0}" y="{bar_y}" width="{width:.1f}" height="14" rx="5" fill="{COLORS[system]}"/>')
            parts.append(f'<text x="{x0+width+9:.1f}" y="{bar_y+12}" fill="#e8eef7" font-family="ui-monospace,monospace" font-size="12">{NAMES[system]}  {val:g}</text>')
    parts.append('<text x="44" y="379" fill="#d9e5f5" font-family="system-ui,sans-serif" font-size="17" font-weight="700">Update series maximum · ms · lower is better</text>')
    update_maxima = {s: max(runs[s]["timing_samples_ms"]["update_batch_series"]) for s in systems}
    max_update = max(update_maxima.values()) * 1.16
    for i, system in enumerate(systems):
        y = 397 + i * 26
        val = update_maxima[system]
        width = val / max_update * 760
        parts.append(f'<text x="44" y="{y+14}" fill="#d1d8e2" font-family="system-ui,sans-serif" font-size="13">{NAMES[system]}</text>')
        parts.append(f'<rect x="180" y="{y}" width="{width:.1f}" height="18" rx="5" fill="{COLORS[system]}"/>')
        parts.append(f'<text x="{188+width:.1f}" y="{y+14}" fill="#e8eef7" font-family="ui-monospace,monospace" font-size="12">{val:g}</text>')
    parts.append('<line x1="44" y1="490" x2="1076" y2="490" stroke="#263448"/><text x="44" y="530" fill="#d9e5f5" font-family="system-ui,sans-serif" font-size="17" font-weight="700">Fresh ingest throughput · nodes + edges / second · higher is better</text>')
    max_rate = max(runs[s]["ingest_records_per_second"] for s in systems) * 1.26
    for i, system in enumerate(systems):
        y = 553 + i * 36
        val = runs[system]["ingest_records_per_second"]
        width = val / max_rate * 770
        parts.append(f'<text x="44" y="{y+17}" fill="#d1d8e2" font-family="system-ui,sans-serif" font-size="14">{NAMES[system]}</text>')
        parts.append(f'<rect x="180" y="{y}" width="{width:.1f}" height="23" rx="6" fill="{COLORS[system]}"/>')
        parts.append(f'<text x="{188+width:.1f}" y="{y+17}" fill="#e8eef7" font-family="ui-monospace,monospace" font-size="12">{val:,.0f}</text>')
    parts.append(f'<text x="44" y="690" fill="#8d9bb0" font-family="system-ui,sans-serif" font-size="12">Diagnostic, non-audited run. Update series: {update_batches} × {batch}-row batches per sample.</text><text x="44" y="712" fill="#8d9bb0" font-family="system-ui,sans-serif" font-size="12">Driver protocols and durability defaults differ; see comparison README and raw receipt.</text></svg>')
    args.output.write_text("\n".join(parts) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
