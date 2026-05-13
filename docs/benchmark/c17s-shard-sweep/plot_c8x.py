#!/usr/bin/env python3
"""c17s_8x (per-shard=8) の trace 別 T sweep を可視化する。

入力: data/results.csv (run.sh 出力、軸 = variant × T × workload × cap × trial)
出力 (figures/):
  - c8x_zipf.png
  - c8x_twitter.png
  - c8x_arc.png
  - c8x_summary.md (c1x 比 gain と c8x 絶対値の trace 一覧)

各 subplot:
  - title: workload + cap
  - x: T ∈ {1, 4, 8, 16}
  - 左 y (棒): aggregate Mops (trial mean)
  - 右 y (折れ線): hit_ratio (trial mean)

実行: uv run --project scripts python docs/benchmark/c17s-shard-sweep/plot_c8x.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "results.csv"
FIGURES = HERE / "figures"


def trace_label(row) -> str:
    src = row["source"]
    cap = int(row["cap"])
    if src == "zipf":
        return f"zipf_s{row['skew']:.1f}_{row['op_mix']}_cap{cap}"
    if src in ("twitter", "twitter-yang"):
        return f"{row['workload_param']}_cap{cap}"
    if src == "arc":
        return f"arc_{row['workload_param']}_cap{cap}"
    return f"{src}_cap{cap}"


def trace_class(row) -> str:
    src = row["source"]
    if src == "zipf":
        return "zipf"
    if src in ("twitter", "twitter-yang"):
        return "twitter"
    if src == "arc":
        return "arc"
    return "other"


def aggregate(df: pd.DataFrame) -> pd.DataFrame:
    """trial を畳む。Mops / HR 共に mean、Mops は std も保持。"""
    grp = df.groupby(["trace", "threads"], sort=False).agg(
        mops_mean=("aggregate_mops", "mean"),
        mops_std=("aggregate_mops", "std"),
        hr_mean=("hit_ratio", "mean"),
    ).reset_index()
    return grp


def plot_class(df_class: pd.DataFrame, title: str, out_path: Path) -> None:
    traces = sorted(df_class["trace"].unique())
    n = len(traces)
    if n == 0:
        return
    ncols = 4 if n >= 8 else min(n, 3)
    nrows = (n + ncols - 1) // ncols
    fig, axes = plt.subplots(
        nrows,
        ncols,
        figsize=(ncols * 3.4, nrows * 2.6),
        squeeze=False,
    )
    fig.suptitle(f"c8x (per-shard=8) — {title}", fontsize=13, y=0.995)

    t_order = [1, 4, 8, 16]
    bar_color = "#1f77b4"
    line_color = "#d62728"

    for i, trace in enumerate(traces):
        ax = axes[i // ncols][i % ncols]
        sub = df_class[df_class["trace"] == trace].set_index("threads")
        sub = sub.reindex(t_order)
        xs = np.arange(len(t_order))
        mops = sub["mops_mean"].to_numpy()
        mops_std = sub["mops_std"].to_numpy()
        hr = sub["hr_mean"].to_numpy()

        bars = ax.bar(
            xs,
            mops,
            yerr=np.nan_to_num(mops_std, nan=0.0),
            color=bar_color,
            alpha=0.75,
            width=0.65,
            capsize=2,
            label="Mops",
        )
        ax.set_xticks(xs)
        ax.set_xticklabels([f"T={t}" for t in t_order], fontsize=8)
        ax.set_title(trace, fontsize=8.5)
        ax.tick_params(axis="y", labelsize=8)
        ax.set_ylabel("Mops", color=bar_color, fontsize=8)
        ax.tick_params(axis="y", labelcolor=bar_color)
        ax.grid(axis="y", linestyle=":", alpha=0.4)
        ymax = np.nanmax(mops) if np.any(~np.isnan(mops)) else 1.0
        ax.set_ylim(0, ymax * 1.18)

        ax_r = ax.twinx()
        ax_r.plot(
            xs, hr, color=line_color, marker="o", markersize=4.5, linewidth=1.6,
            label="HR",
        )
        ax_r.set_ylabel("HR", color=line_color, fontsize=8)
        ax_r.tick_params(axis="y", labelcolor=line_color, labelsize=8)
        ax_r.set_ylim(0, 1.0)

        # value labels above bars
        for x, m in zip(xs, mops):
            if not np.isnan(m):
                ax.text(x, m, f"{m:.0f}", ha="center", va="bottom", fontsize=7,
                        color=bar_color)
        for x, h in zip(xs, hr):
            if not np.isnan(h):
                ax_r.text(x, h, f"{h:.2f}", ha="center", va="bottom", fontsize=7,
                          color=line_color)

    # blank unused axes
    for j in range(n, nrows * ncols):
        axes[j // ncols][j % ncols].axis("off")

    fig.tight_layout(rect=[0, 0, 1, 0.97])
    fig.savefig(out_path, dpi=140, bbox_inches="tight")
    plt.close(fig)


def write_summary_md(df_c8x: pd.DataFrame, df_c1x: pd.DataFrame, out: Path) -> None:
    pivot_c8 = df_c8x.pivot(index="trace", columns="threads", values="mops_mean")
    pivot_c1 = df_c1x.pivot(index="trace", columns="threads", values="mops_mean")
    pivot_hr = df_c8x.pivot(index="trace", columns="threads", values="hr_mean")
    pivot_hr_c1 = df_c1x.pivot(index="trace", columns="threads", values="hr_mean")

    rows = []
    for trace in sorted(pivot_c8.index):
        if 16 not in pivot_c8.columns or trace not in pivot_c1.index:
            continue
        m_c8_T1 = pivot_c8.loc[trace].get(1, float("nan"))
        m_c8_T16 = pivot_c8.loc[trace].get(16, float("nan"))
        m_c1_T16 = pivot_c1.loc[trace].get(16, float("nan"))
        gain_pct = (
            (m_c8_T16 / m_c1_T16 - 1.0) * 100.0
            if not np.isnan(m_c1_T16) and m_c1_T16 > 0
            else float("nan")
        )
        hr_c8_T16 = pivot_hr.loc[trace].get(16, float("nan"))
        hr_c1_T16 = pivot_hr_c1.loc[trace].get(16, float("nan"))
        hr_delta_pp = (hr_c8_T16 - hr_c1_T16) * 100.0 if not np.isnan(hr_c1_T16) else float("nan")
        rows.append((trace, m_c8_T1, m_c8_T16, gain_pct, hr_c8_T16, hr_delta_pp))

    rows.sort(key=lambda r: -(r[3] if not np.isnan(r[3]) else -1e9))

    with out.open("w") as f:
        f.write("# c8x (per-shard=8) — trace 別実測値\n\n")
        f.write(f"出典: `data/results.csv` ({len(df_c8x)} c8x rows、trial 平均)\n\n")
        f.write("- 列の意味:\n")
        f.write("  - `Mops T=1` / `Mops T=16`: c8x の絶対値 (trial 平均)\n")
        f.write("  - `gain @T=16`: c1x (= 現状 senba auto-shard, per-shard≤64) 比の Mops 改善率\n")
        f.write("  - `HR @T=16`: c8x の hit ratio\n")
        f.write("  - `ΔHR pp`: c8x − c1x の hit ratio 差 (percentage point、+ なら c8x が良い)\n\n")
        f.write("`gain @T=16` 降順:\n\n")
        f.write(
            "| trace | Mops T=1 | Mops T=16 | gain @T=16 | HR @T=16 | ΔHR pp |\n"
        )
        f.write("|---|---:|---:|---:|---:|---:|\n")
        for trace, m1, m16, g, hr, dhr in rows:
            f.write(
                f"| {trace} | {m1:.1f} | {m16:.1f} | "
                f"{('%.0f' % g) + '%' if not np.isnan(g) else 'n/a'} | "
                f"{hr:.3f} | "
                f"{('+%.2f' if not np.isnan(dhr) and dhr >= 0 else '%.2f') % dhr if not np.isnan(dhr) else 'n/a'} |\n"
            )

        # cliff zone (HR drop > 3pp)
        cliffs = [r for r in rows if not np.isnan(r[5]) and r[5] < -3.0]
        if cliffs:
            f.write("\n## HR cliff (c8x が c1x 比で −3pp 以上 HR を落とした trace)\n\n")
            f.write("| trace | ΔHR pp | gain @T=16 |\n|---|---:|---:|\n")
            for trace, _, _, g, _, dhr in sorted(cliffs, key=lambda r: r[5]):
                f.write(
                    f"| {trace} | {dhr:+.2f} | "
                    f"{('%.0f' % g) + '%' if not np.isnan(g) else 'n/a'} |\n"
                )


def main() -> int:
    if not DATA.exists():
        print(f"missing: {DATA}", file=sys.stderr)
        return 1
    FIGURES.mkdir(exist_ok=True)

    df = pd.read_csv(DATA)
    df["trace"] = df.apply(trace_label, axis=1)
    df["trace_class"] = df.apply(trace_class, axis=1)

    df_c8x = df[df["variant"] == "c8x"].copy()
    df_c1x = df[df["variant"] == "c1x"].copy()
    if df_c8x.empty:
        print("no c8x rows", file=sys.stderr)
        return 1

    agg_c8 = aggregate(df_c8x.assign(trace=df_c8x["trace"]))
    agg_c1 = aggregate(df_c1x.assign(trace=df_c1x["trace"]))
    agg_c8["trace_class"] = agg_c8["trace"].map(
        df_c8x.drop_duplicates("trace").set_index("trace")["trace_class"]
    )

    for cls, title in [
        ("zipf", "Zipf synthetic"),
        ("twitter", "Twitter Yang clusters"),
        ("arc", "ARC presets"),
    ]:
        sub = agg_c8[agg_c8["trace_class"] == cls]
        plot_class(sub, title, FIGURES / f"c8x_{cls}.png")
        print(f"wrote {FIGURES / f'c8x_{cls}.png'} ({sub['trace'].nunique()} traces)")

    write_summary_md(agg_c8, agg_c1, FIGURES / "c8x_summary.md")
    print(f"wrote {FIGURES / 'c8x_summary.md'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
