#!/usr/bin/env python3
"""sieve_r3 vs sieve_c17s vs senba::concurrent::Cache 性能比較。

入力 : data/results.csv (run.sh 出力)
出力 (figures/):
  - mops_vs_threads.png    : 6 subplots (skew × mix)、x=T、bars=3 variants、y=Mops
  - regression_summary.md  : セル毎 (T,skew,mix) の比 + 全体 aggregate

実行 : uv run --project scripts python docs/benchmark/r3-vs-c17s/plot.py
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

VARIANTS = ["c17s", "r3", "senba_concurrent"]
COLORS = {"c17s": "#1f77b4", "r3": "#2ca02c", "senba_concurrent": "#d62728"}


def load() -> pd.DataFrame:
    if not DATA.exists():
        sys.exit(f"missing {DATA}; run run.sh first")
    df = pd.read_csv(DATA)
    if df.empty:
        sys.exit("results.csv has no data rows")
    return df


def make_grid(df: pd.DataFrame) -> None:
    skews = sorted(df["skew"].unique())
    mixes = sorted(df["op_mix"].unique())

    fig, axes = plt.subplots(
        len(mixes), len(skews), figsize=(4.5 * len(skews), 3.4 * len(mixes)), sharey=False
    )
    if len(mixes) == 1:
        axes = np.array([axes])
    if len(skews) == 1:
        axes = axes.reshape(-1, 1)

    for i, mix in enumerate(mixes):
        for j, skew in enumerate(skews):
            ax = axes[i, j]
            sub = df[(df["op_mix"] == mix) & (df["skew"] == skew)]
            if sub.empty:
                ax.set_title(f"mix={mix} skew={skew}\n(no data)")
                continue
            threads = sorted(sub["threads"].unique())
            x = np.arange(len(threads))
            w = 0.27
            for k, v in enumerate(VARIANTS):
                vsub = sub[sub["variant"] == v]
                means = []
                for t in threads:
                    cell = vsub[vsub["threads"] == t]["aggregate_mops"]
                    means.append(cell.mean() if not cell.empty else np.nan)
                ax.bar(x + (k - 1) * w, means, w, label=v, color=COLORS[v])
            ax.set_xticks(x)
            ax.set_xticklabels([str(t) for t in threads])
            ax.set_xlabel("threads")
            ax.set_ylabel("Mops" if j == 0 else "")
            ax.set_title(f"mix={mix} skew={skew}")
            ax.grid(axis="y", alpha=0.3)
            if i == 0 and j == 0:
                ax.legend(loc="upper left", fontsize=8)

    fig.suptitle(
        "r3 (RwLock) vs c17s vs senba::concurrent — Mops, cap=4096 shards=512 zipf",
        fontsize=11,
    )
    fig.tight_layout()
    out = FIG / "mops_vs_threads.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}", file=sys.stderr)


def _mean(sub: pd.DataFrame, variant: str) -> float:
    cell = sub[sub["variant"] == variant]["aggregate_mops"]
    return float(cell.mean()) if not cell.empty else float("nan")


def write_summary(df: pd.DataFrame) -> None:
    rows = []
    for (mix, skew, t), sub in df.groupby(["op_mix", "skew", "threads"]):
        c = _mean(sub, "c17s")
        r = _mean(sub, "r3")
        s = _mean(sub, "senba_concurrent")
        if np.isnan(c) or np.isnan(r):
            continue
        rows.append(
            {
                "mix": mix,
                "skew": skew,
                "T": t,
                "c17s": round(c, 2),
                "r3": round(r, 2),
                "senba": round(s, 2) if not np.isnan(s) else "-",
                "r3/c17s_%": round((r / c - 1) * 100, 1),
                "r3/senba_%": (
                    round((r / s - 1) * 100, 1) if not np.isnan(s) else "-"
                ),
                "senba/c17s_%": (
                    round((s / c - 1) * 100, 1) if not np.isnan(s) else "-"
                ),
            }
        )
    rt = pd.DataFrame(rows).sort_values(["mix", "skew", "T"])

    out = FIG / "regression_summary.md"
    with out.open("w") as f:
        f.write("# r3 vs c17s vs senba_concurrent — head-to-head\n\n")
        f.write("cap=4096, shards=512, zipf, 3 trials, machine: 12600K (16 threads)\n\n")
        f.write(
            "**`r3/c17s_%`** is the headline (negative = r3 退行)。"
            "`r3/senba_%` shows the recovery from the published `senba::concurrent::Cache`。"
            "`senba/c17s_%` is the regression reproduced from 2026-05-13-senba-concurrent-vs-c17s.\n\n"
        )
        cols = list(rt.columns)
        f.write("| " + " | ".join(cols) + " |\n")
        f.write("| " + " | ".join("---" for _ in cols) + " |\n")
        for _, row in rt.iterrows():
            f.write("| " + " | ".join(str(row[c]) for c in cols) + " |\n")

        f.write("\n## r3/c17s aggregate\n\n")
        d = rt["r3/c17s_%"]
        f.write(f"- worst : {d.min():.1f}%\n")
        f.write(f"- best  : {d.max():.1f}%\n")
        f.write(f"- median: {d.median():.1f}%\n")
        f.write(f"- mean  : {d.mean():.1f}%\n")

        f.write("\n### by op_mix\n\n")
        for mix, mxsub in rt.groupby("mix"):
            d = mxsub["r3/c17s_%"]
            f.write(f"- **{mix}**: median {d.median():.1f}%, worst {d.min():.1f}%\n")
        f.write("\n### by skew\n\n")
        for skew, sksub in rt.groupby("skew"):
            d = sksub["r3/c17s_%"]
            f.write(f"- **skew={skew}**: median {d.median():.1f}%, worst {d.min():.1f}%\n")
        f.write("\n### by threads\n\n")
        for t, tsub in rt.groupby("T"):
            d = tsub["r3/c17s_%"]
            f.write(f"- **T={t}**: median {d.median():.1f}%, worst {d.min():.1f}%\n")

        # r3 vs senba aggregate (recovery)
        rsub = rt[rt["r3/senba_%"] != "-"].copy()
        if not rsub.empty:
            rsub["r3/senba_%"] = rsub["r3/senba_%"].astype(float)
            d = rsub["r3/senba_%"]
            f.write("\n## r3/senba_concurrent aggregate (recovery)\n\n")
            f.write(f"- worst : {d.min():.1f}%\n")
            f.write(f"- best  : {d.max():.1f}%\n")
            f.write(f"- median: {d.median():.1f}%\n")
            f.write(f"- mean  : {d.mean():.1f}%\n")
    print(f"wrote {out}", file=sys.stderr)


def main() -> None:
    df = load()
    make_grid(df)
    write_summary(df)


if __name__ == "__main__":
    main()
