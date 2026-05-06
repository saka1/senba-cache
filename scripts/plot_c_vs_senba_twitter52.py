"""Plot cachesim (C) vs senba (Rust) MQPS on twitter_cluster52.

Reads:
  docs/reports/data/2026-05-06-sieve-c-vs-senba-twitter52.csv
  docs/reports/data/2026-05-06-sieve-c-vs-senba-twitter52-oraclegen.csv

Writes:
  docs/figures/c_vs_senba_twitter52_mqps.png
"""
from pathlib import Path
import csv
import statistics

import matplotlib.pyplot as plt
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
CSV_MAIN = ROOT / "docs/reports/data/2026-05-06-sieve-c-vs-senba-twitter52.csv"
CSV_BIN = ROOT / "docs/reports/data/2026-05-06-sieve-c-vs-senba-twitter52-oraclegen.csv"
OUT = ROOT / "docs/figures/c_vs_senba_twitter52_mqps.png"

CAPS = [144, 1435, 14354]


def load(path):
    rows = []
    with path.open() as f:
        for r in csv.DictReader(f):
            rows.append(r)
    return rows


def median_mqps(rows, tool, variant, cap):
    vals = [
        float(r["mqps"])
        for r in rows
        if r["tool"] == tool and r["variant"] == variant and int(r["capacity"]) == cap
    ]
    return statistics.median(vals) if vals else None


def main():
    main_rows = load(CSV_MAIN)
    bin_rows = load(CSV_BIN)

    senba_cache_variant = {144: "senba_n16", 1435: "senba_n32", 14354: "senba_n256"}

    series = {
        "cachesim CSV (C)": [median_mqps(main_rows, "cachesim", "Sieve", c) for c in CAPS],
        "cachesim oracleGeneral (C)": [median_mqps(bin_rows, "cachesim-bin", "Sieve", c) for c in CAPS],
        "senba sieve_orig (Rust)": [median_mqps(main_rows, "senba", "orig", c) for c in CAPS],
        "senba Cache (Rust)": [
            median_mqps(main_rows, "senba", senba_cache_variant[c], c) for c in CAPS
        ],
    }

    x = np.arange(len(CAPS))
    width = 0.2
    fig, ax = plt.subplots(figsize=(8, 4.5))
    colors = ["#bbbbbb", "#888888", "#1f77b4", "#ff7f0e"]
    for i, (label, vals) in enumerate(series.items()):
        offs = (i - 1.5) * width
        bars = ax.bar(x + offs, vals, width, label=label, color=colors[i])
        for b, v in zip(bars, vals):
            ax.text(
                b.get_x() + b.get_width() / 2,
                v,
                f"{v:.1f}",
                ha="center",
                va="bottom",
                fontsize=8,
            )

    ax.set_xticks(x)
    ax.set_xticklabels([f"{c}" for c in CAPS])
    ax.set_xlabel("capacity (objects)")
    ax.set_ylabel("MQPS (median of 5, taskset -c 0)")
    ax.set_title("twitter_cluster52: cachesim (C) vs senba (Rust) — single-thread wall-clock")
    ax.legend(loc="upper left", fontsize=9)
    ax.grid(axis="y", linestyle=":", alpha=0.5)

    fig.tight_layout()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT, dpi=140)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
