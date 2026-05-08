"""Pareto plot for `bench` CSV (external library comparison harness).

Consumes the CSV emitted by `research/src/bin/bench.rs` (header:
``variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions``)
and produces one figure per workload group, with each variant as a series and
each capacity as a point joined by a line in capacity-sweep order.

Default axes: X = hit ratio (%), Y = throughput (Mops/s) — upper-right is better.

Run via:
    uv run --project scripts python scripts/plot_pareto.py <csv> [--out-dir DIR] \\
        [--axes hr-vs-tp|cap-vs-hr|cap-vs-tp] [--title-prefix TEXT]

Workload grouping: (source, skew, keys). For file-based sources skew/keys are
typically NaN/0 and the whole CSV collapses to a single group, which is fine.
Pass --group-by to override.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("csv", type=Path, help="bench.rs stdout CSV")
    p.add_argument(
        "--out-dir",
        type=Path,
        default=Path("./pareto"),
        help="output directory for PNGs (created if missing)",
    )
    p.add_argument(
        "--axes",
        choices=["hr-vs-tp", "cap-vs-hr", "cap-vs-tp"],
        default="hr-vs-tp",
        help="X vs Y. hr-vs-tp = HR%% (X) vs Mops/s (Y); cap-vs-hr = capacity (X log) vs HR%%; cap-vs-tp = capacity (X log) vs Mops/s",
    )
    p.add_argument(
        "--group-by",
        default="source,skew,keys",
        help="comma-separated CSV columns that identify a workload (one figure per group)",
    )
    p.add_argument(
        "--title-prefix",
        default="",
        help="prefix prepended to each figure title (e.g. trace name)",
    )
    return p.parse_args()


def workload_label(group_keys: list[str], values: tuple) -> str:
    bits = []
    for k, v in zip(group_keys, values):
        if isinstance(v, float) and pd.isna(v):
            continue
        bits.append(f"{k}={v}")
    return ", ".join(bits) if bits else "all"


def main() -> int:
    args = parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    df = pd.read_csv(args.csv)
    required = {"variant", "capacity", "len", "elapsed_ns", "hits", "misses"}
    missing = required - set(df.columns)
    if missing:
        sys.exit(f"CSV missing required columns: {sorted(missing)}")

    df["accesses"] = df["hits"] + df["misses"]
    df["hit_ratio"] = df["hits"] / df["accesses"]
    df["mops_per_s"] = df["accesses"] * 1e3 / df["elapsed_ns"]  # ns→ms→Mops/s

    group_keys = [k.strip() for k in args.group_by.split(",") if k.strip()]
    missing_g = [k for k in group_keys if k not in df.columns]
    if missing_g:
        sys.exit(f"--group-by columns not in CSV: {missing_g}")

    sns.set_theme(style="whitegrid", context="talk")

    for values, sub in df.groupby(group_keys, dropna=False):
        if not isinstance(values, tuple):
            values = (values,)
        label = workload_label(group_keys, values)
        title = f"{args.title_prefix}{label}".strip()
        slug = (
            "-".join(str(v) for v in values if not (isinstance(v, float) and pd.isna(v)))
            .replace("/", "_")
            .replace(" ", "_")
        )
        if not slug:
            slug = "all"
        out = args.out_dir / f"pareto-{slug}-{args.axes}.png"

        fig, ax = plt.subplots(figsize=(8.0, 5.5))
        variants = sorted(sub["variant"].unique())
        palette = sns.color_palette("tab10", n_colors=max(len(variants), 3))

        for color, variant in zip(palette, variants):
            vsub = sub[sub["variant"] == variant].sort_values("capacity")
            if vsub.empty:
                continue
            if args.axes == "hr-vs-tp":
                xs = vsub["hit_ratio"] * 100
                ys = vsub["mops_per_s"]
                ax.plot(
                    xs, ys, "-o", color=color, markersize=9, linewidth=2, label=variant
                )
                for _, r in vsub.iterrows():
                    ax.annotate(
                        f"cap={int(r['capacity'])}",
                        (r["hit_ratio"] * 100, r["mops_per_s"]),
                        textcoords="offset points",
                        xytext=(5, 5),
                        fontsize=7,
                        color=color,
                    )
            elif args.axes == "cap-vs-hr":
                ax.plot(
                    vsub["capacity"],
                    vsub["hit_ratio"] * 100,
                    "-o",
                    color=color,
                    markersize=9,
                    linewidth=2,
                    label=variant,
                )
            elif args.axes == "cap-vs-tp":
                ax.plot(
                    vsub["capacity"],
                    vsub["mops_per_s"],
                    "-o",
                    color=color,
                    markersize=9,
                    linewidth=2,
                    label=variant,
                )

        if args.axes == "hr-vs-tp":
            ax.set_xlabel("hit ratio (%)")
            ax.set_ylabel("throughput (Mops/s)")
        elif args.axes == "cap-vs-hr":
            ax.set_xlabel("capacity (log)")
            ax.set_ylabel("hit ratio (%)")
            ax.set_xscale("log")
        else:  # cap-vs-tp
            ax.set_xlabel("capacity (log)")
            ax.set_ylabel("throughput (Mops/s)")
            ax.set_xscale("log")

        ax.set_title(title or "Pareto")
        ax.grid(True, which="both", alpha=0.4)
        ax.legend(fontsize=9, loc="best")
        fig.tight_layout()
        fig.savefig(out, dpi=150, bbox_inches="tight")
        plt.close(fig)
        print(f"wrote {out}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
