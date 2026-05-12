#!/usr/bin/env python3
"""partitioned sweep の領域マップ / pareto / scalability 図を生成する。

入力: data/results.csv (run.sh の出力、append-only)
出力: figures/*.png + summary.md

生成図:
  - fig_mops_vs_threads_<workload>_<value>.png : T scalability、line per (variant, N|ways)
  - fig_heatmap_mops_<workload>_<value>.png    : T × N の Mops heatmap (partitioned 専用)
  - fig_heatmap_hr_<workload>_<value>.png      : T × N の HR heatmap   (partitioned 専用)
  - fig_pareto_<workload>_<value>.png          : HR drop vs Mops gain (vs c17s baseline)
  - fig_pareto_overlay_all.png                 : 全 workload 1 図 overlay
  - summary.md                                 : 採用領域カウント + best cell 表

実行: uv run --project scripts python plot.py data/results.csv figures/
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

ACCEPT_HR_DROP_MAX_PP = 5.0
ACCEPT_MOPS_GAIN_MIN_PCT = 20.0


def workload_label(row) -> str:
    src = row["source"]
    if src == "zipf":
        return f"zipf_s{row['skew']:.1f}_{row['op_mix']}"
    if src in ("twitter", "twitter-yang"):
        return f"twitter_{row['workload_param']}"
    if src == "arc":
        return f"arc_{row['workload_param']}_cap{row['cap']}"
    return src


def variant_axis(row) -> int:
    """variant ごとに「主軸 N」を抽出。partitioned→partitions, r1→ways, others→1."""
    if row["variant"] == "partitioned":
        return int(row["partitions"])
    if row["variant"] == "r1":
        return int(row["ways"])
    return 1


def aggregate(df: pd.DataFrame) -> pd.DataFrame:
    df = df.copy()
    df["workload_param"] = df["workload_param"].fillna("").astype(str)
    keys = [
        "variant",
        "ways",
        "partitions",
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
    agg["workload"] = agg.apply(workload_label, axis=1)
    agg["axis"] = agg.apply(variant_axis, axis=1)
    return agg


def plot_mops_vs_threads(agg: pd.DataFrame, workload: str, value: str, outdir: Path):
    sub = agg[(agg["workload"] == workload) & (agg["value"] == value)]
    if sub.empty:
        return
    fig, ax = plt.subplots(figsize=(8, 5))

    # partitioned: N ごと 1 line
    p_levels = sorted(sub[sub["variant"] == "partitioned"]["partitions"].unique())
    cmap_p = plt.colormaps.get_cmap("Blues").resampled(max(len(p_levels) + 2, 3))
    for i, n in enumerate(p_levels):
        rows = sub[(sub["variant"] == "partitioned") & (sub["partitions"] == n)].sort_values("threads")
        if rows.empty:
            continue
        ax.errorbar(
            rows["threads"], rows["mops_median"], yerr=rows["mops_std"],
            marker="o", color=cmap_p(i + 1), label=f"partitioned N={n}",
            capsize=2, linewidth=1.4,
        )

    # r1: WAYS ごと 1 line
    w_levels = sorted(sub[sub["variant"] == "r1"]["ways"].unique())
    cmap_r = plt.colormaps.get_cmap("Oranges").resampled(max(len(w_levels) + 2, 3))
    for i, w in enumerate(w_levels):
        rows = sub[(sub["variant"] == "r1") & (sub["ways"] == w)].sort_values("threads")
        if rows.empty:
            continue
        ax.errorbar(
            rows["threads"], rows["mops_median"], yerr=rows["mops_std"],
            marker="^", color=cmap_r(i + 1), label=f"r1 w={w}",
            capsize=2, linewidth=1.2, linestyle=":",
        )

    base = sub[(sub["variant"] == "c17s")].sort_values("threads")
    if not base.empty:
        ax.errorbar(
            base["threads"], base["mops_median"], yerr=base["mops_std"],
            marker="s", color="black", linestyle="--", label="c17s",
            capsize=2, linewidth=1.6,
        )

    ax.set_xscale("log", base=2)
    ax.set_xlabel("threads (T)")
    ax.set_ylabel("aggregate Mops")
    ax.set_title(f"Mops vs T — {workload} ({value})")
    ax.legend(loc="best", fontsize=8, ncol=2)
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    fig.savefig(outdir / f"fig_mops_vs_threads_{workload}_{value}.png", dpi=140, bbox_inches="tight")
    plt.close(fig)


def plot_heatmap(agg: pd.DataFrame, workload: str, value: str, metric: str, outdir: Path):
    sub = agg[(agg["variant"] == "partitioned") & (agg["workload"] == workload) & (agg["value"] == value)]
    if sub.empty:
        return
    pivot = sub.pivot_table(index="partitions", columns="threads", values=metric, aggfunc="median")
    if pivot.empty:
        return
    fig, ax = plt.subplots(figsize=(6, 5))
    im = ax.imshow(pivot.values, aspect="auto", origin="lower", cmap="viridis")
    ax.set_xticks(range(len(pivot.columns)), labels=pivot.columns)
    ax.set_yticks(range(len(pivot.index)), labels=pivot.index)
    ax.set_xlabel("threads (T)")
    ax.set_ylabel("partitions (N)")
    label = "Mops" if metric == "mops_median" else "hit ratio"
    ax.set_title(f"partitioned {label} — {workload} ({value})")
    for i in range(pivot.shape[0]):
        for j in range(pivot.shape[1]):
            v = pivot.values[i, j]
            if not np.isnan(v):
                ax.text(j, i, f"{v:.2f}" if metric == "mops_median" else f"{v:.2f}",
                        ha="center", va="center", fontsize=8,
                        color="white" if v < pivot.values[~np.isnan(pivot.values)].mean() else "black")
    fig.colorbar(im, ax=ax, label=label)
    kind = "mops" if metric == "mops_median" else "hr"
    fig.tight_layout()
    fig.savefig(outdir / f"fig_heatmap_{kind}_{workload}_{value}.png", dpi=140, bbox_inches="tight")
    plt.close(fig)


def pareto_pairs(agg: pd.DataFrame) -> pd.DataFrame:
    """各 (workload, value, T) について c17s baseline と partitioned / r1 を pair。"""
    base_keys = ["workload", "value", "threads"]
    c17s = agg[agg["variant"] == "c17s"][base_keys + ["mops_median", "hr_median"]].rename(
        columns={"mops_median": "mops_c17s", "hr_median": "hr_c17s"}
    )
    cand = agg[agg["variant"].isin(["partitioned", "r1"])][
        base_keys + ["variant", "axis", "mops_median", "hr_median", "cv_median"]
    ].rename(columns={"mops_median": "mops", "hr_median": "hr"})
    merged = cand.merge(c17s, on=base_keys, how="inner")
    merged["mops_gain_pct"] = (merged["mops"] / merged["mops_c17s"] - 1) * 100
    merged["hr_drop_pp"] = (merged["hr_c17s"] - merged["hr"]) * 100
    return merged


def plot_pareto_single(pairs: pd.DataFrame, workload: str, value: str, outdir: Path):
    sub = pairs[(pairs["workload"] == workload) & (pairs["value"] == value)]
    if sub.empty:
        return
    fig, ax = plt.subplots(figsize=(7, 6))
    for variant, color in [("partitioned", "tab:blue"), ("r1", "tab:orange")]:
        v_sub = sub[sub["variant"] == variant]
        if v_sub.empty:
            continue
        ax.scatter(
            v_sub["hr_drop_pp"], v_sub["mops_gain_pct"],
            c=color, alpha=0.7, s=50, edgecolor="black", linewidth=0.5,
            label=variant,
        )
    ax.axhline(0, color="black", linewidth=0.8)
    ax.axvline(0, color="black", linewidth=0.8)
    xlim = ax.get_xlim()
    ylim = ax.get_ylim()
    ax.fill_betweenx(
        [ACCEPT_MOPS_GAIN_MIN_PCT, max(ylim[1], ACCEPT_MOPS_GAIN_MIN_PCT + 10)],
        xlim[0], ACCEPT_HR_DROP_MAX_PP,
        color="green", alpha=0.10,
        label=f"accept zone (HR drop ≤{ACCEPT_HR_DROP_MAX_PP}pp, Mops ≥+{ACCEPT_MOPS_GAIN_MIN_PCT}%)",
    )
    ax.set_xlabel("HR drop vs c17s (pp; +→ worse)")
    ax.set_ylabel("Mops gain vs c17s (%; +→ better)")
    ax.set_title(f"Pareto vs c17s — {workload} ({value})")
    ax.legend(loc="best", fontsize=8)
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    fig.savefig(outdir / f"fig_pareto_{workload}_{value}.png", dpi=140, bbox_inches="tight")
    plt.close(fig)


def plot_pareto_overlay_all(pairs: pd.DataFrame, outdir: Path):
    if pairs.empty:
        return
    fig, axes = plt.subplots(1, 2, figsize=(13, 6), sharey=True)
    for ax, variant in zip(axes, ["partitioned", "r1"]):
        sub = pairs[pairs["variant"] == variant]
        wls = sorted((sub["workload"] + "/" + sub["value"]).unique())
        cmap = plt.colormaps.get_cmap("tab20").resampled(max(len(wls), 2))
        for i, wv in enumerate(wls):
            wl, val = wv.rsplit("/", 1)
            s = sub[(sub["workload"] == wl) & (sub["value"] == val)]
            ax.scatter(s["hr_drop_pp"], s["mops_gain_pct"],
                       color=cmap(i), alpha=0.55, s=30, label=wv)
        ax.axhline(0, color="black", linewidth=0.8)
        ax.axvline(0, color="black", linewidth=0.8)
        xlim = ax.get_xlim()
        ylim = ax.get_ylim()
        ax.fill_betweenx(
            [ACCEPT_MOPS_GAIN_MIN_PCT, max(ylim[1], ACCEPT_MOPS_GAIN_MIN_PCT + 10)],
            xlim[0], ACCEPT_HR_DROP_MAX_PP, color="green", alpha=0.10,
        )
        ax.set_xlabel("HR drop vs c17s (pp)")
        ax.set_title(f"{variant} pareto overlay (all workloads)")
        ax.grid(True, alpha=0.3)
        ax.legend(loc="best", fontsize=6, ncol=2)
    axes[0].set_ylabel("Mops gain vs c17s (%)")
    fig.tight_layout()
    fig.savefig(outdir / "fig_pareto_overlay_all.png", dpi=140, bbox_inches="tight")
    plt.close(fig)


def fmt_table(df: pd.DataFrame, float_cols: list[str]) -> str:
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


def write_summary(pairs: pd.DataFrame, agg: pd.DataFrame, outdir: Path):
    lines = ["# partitioned sweep summary", ""]
    if pairs.empty:
        lines.append("(no pareto pairs — bench データが空)")
        (outdir / "summary.md").write_text("\n".join(lines))
        return

    accept = pairs[
        (pairs["hr_drop_pp"] <= ACCEPT_HR_DROP_MAX_PP)
        & (pairs["mops_gain_pct"] >= ACCEPT_MOPS_GAIN_MIN_PCT)
    ]
    by_variant = pairs.groupby("variant").size()
    accept_by_variant = accept.groupby("variant").size()
    lines.append("## 採用領域 (accept zone)")
    lines.append("")
    lines.append(f"基準: HR drop ≤ {ACCEPT_HR_DROP_MAX_PP}pp **AND** Mops gain ≥ +{ACCEPT_MOPS_GAIN_MIN_PCT}% (vs c17s)")
    lines.append("")
    lines.append("| variant | total cells | accept cells | accept rate |")
    lines.append("|---|---|---|---|")
    for v in sorted(by_variant.index):
        tot = int(by_variant.get(v, 0))
        acc = int(accept_by_variant.get(v, 0))
        rate = acc / tot * 100 if tot else 0
        lines.append(f"| {v} | {tot} | {acc} | {rate:.1f}% |")

    cols = ["workload", "value", "threads", "variant", "axis", "hr_drop_pp", "mops_gain_pct", "mops", "mops_c17s"]
    fcols = ["hr_drop_pp", "mops_gain_pct", "mops", "mops_c17s"]

    lines.append("")
    lines.append("## 鍵となる contrast cells (設計書 §sweep)")
    lines.append("")
    key_cells = []
    # 1. T=16, N=16, Zipf 1.4 read-heavy u64 — uncontended ceiling
    key_cells.append(("zipf_s1.4_read-heavy", "u64", 16, "partitioned", 16, "uncontended ceiling"))
    # 2. T=16, N=1 — degenerate
    key_cells.append(("zipf_s1.4_read-heavy", "u64", 16, "partitioned", 1, "degenerate (1 mutex)"))
    # 3. T=4, N=16
    key_cells.append(("zipf_s1.4_read-heavy", "u64", 4, "partitioned", 16, "T<N surplus"))
    # 4. T=16, N=16 ARC OLTP — HR penalty
    key_cells.append(("arc_OLTP_cap4000", "u64", 16, "partitioned", 16, "HR-sensitive"))
    # 5. T=16, N=16 Twitter cluster019
    key_cells.append(("twitter_cluster019", "u64", 16, "partitioned", 16, "HR-tolerant"))

    rows = []
    for wl, val, t, var, ax, note in key_cells:
        match = pairs[(pairs["workload"] == wl) & (pairs["value"] == val)
                      & (pairs["threads"] == t) & (pairs["variant"] == var)
                      & (pairs["axis"] == ax)]
        if not match.empty:
            r = match.iloc[0]
            rows.append({
                "note": note, "workload": wl, "T": t, "N|w": ax,
                "Mops": round(r["mops"], 2),
                "c17s_Mops": round(r["mops_c17s"], 2),
                "Mops_gain_%": round(r["mops_gain_pct"], 1),
                "HR_drop_pp": round(r["hr_drop_pp"], 2),
            })
    if rows:
        df = pd.DataFrame(rows)
        lines.append(fmt_table(df, ["Mops", "c17s_Mops", "Mops_gain_%", "HR_drop_pp"]))

    lines.append("")
    lines.append("## variant 別 best cell (Mops gain top-1 per workload/value)")
    lines.append("")
    for variant in ["partitioned", "r1"]:
        vp = pairs[pairs["variant"] == variant]
        if vp.empty:
            continue
        lines.append(f"### {variant}")
        lines.append("")
        best = vp.loc[vp.groupby(["workload", "value"])["mops_gain_pct"].idxmax()][cols].sort_values(
            "mops_gain_pct", ascending=False
        )
        lines.append(fmt_table(best, fcols))
        lines.append("")

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
        return
    agg = aggregate(df)
    pairs = pareto_pairs(agg)
    wv = agg[["workload", "value"]].drop_duplicates().values.tolist()
    for workload, value in wv:
        plot_mops_vs_threads(agg, workload, value, out_dir)
        plot_heatmap(agg, workload, value, "mops_median", out_dir)
        plot_heatmap(agg, workload, value, "hr_median", out_dir)
        plot_pareto_single(pairs, workload, value, out_dir)
    plot_pareto_overlay_all(pairs, out_dir)
    write_summary(pairs, agg, out_dir)
    print(f"plotted {len(wv)} workload-value pairs into {out_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
