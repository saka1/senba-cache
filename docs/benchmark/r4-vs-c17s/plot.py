#!/usr/bin/env python3
"""sieve_r4 vs sieve_c17s vs senba::concurrent::Cache 性能差分の可視化。

入力 : data/results.csv (run.sh 出力)
出力 (figures/):
  - mops_vs_threads_u64.png      : 6 subplots (skew × mix)、x=T、bars=variant、V=u64
  - mops_vs_threads_string.png   : 同上、V=String
  - regression_summary.md        : セル毎 (V, T, skew, mix) の r4/c17s/senba 比 + accept 判定

実行 : uv run --project scripts python docs/benchmark/r4-vs-c17s/plot.py
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

VARIANTS = ["c17s", "r4", "senba_concurrent"]
COLORS = {"c17s": "#1f77b4", "r4": "#2ca02c", "senba_concurrent": "#d62728"}


def load() -> pd.DataFrame:
    if not DATA.exists():
        sys.exit(f"missing {DATA}; run run.sh first")
    df = pd.read_csv(DATA)
    if df.empty:
        sys.exit("results.csv has no data rows")
    return df


def make_grid(df: pd.DataFrame, value: str) -> None:
    sub_v = df[df["value"] == value]
    if sub_v.empty:
        print(f"[skip] no rows for value={value}", file=sys.stderr)
        return
    skews = sorted(sub_v["skew"].unique())
    mixes = sorted(sub_v["op_mix"].unique())

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
            sub = sub_v[(sub_v["op_mix"] == mix) & (sub_v["skew"] == skew)]
            if sub.empty:
                ax.set_title(f"mix={mix} skew={skew}\n(no data)")
                continue
            threads = sorted(sub["threads"].unique())
            x = np.arange(len(threads))
            w = 0.27
            offsets = {"c17s": -1, "r4": 0, "senba_concurrent": 1}
            for v in VARIANTS:
                vsub = sub[sub["variant"] == v]
                means = []
                for t in threads:
                    cell = vsub[vsub["threads"] == t]["aggregate_mops"]
                    means.append(cell.mean() if not cell.empty else np.nan)
                ax.bar(x + offsets[v] * w, means, w, label=v, color=COLORS[v])
            ax.set_xticks(x)
            ax.set_xticklabels([str(t) for t in threads])
            ax.set_xlabel("threads")
            ax.set_ylabel("Mops" if j == 0 else "")
            ax.set_title(f"mix={mix} skew={skew}")
            ax.grid(axis="y", alpha=0.3)
            if i == 0 and j == 0:
                ax.legend(loc="upper left", fontsize=8)

    fig.suptitle(
        f"sieve_r4 vs c17s vs senba::concurrent — Mops, V={value}, cap=4096 shards=512 zipf",
        fontsize=11,
    )
    fig.tight_layout()
    out = FIG / f"mops_vs_threads_{value}.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}", file=sys.stderr)


def write_summary(df: pd.DataFrame) -> None:
    rows = []
    for (value, mix, skew, t), sub in df.groupby(["value", "op_mix", "skew", "threads"]):
        c = sub[sub["variant"] == "c17s"]["aggregate_mops"]
        r = sub[sub["variant"] == "r4"]["aggregate_mops"]
        s = sub[sub["variant"] == "senba_concurrent"]["aggregate_mops"]
        if c.empty or r.empty or s.empty:
            continue
        c_m, r_m, s_m = c.mean(), r.mean(), s.mean()
        rows.append(
            {
                "value": value,
                "mix": mix,
                "skew": skew,
                "T": t,
                "c17s": round(c_m, 2),
                "r4": round(r_m, 2),
                "senba": round(s_m, 2),
                "r4_vs_c17s_pct": round((r_m / c_m - 1) * 100, 1),
                "r4_vs_senba_pct": round((r_m / s_m - 1) * 100, 1),
            }
        )
    rt = pd.DataFrame(rows).sort_values(["value", "mix", "skew", "T"])

    out = FIG / "regression_summary.md"
    with out.open("w") as f:
        f.write("# sieve_r4 vs c17s vs senba::concurrent — 432-cell sweep\n\n")
        f.write("cap=4096, shards=512, zipf, 3 trials/cell, value=u64+string, threads=1/4/8/16, skew=0.8/1.0/1.4, mix=gim/read-heavy\n\n")
        f.write("`r4_vs_c17s_pct` = (r4/c17s - 1) × 100; `r4_vs_senba_pct` = (r4/senba - 1) × 100\n\n")

        cols = list(rt.columns)
        f.write("| " + " | ".join(cols) + " |\n")
        f.write("| " + " | ".join("---" for _ in cols) + " |\n")
        for _, row in rt.iterrows():
            f.write("| " + " | ".join(str(row[c]) for c in cols) + " |\n")

        f.write("\n## Accept 基準達否 (設計 §G4)\n\n")
        for value in sorted(rt["value"].unique()):
            sub = rt[rt["value"] == value]
            med_c17s = sub["r4_vs_c17s_pct"].median()
            worst_c17s = sub["r4_vs_c17s_pct"].min()
            med_senba = sub["r4_vs_senba_pct"].median()
            worst_senba = sub["r4_vs_senba_pct"].min()
            f.write(f"### V={value}\n\n")
            f.write(f"- r4 vs c17s   : median **{med_c17s:+.1f}%**, worst **{worst_c17s:+.1f}%**\n")
            f.write(f"- r4 vs senba  : median **{med_senba:+.1f}%**, worst **{worst_senba:+.1f}%**\n")

            if value == "u64":
                ok = med_c17s >= -5 and worst_c17s >= -10
                f.write(f"- accept (V=u64: median ≥ -5%, worst ≥ -10% vs c17s): **{'PASS' if ok else 'FAIL'}**\n\n")
            else:
                ok = med_senba >= 30 and worst_senba >= 20
                f.write(f"- accept (V=string: median ≥ +30%, worst ≥ +20% vs senba): **{'PASS' if ok else 'FAIL'}**\n\n")

        # By-axis breakdown for diagnosis
        f.write("\n## Δ% breakdown by axis\n\n")
        for value in sorted(rt["value"].unique()):
            sub = rt[rt["value"] == value]
            f.write(f"### V={value}\n\n")
            for axis in ["mix", "skew", "T"]:
                f.write(f"#### by {axis}\n\n")
                for k, kgrp in sub.groupby(axis):
                    f.write(
                        f"- **{axis}={k}**: r4_vs_c17s median {kgrp['r4_vs_c17s_pct'].median():+.1f}% / "
                        f"worst {kgrp['r4_vs_c17s_pct'].min():+.1f}%, "
                        f"r4_vs_senba median {kgrp['r4_vs_senba_pct'].median():+.1f}% / "
                        f"worst {kgrp['r4_vs_senba_pct'].min():+.1f}%\n"
                    )
                f.write("\n")
    print(f"wrote {out}", file=sys.stderr)


def main() -> None:
    df = load()
    for value in ["u64", "string"]:
        make_grid(df, value)
    write_summary(df)


if __name__ == "__main__":
    main()
