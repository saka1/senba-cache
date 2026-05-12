#!/usr/bin/env python3
"""r1 vs moka sweep の図と summary を生成する。

入力: data/results.csv (run.sh 出力)
出力:
  - figures/fig_mops_vs_threads__<workload>__<value>.png
  - figures/fig_p99_vs_threads__<workload>__<value>.png
  - figures/fig_hr_vs_threads__<workload>__<value>.png
  - figures/summary.md (T=16 比較表 + variant best/worst cell)

実行: uv run --project scripts python plot.py data/results.csv figures/
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd


# variant_label: 凡例で variant + ways を区別。r1 のみ ways を表示。
VARIANT_STYLE = {
    "c17s": {"color": "#444444", "marker": "o", "linestyle": "-"},
    "r1@1": {"color": "#1f77b4", "marker": "s", "linestyle": "--"},
    "r1@8": {"color": "#2ca02c", "marker": "^", "linestyle": "-"},
    "moka": {"color": "#d62728", "marker": "x", "linestyle": "-"},
    "mini_moka": {"color": "#ff7f0e", "marker": "+", "linestyle": "--"},
}
VARIANT_ORDER = ["c17s", "r1@1", "r1@8", "moka", "mini_moka"]


def workload_label(row) -> str:
    src = row["source"]
    if src == "zipf":
        return f"zipf_s{row['skew']:.1f}_{row['op_mix']}"
    if src in ("twitter", "twitter-yang"):
        return f"twitter_{row['workload_param']}"
    if src == "arc":
        return f"arc_{row['workload_param']}_cap{row['cap']}"
    return src


def variant_label(row) -> str:
    if row["variant"] == "r1":
        return f"r1@{int(row['ways'])}"
    return row["variant"]


def aggregate(df: pd.DataFrame) -> pd.DataFrame:
    df = df.copy()
    df["workload_param"] = df["workload_param"].fillna("").astype(str)
    df["workload"] = df.apply(workload_label, axis=1)
    df["variant_lbl"] = df.apply(variant_label, axis=1)
    keys = ["variant_lbl", "workload", "value", "threads"]
    agg = (
        df.groupby(keys, dropna=False)
        .agg(
            mops_median=("aggregate_mops", "median"),
            mops_std=("aggregate_mops", "std"),
            hr_median=("hit_ratio", "median"),
            p99_median=("p99_chunk_ns", "median"),
            p50_median=("p50_chunk_ns", "median"),
            cv_median=("thread_throughput_cv", "median"),
            n_trials=("trial", "count"),
        )
        .reset_index()
    )
    agg["mops_std"] = agg["mops_std"].fillna(0.0)
    return agg


def plot_metric_vs_threads(agg: pd.DataFrame, metric: str, ylabel: str, fname_prefix: str, outdir: Path, log_y: bool = False):
    for (wl, val), sub in agg.groupby(["workload", "value"]):
        fig, ax = plt.subplots(figsize=(6.5, 4.2))
        for vlbl in VARIANT_ORDER:
            vs = sub[sub["variant_lbl"] == vlbl].sort_values("threads")
            if vs.empty:
                continue
            style = VARIANT_STYLE.get(vlbl, {"color": "gray", "marker": "o", "linestyle": "-"})
            ax.plot(vs["threads"], vs[metric], label=vlbl, **style)
        ax.set_xlabel("threads")
        ax.set_ylabel(ylabel)
        ax.set_title(f"{wl} / value={val}")
        if log_y:
            ax.set_yscale("log")
        ax.set_xscale("log", base=2)
        ax.set_xticks([1, 2, 4, 8, 16])
        ax.set_xticklabels(["1", "2", "4", "8", "16"])
        ax.grid(True, alpha=0.3)
        ax.legend(loc="best", fontsize=8)
        fig.tight_layout()
        safe_wl = wl.replace("/", "_")
        fig.savefig(outdir / f"{fname_prefix}__{safe_wl}__{val}.png", dpi=140, bbox_inches="tight")
        plt.close(fig)


def fmt_table(df: pd.DataFrame, float_cols: list[str]) -> str:
    hdrs = list(df.columns)
    out = ["| " + " | ".join(hdrs) + " |", "|" + "|".join(["---"] * len(hdrs)) + "|"]
    for _, row in df.iterrows():
        cells = []
        for c in hdrs:
            v = row[c]
            if c in float_cols and isinstance(v, (int, float, np.floating, np.integer)):
                cells.append(f"{float(v):.2f}")
            elif isinstance(v, (np.floating, np.integer)):
                cells.append(str(v.item()))
            else:
                cells.append(str(v))
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


def pivot_metric(agg: pd.DataFrame, metric: str, threads: int) -> pd.DataFrame:
    sub = agg[agg["threads"] == threads]
    if sub.empty:
        return pd.DataFrame()
    pv = sub.pivot_table(
        index=["workload", "value"],
        columns="variant_lbl",
        values=metric,
        aggfunc="first",
    ).reset_index()
    return pv


def write_summary(agg: pd.DataFrame, outdir: Path):
    lines = ["# r1 vs moka sweep summary", ""]
    lines.append("variants: c17s (baseline @ ways=1) / r1@ways={1,8} / moka 0.12 sync / mini_moka 0.10 sync")
    lines.append("")
    lines.append(f"observations: {len(agg)} cells aggregated from raw CSV")
    lines.append("")

    # ----- T=16 Mops table -----
    lines.append("## T=16 Mops")
    lines.append("")
    pv = pivot_metric(agg, "mops_median", 16)
    if not pv.empty:
        cols = ["workload", "value"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(2)
        # r1@8 / moka 比
        if "r1@8" in pv.columns and "moka" in pv.columns:
            pv["r1@8/moka"] = (pv["r1@8"] / pv["moka"]).round(2)
        pv = pv.sort_values(["value", "workload"])
        lines.append(fmt_table(pv, cols[2:] + (["r1@8/moka"] if "r1@8/moka" in pv.columns else [])))
    lines.append("")

    # ----- T=16 p99 ns -----
    lines.append("## T=16 p99 chunk latency (ns)")
    lines.append("")
    pv = pivot_metric(agg, "p99_median", 16)
    if not pv.empty:
        cols = ["workload", "value"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(0).astype("Int64")
        if "moka" in pv.columns and "r1@8" in pv.columns:
            pv["moka/r1@8"] = (pv["moka"].astype(float) / pv["r1@8"].astype(float)).round(2)
        pv = pv.sort_values(["value", "workload"])
        lines.append(fmt_table(pv, ["moka/r1@8"] if "moka/r1@8" in pv.columns else []))
    lines.append("")

    # ----- T=16 HR sanity -----
    lines.append("## T=16 hit ratio")
    lines.append("")
    pv = pivot_metric(agg, "hr_median", 16)
    if not pv.empty:
        cols = ["workload", "value"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(3)
        pv = pv.sort_values(["value", "workload"])
        lines.append(fmt_table(pv, cols[2:]))
    lines.append("")

    # ----- best/worst cells for r1@8 vs moka -----
    if "r1@8" in agg["variant_lbl"].values and "moka" in agg["variant_lbl"].values:
        wide = agg.pivot_table(
            index=["workload", "value", "threads"],
            columns="variant_lbl",
            values="mops_median",
            aggfunc="first",
        ).reset_index()
        if "r1@8" in wide.columns and "moka" in wide.columns:
            wide = wide.dropna(subset=["r1@8", "moka"])
            wide["r1@8/moka"] = wide["r1@8"] / wide["moka"]
            wide = wide.sort_values("r1@8/moka", ascending=False)

            lines.append("## r1@8 / moka 比 top-10 (どの cell で r1@8 が moka より勝つか)")
            lines.append("")
            top = wide.head(10)[["workload", "value", "threads", "r1@8", "moka", "r1@8/moka"]].copy()
            top["r1@8"] = top["r1@8"].round(2)
            top["moka"] = top["moka"].round(2)
            top["r1@8/moka"] = top["r1@8/moka"].round(2)
            lines.append(fmt_table(top, ["r1@8", "moka", "r1@8/moka"]))
            lines.append("")

            lines.append("## r1@8 / moka 比 bottom-10 (どの cell で r1@8 が moka に近い/負ける)")
            lines.append("")
            bot = wide.tail(10)[["workload", "value", "threads", "r1@8", "moka", "r1@8/moka"]].copy()
            bot["r1@8"] = bot["r1@8"].round(2)
            bot["moka"] = bot["moka"].round(2)
            bot["r1@8/moka"] = bot["r1@8/moka"].round(2)
            lines.append(fmt_table(bot, ["r1@8", "moka", "r1@8/moka"]))
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
    plot_metric_vs_threads(agg, "mops_median", "aggregate Mops/s", "fig_mops_vs_threads", out_dir)
    plot_metric_vs_threads(agg, "p99_median", "p99 chunk latency (ns)", "fig_p99_vs_threads", out_dir, log_y=True)
    plot_metric_vs_threads(agg, "hr_median", "hit ratio", "fig_hr_vs_threads", out_dir)
    write_summary(agg, out_dir)
    n_figs = len(list(out_dir.glob("fig_*.png")))
    print(f"summary written: {out_dir}/summary.md ({n_figs} figures)", file=sys.stderr)


if __name__ == "__main__":
    main()
