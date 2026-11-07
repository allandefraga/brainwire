"""Plot actual one-hour samples and latency receipts; requires matplotlib.

This is a synthetic localhost observation, not an RSS bound or production SLO.
"""
import hashlib
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

DIRECTORY = Path(__file__).resolve().parent


def main():
    source = DIRECTORY / "mixed-100000-3600.json"
    receipt = json.loads(source.read_text())
    assert receipt["result"] == "PASS" and receipt["load_seconds"] >= 3600
    observations = receipt["observations"]
    assert observations and observations[-1]["seconds"] >= 3590
    minutes = [value["seconds"] / 60 for value in observations]
    with plt.rc_context({"font.size": 10, "axes.spines.top": False,
                         "axes.spines.right": False, "figure.facecolor": "white"}):
        figure, axes = plt.subplots(3, 1, figsize=(10, 9), constrained_layout=True)
        axes[0].plot(minutes, [value["rss_kib"] / 1024 for value in observations], color="#22883b", linewidth=1.3)
        axes[0].set(title="Native server RSS during one continuous hour", ylabel="RSS (MiB)", xlabel="Elapsed load (minutes)")
        axes[0].grid(alpha=.18)
        axes[1].plot(minutes, [value["poolReservedBytes"] / 1024 for value in observations], color="#008c99", label="Current accounted reservations")
        axes[1].plot(minutes, [value["poolPeakBytes"] / 1024 for value in observations], color="#6250a2", linestyle="--", label="Process-lifetime accounted peak (includes import)")
        axes[1].set(title="Shared query-pool accounting; excludes cache and total process RAM", ylabel="Accounted (KiB)", xlabel="Elapsed load (minutes)")
        axes[1].grid(alpha=.18)
        axes[1].legend(fontsize=8)
        kinds = ("point", "scalar", "writes")
        for offset, metric, color in ((-.2, "p95_ms", "#22883b"), (.2, "p99_ms", "#008c99")):
            axes[2].bar([index + offset for index in range(3)], [receipt[kind][metric] for kind in kinds], width=.36, label=metric[:3], color=color)
        axes[2].set(xticks=range(3), xticklabels=["Hot primary lookup", "Scalar-index lookup", "Indexed email update"], ylabel="Full Python/Bolt request (ms)", title="Four readers + one deliberately throttled writer")
        axes[2].grid(axis="y", alpha=.18)
        axes[2].legend(fontsize=8)
        figure.suptitle("Brainwire · synthetic 100,000-Customer qualification", fontsize=15, fontweight="bold")
        figure.supxlabel("Local native process; RSS sampled about every 5 seconds; queries return IDs/email only.\nHistory, search and replication disabled. Host also ran qualification tasks; no speedup or production SLO claim.", fontsize=8)
        output = DIRECTORY / "one-hour-observations.png"
        figure.savefig(output, dpi=160)
        plt.close(figure)
    metadata = {"source_receipt": source.name, "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                "native_binary_sha256": receipt["binary_sha256"], "matplotlib": matplotlib.__version__,
                "output": output.name, "output_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
                "scope": "plot of actual synthetic localhost receipt; no additional qualification"}
    (DIRECTORY / "plot.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
