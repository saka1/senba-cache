"""Aggregate the c8/c9/moka/mini_moka concurrent sweep CSV and plot.

Inputs:  docs/reports/data/2026-05-08-c8-vs-c9-thread-sweep.csv
Outputs: docs/reports/data/2026-05-08-c8-vs-c9-{throughput,p99,hr}-{op_mix}.png
         docs/reports/data/2026-05-08-c8-vs-c9-summary.csv (median over trials)
"""

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "docs" / "reports" / "data" / "2026-05-08-c8-vs-c9-thread-sweep.csv"
OUTDIR = ROOT / "docs" / "reports" / "data"

VARIANTS = ["c8", "c9", "moka", "mini_moka"]
COLORS = {"c8": "#1f77b4", "c9": "#d62728", "moka": "#2ca02c", "mini_moka": "#ff7f0e"}
MARKERS = {"c8": "o", "c9": "s", "moka": "^", "mini_moka": "D"}


def main() -> None:
    df = pd.read_csv(DATA)
    # median across trials per (variant, op_mix, skew, threads)
    grp = (
        df.groupby(["variant", "op_mix", "skew", "threads"], as_index=False)
        .agg(
            aggregate_mops=("aggregate_mops", "median"),
            hit_ratio=("hit_ratio", "median"),
            p50_chunk_ns=("p50_chunk_ns", "median"),
            p99_chunk_ns=("p99_chunk_ns", "median"),
            cv=("thread_throughput_cv", "median"),
        )
        .sort_values(["op_mix", "skew", "variant", "threads"])
    )
    grp.to_csv(OUTDIR / "2026-05-08-c8-vs-c9-summary.csv", index=False)

    skews = sorted(grp["skew"].unique())
    op_mixes = sorted(grp["op_mix"].unique())

    for op_mix in op_mixes:
        for metric, ylabel, fname_tag in [
            ("aggregate_mops", "Aggregate Mops/s", "throughput"),
            ("p99_chunk_ns", "p99 chunk latency (ns)", "p99"),
            ("hit_ratio", "Hit ratio", "hr"),
        ]:
            fig, axes = plt.subplots(1, len(skews), figsize=(4.5 * len(skews), 3.6), sharey=False)
            if len(skews) == 1:
                axes = [axes]
            for ax, skew in zip(axes, skews):
                sub = grp[(grp["op_mix"] == op_mix) & (grp["skew"] == skew)]
                for v in VARIANTS:
                    s = sub[sub["variant"] == v].sort_values("threads")
                    if s.empty:
                        continue
                    ax.plot(
                        s["threads"], s[metric],
                        marker=MARKERS[v], color=COLORS[v], label=v, linewidth=1.6,
                    )
                ax.set_xscale("log", base=2)
                if metric != "hit_ratio":
                    ax.set_yscale("log")
                ax.set_xticks([1, 2, 4, 8, 16])
                ax.set_xticklabels([1, 2, 4, 8, 16])
                ax.set_xlabel("threads")
                ax.set_title(f"skew={skew}")
                ax.grid(True, alpha=0.3, which="both")
            axes[0].set_ylabel(ylabel)
            axes[-1].legend(loc="best", fontsize=8)
            fig.suptitle(f"{op_mix} — {ylabel}", y=1.02)
            fig.tight_layout()
            out = OUTDIR / f"2026-05-08-c8-vs-c9-{fname_tag}-{op_mix}.png"
            fig.savefig(out, dpi=130, bbox_inches="tight")
            plt.close(fig)
            print(f"wrote {out.relative_to(ROOT)}")

    # printable text table for the report
    print("\n=== median aggregate Mops/s (op_mix x skew x threads x variant) ===")
    pivot = grp.pivot_table(
        index=["op_mix", "skew", "threads"],
        columns="variant",
        values="aggregate_mops",
    )
    print(pivot.to_string(float_format=lambda x: f"{x:7.2f}"))
    print("\n=== median p99 chunk latency (ns) ===")
    pivot99 = grp.pivot_table(
        index=["op_mix", "skew", "threads"], columns="variant", values="p99_chunk_ns",
    )
    print(pivot99.to_string(float_format=lambda x: f"{x:8.0f}"))
    print("\n=== median hit_ratio ===")
    pivothr = grp.pivot_table(
        index=["op_mix", "skew", "threads"], columns="variant", values="hit_ratio",
    )
    print(pivothr.to_string(float_format=lambda x: f"{x:6.3f}"))


if __name__ == "__main__":
    main()
