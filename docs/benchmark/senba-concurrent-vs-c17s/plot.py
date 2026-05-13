#!/usr/bin/env python3
"""senba::concurrent::Cache vs sieve_c17s 性能差分の可視化。

入力 : data/results.csv (run.sh 出力)
出力 (figures/):
  - mops_vs_threads.png     : 6 subplots (skew × mix)、x=T、bars=variant、left y=Mops
  - regression_summary.md   : セル毎 (T,skew,mix) の senba / c17s 比 + 全体まとめ

実行 : uv run --project scripts python docs/benchmark/senba-concurrent-vs-c17s/plot.py
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
    variants = ["c17s", "senba_concurrent"]
    colors = {"c17s": "#1f77b4", "senba_concurrent": "#d62728"}

    fig, axes = plt.subplots(
        len(mixes), len(skews), figsize=(4 * len(skews), 3.2 * len(mixes)), sharey=False
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
            w = 0.4
            for k, v in enumerate(variants):
                vsub = sub[sub["variant"] == v]
                means = []
                for t in threads:
                    cell = vsub[vsub["threads"] == t]["aggregate_mops"]
                    means.append(cell.mean() if not cell.empty else np.nan)
                ax.bar(x + (k - 0.5) * w, means, w, label=v, color=colors[v])
            ax.set_xticks(x)
            ax.set_xticklabels([str(t) for t in threads])
            ax.set_xlabel("threads")
            ax.set_ylabel("Mops" if j == 0 else "")
            ax.set_title(f"mix={mix} skew={skew}")
            ax.grid(axis="y", alpha=0.3)
            if i == 0 and j == 0:
                ax.legend(loc="upper left", fontsize=8)

    fig.suptitle(
        "senba::concurrent::Cache vs sieve_c17s — Mops, cap=4096 shards=512 zipf",
        fontsize=11,
    )
    fig.tight_layout()
    out = FIG / "mops_vs_threads.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}", file=sys.stderr)


def write_summary(df: pd.DataFrame) -> None:
    rows = []
    for (mix, skew, t), sub in df.groupby(["op_mix", "skew", "threads"]):
        c = sub[sub["variant"] == "c17s"]["aggregate_mops"]
        s = sub[sub["variant"] == "senba_concurrent"]["aggregate_mops"]
        if c.empty or s.empty:
            continue
        ratio = s.mean() / c.mean()
        rows.append(
            {
                "mix": mix,
                "skew": skew,
                "T": t,
                "c17s_Mops": round(c.mean(), 2),
                "senba_Mops": round(s.mean(), 2),
                "ratio": round(ratio, 3),
                "delta_pct": round((ratio - 1) * 100, 1),
            }
        )
    rt = pd.DataFrame(rows).sort_values(["mix", "skew", "T"])

    out = FIG / "regression_summary.md"
    with out.open("w") as f:
        f.write("# senba::concurrent vs c17s — regression table\n\n")
        f.write(f"cap=4096, shards=512, zipf, 3 trials, machine: 12600K (16 threads)\n\n")
        f.write("`ratio` = senba_concurrent / c17s; `delta_pct` = (ratio - 1) * 100\n\n")
        cols = list(rt.columns)
        f.write("| " + " | ".join(cols) + " |\n")
        f.write("| " + " | ".join("---" for _ in cols) + " |\n")
        for _, row in rt.iterrows():
            f.write("| " + " | ".join(str(row[c]) for c in cols) + " |\n")
        f.write("\n\n## Aggregate\n\n")
        f.write(f"- worst-cell delta: {rt['delta_pct'].min():.1f}%\n")
        f.write(f"- best-cell delta : {rt['delta_pct'].max():.1f}%\n")
        f.write(f"- median delta    : {rt['delta_pct'].median():.1f}%\n")
        f.write(f"- mean delta      : {rt['delta_pct'].mean():.1f}%\n")
        f.write("\n## By op_mix\n\n")
        for mix, mxsub in rt.groupby("mix"):
            f.write(f"- **{mix}**: median {mxsub['delta_pct'].median():.1f}%, worst {mxsub['delta_pct'].min():.1f}%\n")
        f.write("\n## By skew\n\n")
        for skew, sksub in rt.groupby("skew"):
            f.write(f"- **skew={skew}**: median {sksub['delta_pct'].median():.1f}%, worst {sksub['delta_pct'].min():.1f}%\n")
        f.write("\n## By threads\n\n")
        for t, tsub in rt.groupby("T"):
            f.write(f"- **T={t}**: median {tsub['delta_pct'].median():.1f}%, worst {tsub['delta_pct'].min():.1f}%\n")
    print(f"wrote {out}", file=sys.stderr)


def main() -> None:
    df = load()
    make_grid(df)
    write_summary(df)


if __name__ == "__main__":
    main()
