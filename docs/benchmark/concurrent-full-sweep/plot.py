#!/usr/bin/env python3
"""senba::concurrent (v0.4.0, r4-based) vs c17s vs moka vs mini_moka — full
sweep across Zipf + libcachesim + Twitter-Yang + ARC.

Input  : data/results.csv (run.sh output, all phases concatenated)
Output : figures/<source>_mops_vs_threads_<value>.png per source/value pair
         figures/regression_summary.md (cell-by-cell Δ% pivot)

Run with: uv run --project scripts python docs/benchmark/concurrent-full-sweep/plot.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "results.csv"
FIG = HERE / "figures"
FIG.mkdir(exist_ok=True)

VARIANTS = ["senba_concurrent", "c17s", "moka", "mini_moka"]
COLORS = {
    "senba_concurrent": "#2ca02c",
    "c17s": "#1f77b4",
    "moka": "#ff7f0e",
    "mini_moka": "#d62728",
}
BASELINE = "c17s"  # Δ% reference (structural comparison point)


def load() -> pd.DataFrame:
    if not DATA.exists():
        sys.exit(f"missing {DATA}; run run.sh first")
    df = pd.read_csv(DATA)
    if df.empty:
        sys.exit("results.csv has no data rows")
    # bench_concurrent leaves `workload_param` blank for `--source zipf`;
    # pandas converts it to NaN and groupby drops the rows by default.
    # Fill with the source name so the rollup tables stay populated.
    df["workload_param"] = df["workload_param"].fillna(df["source"])
    df.loc[df["workload_param"] == "", "workload_param"] = df["source"]
    return df


def make_grid(df: pd.DataFrame, source: str, value: str) -> None:
    """One figure per (source, value). Subplot grid varies by source axes."""
    sub = df[(df["source"] == source) & (df["value"] == value)]
    if sub.empty:
        return

    if source == "zipf":
        rows_axis, cols_axis = "op_mix", "skew"
    else:
        rows_axis, cols_axis = "op_mix", "workload_param"

    rows = sorted(sub[rows_axis].unique())
    cols = sorted(sub[cols_axis].unique())
    fig, axes = plt.subplots(
        len(rows), len(cols),
        figsize=(3.5 * len(cols), 3.0 * len(rows)),
        sharey=False, squeeze=False,
    )

    for i, rv in enumerate(rows):
        for j, cv in enumerate(cols):
            ax = axes[i, j]
            cell = sub[(sub[rows_axis] == rv) & (sub[cols_axis] == cv)]
            if cell.empty:
                ax.set_title(f"{rows_axis}={rv} {cols_axis}={cv}\n(no data)")
                continue
            threads = sorted(cell["threads"].unique())
            x = np.arange(len(threads))
            w = 0.20
            offsets = {v: i - 1.5 for i, v in enumerate(VARIANTS)}
            for v in VARIANTS:
                vsub = cell[cell["variant"] == v]
                means = [vsub[vsub["threads"] == t]["aggregate_mops"].mean() for t in threads]
                ax.bar(x + offsets[v] * w, means, w, label=v, color=COLORS[v])
            ax.set_xticks(x)
            ax.set_xticklabels([str(t) for t in threads])
            ax.set_xlabel("threads")
            ax.set_ylabel("Mops" if j == 0 else "")
            ax.set_title(f"{rows_axis}={rv} {cols_axis}={cv}")
            ax.grid(axis="y", alpha=0.3)
            if i == 0 and j == 0:
                ax.legend(loc="best", fontsize=7)

    fig.suptitle(
        f"concurrent-full-sweep: source={source}, V={value}",
        fontsize=11,
    )
    fig.tight_layout()
    out = FIG / f"{source}_mops_vs_threads_{value}.png"
    fig.savefig(out, dpi=130)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def write_summary(df: pd.DataFrame) -> None:
    """Per-cell Δ% pivot vs BASELINE, plus per-source / per-value rollups."""
    rows = []
    keys = ["source", "workload_param", "value", "op_mix", "skew", "threads"]
    for cell_key, sub in df.groupby(keys):
        base = sub[sub["variant"] == BASELINE]["aggregate_mops"]
        if base.empty:
            continue
        base_m = base.mean()
        entry: dict = dict(zip(keys, cell_key))
        entry[BASELINE] = round(base_m, 2)
        for v in VARIANTS:
            if v == BASELINE:
                continue
            vals = sub[sub["variant"] == v]["aggregate_mops"]
            if vals.empty:
                entry[f"{v}_pct"] = None
                continue
            entry[f"{v}_pct"] = round((vals.mean() / base_m - 1) * 100, 1)
        rows.append(entry)
    rt = pd.DataFrame(rows).sort_values(keys)

    out = FIG / "regression_summary.md"
    with out.open("w") as f:
        f.write("# concurrent-full-sweep — senba::concurrent (v0.4.0) vs c17s vs moka vs mini_moka\n\n")
        f.write(
            f"`<variant>_pct` = (variant / {BASELINE} − 1) × 100, "
            f"computed per (source, workload, value, op_mix, skew, threads) cell. "
            f"Positive ⇒ variant is faster than {BASELINE}.\n\n"
        )

        # Per-source/value roll-up.
        f.write("## Rollup — median / worst Δ% by (source, value)\n\n")
        for (source, value), sub in rt.groupby(["source", "value"]):
            f.write(f"### source={source}, value={value}\n\n")
            for v in VARIANTS:
                if v == BASELINE:
                    continue
                col = f"{v}_pct"
                if col not in sub or sub[col].isna().all():
                    continue
                med = sub[col].median()
                worst = sub[col].min()
                f.write(f"- **{v}**: median **{med:+.1f}%**, worst **{worst:+.1f}%**\n")
            f.write("\n")

        # Migration accept criterion: senba_concurrent (= r4-based) must be
        # Pareto-non-worse than c17s on every cell (worst ≥ 0 for u64 +
        # tolerated noise floor), and improving on V=String.
        f.write("## Migration accept criterion\n\n")
        f.write(
            "Goal: `senba_concurrent` (v0.4.0, r4-based) Pareto-dominates the prior "
            "lift (c17s-equivalent). Tolerance: median ≥ −5% on V=u64, worst ≥ −10% "
            "(within perf-gate noise); median ≥ +5% on V=String (the r4 design goal).\n\n"
        )
        for value in sorted(rt["value"].unique()):
            sub = rt[rt["value"] == value]
            col = "senba_concurrent_pct"
            if col not in sub or sub[col].isna().all():
                continue
            med = sub[col].median()
            worst = sub[col].min()
            f.write(f"- **V={value}**: median **{med:+.1f}%**, worst **{worst:+.1f}%** ")
            if value == "u64":
                ok = med >= -5 and worst >= -10
            else:
                ok = med >= 5
            f.write(f"→ {'PASS' if ok else 'FAIL'}\n")
        f.write("\n")

        # Full cell table.
        f.write("## Per-cell table\n\n")
        cols = list(rt.columns)
        f.write("| " + " | ".join(cols) + " |\n")
        f.write("| " + " | ".join("---" for _ in cols) + " |\n")
        for _, row in rt.iterrows():
            f.write("| " + " | ".join("" if pd.isna(row[c]) else str(row[c]) for c in cols) + " |\n")

    print(f"wrote {out}", file=sys.stderr)


def main() -> None:
    df = load()
    sources = sorted(df["source"].unique())
    values = sorted(df["value"].unique())
    for s in sources:
        for v in values:
            make_grid(df, s, v)
    write_summary(df)


if __name__ == "__main__":
    main()
