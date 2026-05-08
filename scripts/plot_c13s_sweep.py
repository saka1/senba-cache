"""Plot c13s (senba::Cache lineage + lock-free Path A) single-shard sweep.

Inputs:  docs/reports/data/2026-05-08-c13s-sweep.csv
Outputs: docs/reports/data/2026-05-08-c13s-{metric}-{op_mix}.png
         docs/reports/data/2026-05-08-c13s-summary.csv
"""

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "docs" / "reports" / "data" / "2026-05-08-c13s-sweep.csv"
OUTDIR = ROOT / "docs" / "reports" / "data"

VARIANTS = ["c8", "c11s", "c12s", "c13s"]
COLORS = {"c8": "#1f77b4", "c11s": "#9467bd", "c12s": "#d62728", "c13s": "#2ca02c"}
MARKERS = {"c8": "o", "c11s": "D", "c12s": "s", "c13s": "^"}


def workload_label(row) -> str:
    w = row["workload"]
    if w == "zipf":
        return f"zipf-{row['skew']}"
    return w


def main() -> None:
    df = pd.read_csv(DATA)
    df["workload_label"] = df.apply(workload_label, axis=1)

    grp = (
        df.groupby(["variant", "op_mix", "workload_label", "threads"], as_index=False)
        .agg(
            aggregate_mops=("aggregate_mops", "median"),
            mops_min_per_thread=("mops_min_per_thread", "median"),
            hit_ratio=("hit_ratio", "median"),
            p50_chunk_ns=("p50_chunk_ns", "median"),
            p99_chunk_ns=("p99_chunk_ns", "median"),
            cv=("thread_throughput_cv", "median"),
        )
        .sort_values(["op_mix", "workload_label", "variant", "threads"])
    )
    grp.to_csv(OUTDIR / "2026-05-08-c13s-summary.csv", index=False)

    workload_order = ["zipf-0.7", "zipf-1.0", "zipf-1.2", "adversarial-hot", "uniform"]
    op_mixes = ["read-only", "read-heavy", "gim"]

    for op_mix in op_mixes:
        for metric, ylabel, fname_tag in [
            ("aggregate_mops", "Aggregate Mops/s", "throughput"),
            ("mops_min_per_thread", "Min per-thread Mops/s", "min-mops"),
            ("p99_chunk_ns", "p99 chunk latency (ns)", "p99"),
            ("hit_ratio", "Hit ratio", "hr"),
        ]:
            fig, axes = plt.subplots(
                1, len(workload_order), figsize=(3.4 * len(workload_order), 3.4), sharey=False
            )
            for ax, w in zip(axes, workload_order):
                sub = grp[(grp["op_mix"] == op_mix) & (grp["workload_label"] == w)]
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
                ax.set_title(w, fontsize=9)
                ax.grid(True, alpha=0.3, which="both")
            axes[0].set_ylabel(ylabel)
            axes[-1].legend(loc="best", fontsize=8)
            fig.suptitle(f"single-shard / {op_mix} — {ylabel}", y=1.02)
            fig.tight_layout()
            out = OUTDIR / f"2026-05-08-c13s-{fname_tag}-{op_mix}.png"
            fig.savefig(out, dpi=130, bbox_inches="tight")
            plt.close(fig)
            print(f"wrote {out.relative_to(ROOT)}")

    # printable text tables (markdown 流し込み用)
    print("\n=== median aggregate Mops/s (op_mix x workload x threads x variant) ===")
    pivot = grp.pivot_table(
        index=["op_mix", "workload_label", "threads"],
        columns="variant",
        values="aggregate_mops",
    )
    print(pivot.to_string(float_format=lambda x: f"{x:7.2f}"))

    print("\n=== median mops_min_per_thread ===")
    pivot_min = grp.pivot_table(
        index=["op_mix", "workload_label", "threads"],
        columns="variant",
        values="mops_min_per_thread",
    )
    print(pivot_min.to_string(float_format=lambda x: f"{x:7.3f}"))

    print("\n=== median p99 chunk latency (ns) ===")
    pivot99 = grp.pivot_table(
        index=["op_mix", "workload_label", "threads"],
        columns="variant",
        values="p99_chunk_ns",
    )
    print(pivot99.to_string(float_format=lambda x: f"{x:8.0f}"))

    print("\n=== median hit_ratio ===")
    pivothr = grp.pivot_table(
        index=["op_mix", "workload_label", "threads"],
        columns="variant",
        values="hit_ratio",
    )
    print(pivothr.to_string(float_format=lambda x: f"{x:6.3f}"))

    # 採否判定: read-heavy zipf-1.0 16T で c13s vs c11s/c8
    print("\n=== 採否判定: read-heavy zipf-1.0 16T (c8 vs c11s vs c12s vs c13s) ===")
    judge = grp[
        (grp["op_mix"] == "read-heavy")
        & (grp["workload_label"] == "zipf-1.0")
        & (grp["threads"] == 16)
    ]
    if not judge.empty:
        print(judge[["variant", "aggregate_mops", "mops_min_per_thread", "p99_chunk_ns", "hit_ratio"]].to_string(index=False))
        c8 = judge[judge["variant"] == "c8"]["aggregate_mops"]
        c11s = judge[judge["variant"] == "c11s"]["aggregate_mops"]
        c13s = judge[judge["variant"] == "c13s"]["aggregate_mops"]
        if not c8.empty and not c13s.empty:
            r8 = float(c13s.iloc[0]) / float(c8.iloc[0])
            print(f"\nc13s / c8   = {r8:.3f} ({(r8 - 1.0) * 100:+.1f}%)")
        if not c11s.empty and not c13s.empty:
            r11 = float(c13s.iloc[0]) / float(c11s.iloc[0])
            print(f"c13s / c11s = {r11:.3f} ({(r11 - 1.0) * 100:+.1f}%)")

    # HR 同等性検証 (c13s は SIEVE 等価のはず → c11s/c8 と一致)
    print("\n=== HR 同等性検証 (c13s vs c11s, all op_mix/workload, 16T) ===")
    hr16 = grp[grp["threads"] == 16].pivot_table(
        index=["op_mix", "workload_label"], columns="variant", values="hit_ratio"
    )
    if "c11s" in hr16.columns and "c13s" in hr16.columns:
        hr16["delta"] = hr16["c13s"] - hr16["c11s"]
        print(hr16.to_string(float_format=lambda x: f"{x:7.4f}"))


if __name__ == "__main__":
    main()
