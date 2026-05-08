"""Summary grid for the external-lib-sweep report.

One Pareto subplot per workload (HR % vs ns/op, lower-right is better),
focused on senba vs mini-moka. orig and moka are excluded.

Run via:
    uv run --project scripts python scripts/plot_summary_grid.py
"""

from __future__ import annotations

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "docs/reports/data/2026-05-08-external-lib-sweep"
OUT = DATA / "summary-pareto.png"

WORKLOADS = [
    ("oltp-extended.csv", "OLTP (DB)"),
    ("mergep.csv", "MergeP (workstation merge)"),
    ("concat.csv", "ConCat (workstation cat)"),
    ("ds1.csv", "DS1 (ERP, head 10M)"),
    ("p3.csv", "P3 (workstation)"),
    ("s3-small.csv", "S3 (search engine)"),
    ("zipf-skew1.csv", "Zipf skew=1.0"),
]

# senba vs mini-moka (single-thread fairness): orig / mini_moka_sync / moka を除く
# - orig は SIEVE oracle で同 policy なので議論を分けるとき以外不要
# - mini_moka_sync は `sync()` 込みで multi-thread 用途の overhead が乗る、
#   single-thread 比較では unsync 版だけが意味ある W-TinyLFU baseline
# - moka 0.12 は background thread / tokio runtime が乗る、これも別軸
VARIANTS = ["senba", "mini_moka_unsync"]
COLORS = {"senba": "#1f77b4", "mini_moka_unsync": "#ff7f0e"}
MARKERS = {"senba": "o", "mini_moka_unsync": "s"}


def load(name: str) -> pd.DataFrame:
    df = pd.read_csv(DATA / name)
    df["accesses"] = df["hits"] + df["misses"]
    df["hit_ratio"] = df["hits"] / df["accesses"] * 100
    df["ns_per_op"] = df["elapsed_ns"] / df["accesses"]
    return df


def main() -> None:
    sns.set_theme(style="whitegrid", context="talk")
    n = len(WORKLOADS)
    cols = 3
    rows = (n + cols - 1) // cols
    fig, axes = plt.subplots(rows, cols, figsize=(6.5 * cols, 5.0 * rows))
    axes = axes.flatten()

    for ax, (csv, title) in zip(axes, WORKLOADS):
        df = load(csv)
        df = df[df["variant"].isin(VARIANTS)]
        for v in VARIANTS:
            sub = df[df["variant"] == v].sort_values("capacity")
            if sub.empty:
                continue
            ax.plot(
                sub["hit_ratio"],
                sub["ns_per_op"],
                marker=MARKERS[v],
                color=COLORS[v],
                markersize=10,
                linewidth=2,
                label=v,
            )
            for _, r in sub.iterrows():
                cap = int(r["capacity"])
                lab = f"{cap // 1000}k" if cap >= 1000 else str(cap)
                ax.annotate(
                    lab,
                    (r["hit_ratio"], r["ns_per_op"]),
                    textcoords="offset points",
                    xytext=(6, 4),
                    fontsize=8,
                    color=COLORS[v],
                )
        ax.set_yscale("log")
        ax.set_xlabel("hit ratio (%)")
        ax.set_ylabel("ns / op (log, lower is better)")
        ax.set_title(title)
        ax.grid(True, which="both", alpha=0.35)
        ax.legend(fontsize=10, loc="best")

    for ax in axes[n:]:
        ax.set_visible(False)

    fig.suptitle(
        "senba vs mini-moka — HR vs ns/op (lower-right = better)",
        fontsize=18,
        y=1.005,
    )
    fig.tight_layout()
    fig.savefig(OUT, dpi=140, bbox_inches="tight")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
