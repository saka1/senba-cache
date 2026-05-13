#!/usr/bin/env python3
"""r1 vs moka — cap-axis sweep の図と summary を生成する。

入力: data/results.csv
出力:
  - figures/fig_mops_vs_cap__<workload>__T<T>.png  variant overlay
  - figures/fig_hr_vs_cap__<workload>__T<T>.png   variant overlay
  - figures/fig_p99_vs_cap__<workload>__T<T>.png  variant overlay (log y)
  - figures/summary.md (per-workload best-cap 表 + HR gap @ large cap)

実行: uv run --project scripts python plot.py data/results.csv figures/
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

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
        return f"arc_{row['workload_param']}"
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
    keys = ["variant_lbl", "workload", "value", "threads", "cap"]
    agg = (
        df.groupby(keys, dropna=False)
        .agg(
            mops_median=("aggregate_mops", "median"),
            mops_std=("aggregate_mops", "std"),
            hr_median=("hit_ratio", "median"),
            p99_median=("p99_chunk_ns", "median"),
            p50_median=("p50_chunk_ns", "median"),
            n_trials=("trial", "count"),
        )
        .reset_index()
    )
    agg["mops_std"] = agg["mops_std"].fillna(0.0)
    return agg


def plot_metric_vs_cap(agg: pd.DataFrame, metric: str, ylabel: str, fname_prefix: str, outdir: Path, log_y: bool = False):
    for (wl, T), sub in agg.groupby(["workload", "threads"]):
        if sub["cap"].nunique() < 2:
            # cap 軸 1 点しかない (e.g. ARC OLTP の cap=256 だけ取った場合) → skip
            continue
        fig, ax = plt.subplots(figsize=(6.5, 4.2))
        for vlbl in VARIANT_ORDER:
            vs = sub[sub["variant_lbl"] == vlbl].sort_values("cap")
            if vs.empty:
                continue
            style = VARIANT_STYLE.get(vlbl, {"color": "gray", "marker": "o", "linestyle": "-"})
            ax.plot(vs["cap"], vs[metric], label=vlbl, **style)
        ax.set_xlabel("cap")
        ax.set_ylabel(ylabel)
        ax.set_title(f"{wl} / T={T}")
        ax.set_xscale("log", base=2)
        if log_y:
            ax.set_yscale("log")
        ax.grid(True, alpha=0.3)
        ax.legend(loc="best", fontsize=8)
        fig.tight_layout()
        safe_wl = wl.replace("/", "_")
        fig.savefig(outdir / f"{fname_prefix}__{safe_wl}__T{T}.png", dpi=140, bbox_inches="tight")
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
            elif pd.isna(v):
                cells.append("-")
            else:
                cells.append(str(v))
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


def write_summary(agg: pd.DataFrame, outdir: Path):
    lines = ["# r1 vs moka cap-axis sweep summary", ""]
    lines.append("variants: c17s (baseline @ ways=1) / r1@ways={1,8} / moka 0.12 sync / mini_moka 0.10 sync")
    lines.append("")
    lines.append(f"observations: {len(agg)} cells")
    lines.append("")

    # 各 (workload, T) で variant 別 HR / Mops を cap で並べる。
    # external-lib-sweep.md の T=1 と本書 T={4,8,16} を視覚的に並べやすい形にする。

    # T=16 hit ratio @ each (workload, cap) を pivot
    sub = agg[agg["threads"] == 16]
    if not sub.empty:
        lines.append("## T=16 hit ratio @ each (workload, cap)")
        lines.append("")
        pv = sub.pivot_table(
            index=["workload", "cap"], columns="variant_lbl", values="hr_median", aggfunc="first"
        ).reset_index().sort_values(["workload", "cap"])
        cols = ["workload", "cap"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(3)
        if "moka" in pv.columns and "c17s" in pv.columns:
            pv["moka−c17s (pp)"] = ((pv["moka"] - pv["c17s"]) * 100).round(2)
        lines.append(fmt_table(pv, cols[2:] + (["moka−c17s (pp)"] if "moka−c17s (pp)" in pv.columns else [])))
        lines.append("")

    # T=16 Mops @ each (workload, cap)
    if not sub.empty:
        lines.append("## T=16 aggregate Mops @ each (workload, cap)")
        lines.append("")
        pv = sub.pivot_table(
            index=["workload", "cap"], columns="variant_lbl", values="mops_median", aggfunc="first"
        ).reset_index().sort_values(["workload", "cap"])
        cols = ["workload", "cap"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(2)
        if "r1@8" in pv.columns and "moka" in pv.columns:
            pv["r1@8/moka"] = (pv["r1@8"] / pv["moka"]).round(2)
        lines.append(fmt_table(pv, cols[2:] + (["r1@8/moka"] if "r1@8/moka" in pv.columns else [])))
        lines.append("")

    # T=16 p99 ns
    if not sub.empty:
        lines.append("## T=16 p99 chunk latency (ns) @ each (workload, cap)")
        lines.append("")
        pv = sub.pivot_table(
            index=["workload", "cap"], columns="variant_lbl", values="p99_median", aggfunc="first"
        ).reset_index().sort_values(["workload", "cap"])
        cols = ["workload", "cap"] + [c for c in VARIANT_ORDER if c in pv.columns]
        pv = pv[cols].copy()
        for c in cols[2:]:
            pv[c] = pv[c].round(0).astype("Int64")
        lines.append(fmt_table(pv, []))
        lines.append("")

    # HR gap analysis: どの (workload, cap) で moka HR > senba HR か (external-lib-sweep の構造再現)
    if "moka" in agg["variant_lbl"].values and "c17s" in agg["variant_lbl"].values:
        wide = agg[agg["threads"] == 16].pivot_table(
            index=["workload", "cap"],
            columns="variant_lbl",
            values="hr_median",
            aggfunc="first",
        ).reset_index()
        if "moka" in wide.columns and "c17s" in wide.columns:
            wide["moka−c17s (pp)"] = (wide["moka"] - wide["c17s"]) * 100
            wins = wide[wide["moka−c17s (pp)"] >= 2.0].sort_values("moka−c17s (pp)", ascending=False)
            if not wins.empty:
                lines.append("## moka が c17s より HR で +2pp 以上勝つ (workload, cap) — policy 層 HR drop 検証")
                lines.append("")
                lines.append("`external-lib-sweep.md` で単スレ計測した SIEVE vs W-TinyLFU の HR 反転帯が T=16 で再現するかを直接観測。")
                lines.append("")
                cols = ["workload", "cap", "c17s", "moka", "moka−c17s (pp)"]
                out = wins[cols].copy()
                out["c17s"] = out["c17s"].round(3)
                out["moka"] = out["moka"].round(3)
                out["moka−c17s (pp)"] = out["moka−c17s (pp)"].round(2)
                lines.append(fmt_table(out, []))
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
    plot_metric_vs_cap(agg, "mops_median", "aggregate Mops/s", "fig_mops_vs_cap", out_dir)
    plot_metric_vs_cap(agg, "hr_median", "hit ratio", "fig_hr_vs_cap", out_dir)
    plot_metric_vs_cap(agg, "p99_median", "p99 chunk latency (ns)", "fig_p99_vs_cap", out_dir, log_y=True)
    write_summary(agg, out_dir)
    n_figs = len(list(out_dir.glob("fig_*.png")))
    print(f"summary written: {out_dir}/summary.md ({n_figs} figures)", file=sys.stderr)


if __name__ == "__main__":
    main()
