#!/usr/bin/env python3
"""r1 sweep の HR vs Mops pareto 図を生成する。

入力: data/results.csv (run.sh の出力、append-only)
出力: figures/*.png

設計 §6.2 の指定図 + scalability:
  - fig_mops_vs_ways_<workload>_<value>.png    : x=ways, y=Mops, line per T
  - fig_hr_vs_ways_<workload>_<value>.png      : x=ways, y=HR, line per T
  - fig_mops_vs_threads_<workload>_<value>.png : x=T, y=Mops, line per WAYS (+c17s baseline)
  - fig_hr_vs_threads_<workload>_<value>.png   : x=T, y=HR, line per WAYS (+c17s baseline)
  - fig_pareto_<workload>_<value>.png          : x=HR drop, y=Mops gain, accept zone shade
  - fig_pareto_overlay_all.png                 : 全 (workload, value) を 1 図に overlay

実行: uv run --project scripts python plot.py data/results.csv figures/
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

# 設計 §6.3 accept zone (HR drop <5pp, Mops gain >+20%)
ACCEPT_HR_DROP_MAX_PP = 5.0
ACCEPT_MOPS_GAIN_MIN_PCT = 20.0


def workload_label(row) -> str:
    src = row["source"]
    if src == "zipf":
        return f"zipf_s{row['skew']:.1f}_{row['op_mix']}"
    elif src in ("twitter", "twitter-yang"):
        return f"twitter_{row['workload_param']}"
    elif src == "arc":
        return f"arc_{row['workload_param']}_cap{row['cap']}"
    return src


def aggregate(df: pd.DataFrame) -> pd.DataFrame:
    """trial 中央値で集約。grouping key は 1 cell を一意に決める列。"""
    df = df.copy()
    # Zipf 行は workload_param 列が空 → pandas が NaN として読む。default `dropna=True`
    # で groupby すると Zipf 全行が捨てられる。fillna("") で同一 group に集約する。
    df["workload_param"] = df["workload_param"].fillna("").astype(str)
    keys = [
        "variant",
        "ways",
        "source",
        "workload_param",
        "op_mix",
        "value",
        "skew",
        "threads",
        "cap",
    ]
    agg = (
        df.groupby(keys, dropna=False)
        .agg(
            mops_median=("aggregate_mops", "median"),
            mops_std=("aggregate_mops", "std"),
            hr_median=("hit_ratio", "median"),
            cv_median=("thread_throughput_cv", "median"),
            n_trials=("trial", "count"),
        )
        .reset_index()
    )
    agg["mops_std"] = agg["mops_std"].fillna(0.0)
    return agg


def plot_mops_vs_ways(agg: pd.DataFrame, workload: str, value: str, outdir: Path):
    sub = agg[
        (agg["variant"] == "r1")
        & (agg.apply(workload_label, axis=1) == workload)
        & (agg["value"] == value)
    ]
    if sub.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 5))
    threads_levels = sorted(sub["threads"].unique())
    cmap = plt.colormaps.get_cmap("viridis").resampled(max(len(threads_levels), 2))
    for i, t in enumerate(threads_levels):
        rows = sub[sub["threads"] == t].sort_values("ways")
        ax.errorbar(
            rows["ways"],
            rows["mops_median"],
            yerr=rows["mops_std"],
            marker="o",
            color=cmap(i),
            label=f"T={t}",
            capsize=3,
            linewidth=1.5,
        )
    ax.set_xscale("log", base=2)
    ax.set_xlabel("WAYS")
    ax.set_ylabel("aggregate Mops")
    ax.set_title(f"r1 Mops vs WAYS — {workload} ({value})")
    ax.legend(loc="best", fontsize=9)
    ax.grid(True, alpha=0.3)
    out = outdir / f"fig_mops_vs_ways_{workload}_{value}.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def plot_mops_vs_threads(agg: pd.DataFrame, workload: str, value: str, outdir: Path):
    """T scalability 曲線: x=T, y=Mops, line per WAYS (+ c17s baseline)。"""
    sub_all = agg[(agg.apply(workload_label, axis=1) == workload) & (agg["value"] == value)]
    if sub_all.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 5))
    ways_levels = sorted(sub_all[sub_all["variant"] == "r1"]["ways"].unique())
    cmap = plt.colormaps.get_cmap("viridis").resampled(max(len(ways_levels), 2))
    for i, w in enumerate(ways_levels):
        rows = sub_all[(sub_all["variant"] == "r1") & (sub_all["ways"] == w)].sort_values("threads")
        if rows.empty:
            continue
        ax.errorbar(
            rows["threads"],
            rows["mops_median"],
            yerr=rows["mops_std"],
            marker="o",
            color=cmap(i),
            label=f"r1 w={w}",
            capsize=3,
            linewidth=1.5,
        )
    base = sub_all[(sub_all["variant"] == "c17s") & (sub_all["ways"] == 1)].sort_values("threads")
    if not base.empty:
        ax.errorbar(
            base["threads"],
            base["mops_median"],
            yerr=base["mops_std"],
            marker="s",
            color="black",
            linestyle="--",
            label="c17s (baseline)",
            capsize=3,
            linewidth=1.5,
        )
    ax.set_xscale("log", base=2)
    ax.set_xlabel("threads (T)")
    ax.set_ylabel("aggregate Mops")
    ax.set_title(f"Mops vs T — {workload} ({value})")
    ax.legend(loc="best", fontsize=9)
    ax.grid(True, alpha=0.3)
    out = outdir / f"fig_mops_vs_threads_{workload}_{value}.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def plot_hr_vs_threads(agg: pd.DataFrame, workload: str, value: str, outdir: Path):
    """HR scalability: x=T, y=HR, line per WAYS (+ c17s baseline)。"""
    sub_all = agg[(agg.apply(workload_label, axis=1) == workload) & (agg["value"] == value)]
    if sub_all.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 5))
    ways_levels = sorted(sub_all[sub_all["variant"] == "r1"]["ways"].unique())
    cmap = plt.colormaps.get_cmap("viridis").resampled(max(len(ways_levels), 2))
    for i, w in enumerate(ways_levels):
        rows = sub_all[(sub_all["variant"] == "r1") & (sub_all["ways"] == w)].sort_values("threads")
        if rows.empty:
            continue
        ax.plot(
            rows["threads"],
            rows["hr_median"],
            marker="o",
            color=cmap(i),
            label=f"r1 w={w}",
            linewidth=1.5,
        )
    base = sub_all[(sub_all["variant"] == "c17s") & (sub_all["ways"] == 1)].sort_values("threads")
    if not base.empty:
        ax.plot(
            base["threads"],
            base["hr_median"],
            marker="s",
            color="black",
            linestyle="--",
            label="c17s (baseline)",
            linewidth=1.5,
        )
    ax.set_xscale("log", base=2)
    ax.set_xlabel("threads (T)")
    ax.set_ylabel("hit ratio")
    ax.set_title(f"HR vs T — {workload} ({value})")
    ax.legend(loc="best", fontsize=9)
    ax.grid(True, alpha=0.3)
    out = outdir / f"fig_hr_vs_threads_{workload}_{value}.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def plot_hr_vs_ways(agg: pd.DataFrame, workload: str, value: str, outdir: Path):
    sub = agg[
        (agg["variant"] == "r1")
        & (agg.apply(workload_label, axis=1) == workload)
        & (agg["value"] == value)
    ]
    if sub.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 5))
    threads_levels = sorted(sub["threads"].unique())
    cmap = plt.colormaps.get_cmap("viridis").resampled(max(len(threads_levels), 2))
    for i, t in enumerate(threads_levels):
        rows = sub[sub["threads"] == t].sort_values("ways")
        ax.plot(
            rows["ways"],
            rows["hr_median"],
            marker="o",
            color=cmap(i),
            label=f"T={t}",
            linewidth=1.5,
        )
    ax.set_xscale("log", base=2)
    ax.set_xlabel("WAYS")
    ax.set_ylabel("hit ratio")
    ax.set_title(f"r1 HR vs WAYS — {workload} ({value})")
    ax.legend(loc="best", fontsize=9)
    ax.grid(True, alpha=0.3)
    out = outdir / f"fig_hr_vs_ways_{workload}_{value}.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def pareto_pairs(agg: pd.DataFrame) -> pd.DataFrame:
    """各 (workload, value, T, ways) について r1 と c17s@ways=1 を pair した DataFrame。"""
    agg = agg.copy()
    agg["workload"] = agg.apply(workload_label, axis=1)
    base_keys = ["workload", "value", "threads"]
    c17s = agg[(agg["variant"] == "c17s") & (agg["ways"] == 1)][
        base_keys + ["mops_median", "hr_median"]
    ].rename(columns={"mops_median": "mops_c17s", "hr_median": "hr_c17s"})
    r1 = agg[agg["variant"] == "r1"][
        base_keys + ["ways", "mops_median", "mops_std", "hr_median", "cv_median"]
    ].rename(columns={"mops_median": "mops_r1", "hr_median": "hr_r1"})
    merged = r1.merge(c17s, on=base_keys, how="inner")
    merged["mops_gain_pct"] = (merged["mops_r1"] / merged["mops_c17s"] - 1) * 100
    merged["hr_drop_pp"] = (merged["hr_c17s"] - merged["hr_r1"]) * 100
    return merged


def plot_pareto_single(pairs: pd.DataFrame, workload: str, value: str, outdir: Path):
    sub = pairs[(pairs["workload"] == workload) & (pairs["value"] == value)]
    if sub.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 6))
    threads_levels = sorted(sub["threads"].unique())
    ways_levels = sorted(sub["ways"].unique())
    cmap = plt.colormaps.get_cmap("viridis").resampled(max(len(threads_levels), 2))
    markers = ["o", "s", "^", "D", "v", "P", "X", "*"]
    way_marker = {w: markers[i % len(markers)] for i, w in enumerate(ways_levels)}
    for i, t in enumerate(threads_levels):
        for w in ways_levels:
            cell = sub[(sub["threads"] == t) & (sub["ways"] == w)]
            if cell.empty:
                continue
            ax.scatter(
                cell["hr_drop_pp"],
                cell["mops_gain_pct"],
                color=cmap(i),
                marker=way_marker[w],
                s=80,
                edgecolor="black",
                linewidth=0.6,
                label=f"T={t}, w={w}",
            )
    # accept zone shade (HR drop <= 5pp, Mops gain >= +20%)
    xlim = ax.get_xlim()
    ylim = ax.get_ylim()
    ax.axhspan(ACCEPT_MOPS_GAIN_MIN_PCT, max(ylim[1], ACCEPT_MOPS_GAIN_MIN_PCT + 10), xmin=0, xmax=1, alpha=0.0)
    ax.fill_betweenx(
        [ACCEPT_MOPS_GAIN_MIN_PCT, max(ylim[1], ACCEPT_MOPS_GAIN_MIN_PCT + 10)],
        xlim[0],
        ACCEPT_HR_DROP_MAX_PP,
        color="green",
        alpha=0.12,
        label="accept zone (HR drop <5pp, Mops gain >+20%)",
    )
    ax.axhline(0, color="black", linewidth=0.8)
    ax.axvline(0, color="black", linewidth=0.8)
    ax.set_xlabel("HR drop vs c17s baseline (pp; +→ r1 worse)")
    ax.set_ylabel("Mops gain vs c17s baseline (%; +→ r1 better)")
    ax.set_title(f"Pareto: r1 vs c17s — {workload} ({value})")
    # ax.legend を最初の T 列だけに絞る (cell 数が多いと見えなくなる)
    handles, labels = ax.get_legend_handles_labels()
    seen_t = set()
    h_keep, l_keep = [], []
    for h, l in zip(handles, labels):
        t_part = l.split(",")[0]
        if "accept zone" in l or t_part not in seen_t:
            h_keep.append(h)
            l_keep.append(l)
            if "accept zone" not in l:
                seen_t.add(t_part)
    ax.legend(h_keep, l_keep, loc="best", fontsize=8, ncol=2)
    ax.grid(True, alpha=0.3)
    out = outdir / f"fig_pareto_{workload}_{value}.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def plot_pareto_overlay_all(pairs: pd.DataFrame, outdir: Path):
    if pairs.empty:
        return
    fig, ax = plt.subplots(figsize=(8, 6))
    workloads = pairs.assign(wv=pairs["workload"] + "/" + pairs["value"])["wv"].unique()
    workloads = sorted(workloads)
    cmap = plt.colormaps.get_cmap("tab20").resampled(max(len(workloads), 2))
    for i, wv in enumerate(workloads):
        wl, val = wv.rsplit("/", 1)
        sub = pairs[(pairs["workload"] == wl) & (pairs["value"] == val)]
        ax.scatter(
            sub["hr_drop_pp"],
            sub["mops_gain_pct"],
            color=cmap(i),
            alpha=0.55,
            s=40,
            label=wv,
        )
    ax.axhline(0, color="black", linewidth=0.8)
    ax.axvline(0, color="black", linewidth=0.8)
    xlim = ax.get_xlim()
    ylim = ax.get_ylim()
    ax.fill_betweenx(
        [ACCEPT_MOPS_GAIN_MIN_PCT, max(ylim[1], ACCEPT_MOPS_GAIN_MIN_PCT + 10)],
        xlim[0],
        ACCEPT_HR_DROP_MAX_PP,
        color="green",
        alpha=0.12,
    )
    ax.set_xlabel("HR drop vs c17s (pp)")
    ax.set_ylabel("Mops gain vs c17s (%)")
    ax.set_title("r1 pareto overlay (all workloads × values)")
    ax.legend(loc="best", fontsize=7, ncol=2)
    ax.grid(True, alpha=0.3)
    out = outdir / "fig_pareto_overlay_all.png"
    fig.tight_layout()
    fig.savefig(out, dpi=150, bbox_inches="tight")
    plt.close(fig)


def write_summary(pairs: pd.DataFrame, agg: pd.DataFrame, outdir: Path):
    """accept zoneに乗ったセル数と best cell を markdown でまとめる。"""
    lines = ["# r1 sweep summary", ""]
    if pairs.empty:
        lines.append("(no r1 × c17s pareto pairs — bench データが空)")
        (outdir / "summary.md").write_text("\n".join(lines))
        return
    accept = pairs[
        (pairs["hr_drop_pp"] <= ACCEPT_HR_DROP_MAX_PP)
        & (pairs["mops_gain_pct"] >= ACCEPT_MOPS_GAIN_MIN_PCT)
    ]
    lines.append(
        f"- accept zone (HR drop ≤{ACCEPT_HR_DROP_MAX_PP}pp, Mops gain ≥+{ACCEPT_MOPS_GAIN_MIN_PCT}%) "
        f"に乗った cell 数: **{len(accept)}** / {len(pairs)}"
    )
    def fmt_table(df: pd.DataFrame, float_cols: list[str]) -> str:
        # tabulate なしで pipe-table (markdown 互換) を手書き。
        hdrs = list(df.columns)
        out = ["| " + " | ".join(hdrs) + " |", "|" + "|".join(["---"] * len(hdrs)) + "|"]
        for _, row in df.iterrows():
            cells = []
            for c in hdrs:
                v = row[c]
                if c in float_cols and isinstance(v, (int, float, np.floating, np.integer)):
                    cells.append(f"{float(v):.3f}")
                else:
                    cells.append(str(v))
            out.append("| " + " | ".join(cells) + " |")
        return "\n".join(out)

    fcols = ["hr_drop_pp", "mops_gain_pct", "mops_r1", "mops_c17s"]
    if not accept.empty:
        lines.append("")
        lines.append("## accept zone cells (上位 20)")
        lines.append("")
        cols = [
            "workload",
            "value",
            "threads",
            "ways",
            "hr_drop_pp",
            "mops_gain_pct",
            "mops_r1",
            "mops_c17s",
        ]
        top = accept.nlargest(20, "mops_gain_pct")[cols]
        lines.append(fmt_table(top, fcols))
    # Best (T, ways) per workload by Mops gain
    lines.append("")
    lines.append("## workload × value 別の best (T, ways) by Mops gain")
    lines.append("")
    best = pairs.loc[pairs.groupby(["workload", "value"])["mops_gain_pct"].idxmax()][
        [
            "workload",
            "value",
            "threads",
            "ways",
            "mops_gain_pct",
            "hr_drop_pp",
            "mops_r1",
            "mops_c17s",
        ]
    ].sort_values("mops_gain_pct", ascending=False)
    lines.append(fmt_table(best, fcols))
    (outdir / "summary.md").write_text("\n".join(lines) + "\n")


def main():
    if len(sys.argv) < 3:
        print("usage: plot.py <results.csv> <figures_dir>", file=sys.stderr)
        sys.exit(1)
    csv_path = Path(sys.argv[1])
    out_dir = Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)
    if not csv_path.exists() or csv_path.stat().st_size == 0:
        print(f"warning: {csv_path} missing or empty", file=sys.stderr)
        return
    df = pd.read_csv(csv_path)
    if df.empty:
        print("warning: no data rows", file=sys.stderr)
        return
    agg = aggregate(df)
    workload_value_pairs = (
        agg.assign(workload=agg.apply(workload_label, axis=1))[["workload", "value"]]
        .drop_duplicates()
        .values.tolist()
    )
    for workload, value in workload_value_pairs:
        plot_mops_vs_ways(agg, workload, value, out_dir)
        plot_hr_vs_ways(agg, workload, value, out_dir)
        plot_mops_vs_threads(agg, workload, value, out_dir)
        plot_hr_vs_threads(agg, workload, value, out_dir)
    pairs = pareto_pairs(agg)
    for workload, value in workload_value_pairs:
        plot_pareto_single(pairs, workload, value, out_dir)
    plot_pareto_overlay_all(pairs, out_dir)
    write_summary(pairs, agg, out_dir)
    print(f"plotted {len(workload_value_pairs)} workload-value pairs into {out_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
