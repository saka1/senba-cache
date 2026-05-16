#!/usr/bin/env python3
"""README headline benchmark plot.

Input  : data/results.csv + data/results_serial.csv (run.sh outputs)
Outputs (figures/):
  - throughput.png         : Mops vs threads, concurrent headline chart
  - latency.png            : p50 / p99 chunk latency vs threads (log y)
  - hit_ratio.png          : hit ratio vs threads — parity check
  - serial_throughput.png  : single-thread Mops, senba::Cache vs mini-moka / lru
  - serial_hit_ratio.png   : single-thread hit ratio parity check
  - summary.md             : per-cell means, ratios, and 1T → 4T scaling

Run    : uv run --project scripts python docs/benchmark/readme-headline/plot.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
from matplotlib import rcParams

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "results.csv"
SERIAL_DATA = HERE / "data" / "results_serial.csv"
FIG = HERE / "figures"
FIG.mkdir(exist_ok=True)

VARIANTS = ["senba_concurrent", "moka", "mini_moka"]
LABELS = {
    "senba_concurrent": "senba::concurrent",
    "moka": "moka",
    "mini_moka": "mini-moka",
}
# Warm primary for senba; cool neutrals for the others so the eye locks onto
# the headline bar first.
COLORS = {
    "senba_concurrent": "#e8543a",
    "moka": "#4c78a8",
    "mini_moka": "#9aa0a6",
}

SERIAL_VARIANTS = ["senba", "mini_moka_unsync", "lru"]
SERIAL_LABELS = {
    "senba": "senba::Cache",
    "mini_moka_unsync": "mini-moka (unsync)",
    "lru": "lru-rs",
}
SERIAL_COLORS = {
    "senba": "#e8543a",
    "mini_moka_unsync": "#9aa0a6",
    "lru": "#4c78a8",
}

# Shared style tuned for README rendering on GitHub (light bg, ~720px wide).
rcParams.update(
    {
        "font.family": "DejaVu Sans",
        "font.size": 11,
        "axes.titlesize": 12,
        "axes.labelsize": 11,
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.edgecolor": "#444",
        "axes.labelcolor": "#222",
        "xtick.color": "#444",
        "ytick.color": "#444",
        "axes.grid": True,
        "grid.color": "#dddddd",
        "grid.linewidth": 0.6,
        "figure.facecolor": "white",
        "savefig.facecolor": "white",
        "savefig.bbox": "tight",
    }
)


def load() -> pd.DataFrame:
    if not DATA.exists():
        sys.exit(f"missing {DATA}; run run.sh first")
    df = pd.read_csv(DATA)
    if df.empty:
        sys.exit("results.csv has no data rows")
    return df


def load_serial() -> pd.DataFrame | None:
    if not SERIAL_DATA.exists():
        print(f"note: {SERIAL_DATA} not found; skipping serial plots", file=sys.stderr)
        return None
    df = pd.read_csv(SERIAL_DATA)
    if df.empty:
        print(f"note: {SERIAL_DATA} has no data rows; skipping serial plots", file=sys.stderr)
        return None
    # bench.rs emits totals — derive throughput / hit ratio per row.
    df["aggregate_mops"] = df["len"] * 1000.0 / df["elapsed_ns"]
    df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])
    return df


def _mean(sub: pd.DataFrame, col: str, variant: str) -> float:
    cell = sub[sub["variant"] == variant][col]
    return float(cell.mean()) if not cell.empty else float("nan")


def plot_throughput(df: pd.DataFrame) -> None:
    threads = sorted(df["threads"].unique())
    x = np.arange(len(threads))
    w = 0.26

    fig, ax = plt.subplots(figsize=(7.6, 4.4))
    ax.set_axisbelow(True)
    ax.grid(axis="x", visible=False)

    # Average across trials. CV is small (<5%) so error bars add noise
    # without informing the headline; we show the trial-mean instead.
    series = {v: [] for v in VARIANTS}
    for t in threads:
        sub = df[df["threads"] == t]
        for v in VARIANTS:
            series[v].append(_mean(sub, "aggregate_mops", v))

    for k, v in enumerate(VARIANTS):
        offset = (k - 1) * w
        bars = ax.bar(
            x + offset,
            series[v],
            w,
            label=LABELS[v],
            color=COLORS[v],
            edgecolor="white",
            linewidth=0.6,
        )
        for bar, m in zip(bars, series[v]):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                bar.get_height() + 0.6,
                f"{m:.1f}",
                ha="center",
                va="bottom",
                fontsize=9,
                color="#222",
            )

    senba_means = series["senba_concurrent"]
    ax.set_ylim(0, max(senba_means) * 1.10)

    ax.set_xticks(x)
    ax.set_xticklabels([f"{t} thread" + ("" if t == 1 else "s") for t in threads])
    ax.set_ylabel("throughput  (million ops / sec)")
    ax.set_title(
        "Concurrent cache throughput — read-heavy Zipf α=1.0",
        loc="left",
        pad=10,
        fontweight="bold",
    )

    ax.legend(
        loc="upper left",
        frameon=False,
        fontsize=10,
        ncol=3,
        bbox_to_anchor=(0.0, -0.12),
    )

    fig.tight_layout(rect=(0, 0.02, 1, 0.97))
    out = FIG / "throughput.png"
    fig.savefig(out, dpi=160)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def plot_latency(df: pd.DataFrame) -> None:
    threads = sorted(df["threads"].unique())
    fig, axes = plt.subplots(1, 2, figsize=(9.6, 4.0), sharey=True)

    for ax, col, title in (
        (axes[0], "p50_chunk_ns", "p50 latency"),
        (axes[1], "p99_chunk_ns", "p99 latency"),
    ):
        ax.set_axisbelow(True)
        for v in VARIANTS:
            ys = [_mean(df[df["threads"] == t], col, v) for t in threads]
            ax.plot(
                threads,
                ys,
                marker="o",
                markersize=6,
                label=LABELS[v],
                color=COLORS[v],
                linewidth=2.2,
            )
            for tx, ty in zip(threads, ys):
                ax.annotate(
                    f"{ty:.0f}",
                    xy=(tx, ty),
                    xytext=(0, 6),
                    textcoords="offset points",
                    ha="center",
                    fontsize=8,
                    color="#333",
                )
        ax.set_xticks(threads)
        ax.set_xlabel("threads")
        ax.set_yscale("log")
        ax.set_title(title, loc="left", fontweight="bold")
        ax.grid(True, which="both", alpha=0.3)

    axes[0].set_ylabel("nanoseconds")
    axes[0].legend(loc="upper left", frameon=False, fontsize=9)
    fig.suptitle(
        "Per-thread chunk latency — read-heavy Zipf α=1.0",
        fontsize=12,
        fontweight="bold",
        x=0.02,
        ha="left",
    )
    fig.tight_layout(rect=(0, 0, 1, 0.94))
    out = FIG / "latency.png"
    fig.savefig(out, dpi=160)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def plot_hit_ratio(df: pd.DataFrame) -> None:
    threads = sorted(df["threads"].unique())
    x = np.arange(len(threads))
    w = 0.26

    fig, ax = plt.subplots(figsize=(7.6, 4.0))
    ax.set_axisbelow(True)
    ax.grid(axis="x", visible=False)

    series = {v: [] for v in VARIANTS}
    for t in threads:
        sub = df[df["threads"] == t]
        for v in VARIANTS:
            series[v].append(_mean(sub, "hit_ratio", v))

    for k, v in enumerate(VARIANTS):
        offset = (k - 1) * w
        bars = ax.bar(
            x + offset,
            series[v],
            w,
            label=LABELS[v],
            color=COLORS[v],
            edgecolor="white",
            linewidth=0.6,
        )
        for bar, m in zip(bars, series[v]):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                bar.get_height() + 0.012,
                f"{m:.3f}",
                ha="center",
                va="bottom",
                fontsize=9,
                color="#222",
            )

    ax.set_ylim(0.0, 1.0)
    ax.set_yticks(np.linspace(0.0, 1.0, 6))

    ax.set_xticks(x)
    ax.set_xticklabels([f"{t} thread" + ("" if t == 1 else "s") for t in threads])
    ax.set_ylabel("hit ratio")
    ax.set_title(
        "Hit ratio — read-heavy Zipf α=1.0, cap=4096, 100k keys",
        loc="left",
        pad=10,
        fontweight="bold",
    )
    ax.legend(
        loc="upper left",
        frameon=False,
        fontsize=10,
        ncol=3,
        bbox_to_anchor=(0.0, -0.12),
    )

    fig.tight_layout(rect=(0, 0.02, 1, 0.97))
    out = FIG / "hit_ratio.png"
    fig.savefig(out, dpi=160)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def plot_serial_throughput(df: pd.DataFrame) -> None:
    means = [_mean(df, "aggregate_mops", v) for v in SERIAL_VARIANTS]
    x = np.arange(len(SERIAL_VARIANTS))

    fig, ax = plt.subplots(figsize=(7.6, 4.0))
    ax.set_axisbelow(True)
    ax.grid(axis="x", visible=False)

    bars = ax.bar(
        x,
        means,
        0.55,
        color=[SERIAL_COLORS[v] for v in SERIAL_VARIANTS],
        edgecolor="white",
        linewidth=0.6,
    )
    for bar, m in zip(bars, means):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            bar.get_height() + max(means) * 0.012,
            f"{m:.1f}",
            ha="center",
            va="bottom",
            fontsize=10,
            color="#222",
        )

    ax.set_ylim(0, max(means) * 1.12)
    ax.set_xticks(x)
    ax.set_xticklabels([SERIAL_LABELS[v] for v in SERIAL_VARIANTS])
    ax.set_ylabel("throughput  (million ops / sec)")
    ax.set_title(
        "Single-thread cache throughput — read-heavy Zipf α=1.0, cap=4096",
        loc="left",
        pad=10,
        fontweight="bold",
    )

    fig.tight_layout()
    out = FIG / "serial_throughput.png"
    fig.savefig(out, dpi=160)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def plot_serial_hit_ratio(df: pd.DataFrame) -> None:
    means = [_mean(df, "hit_ratio", v) for v in SERIAL_VARIANTS]
    x = np.arange(len(SERIAL_VARIANTS))

    fig, ax = plt.subplots(figsize=(7.6, 4.0))
    ax.set_axisbelow(True)
    ax.grid(axis="x", visible=False)

    bars = ax.bar(
        x,
        means,
        0.55,
        color=[SERIAL_COLORS[v] for v in SERIAL_VARIANTS],
        edgecolor="white",
        linewidth=0.6,
    )
    for bar, m in zip(bars, means):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            bar.get_height() + 0.012,
            f"{m:.3f}",
            ha="center",
            va="bottom",
            fontsize=10,
            color="#222",
        )

    ax.set_ylim(0.0, 1.0)
    ax.set_yticks(np.linspace(0.0, 1.0, 6))
    ax.set_xticks(x)
    ax.set_xticklabels([SERIAL_LABELS[v] for v in SERIAL_VARIANTS])
    ax.set_ylabel("hit ratio")
    ax.set_title(
        "Single-thread hit ratio — Zipf α=1.0, cap=4096, 100k keys",
        loc="left",
        pad=10,
        fontweight="bold",
    )

    fig.tight_layout()
    out = FIG / "serial_hit_ratio.png"
    fig.savefig(out, dpi=160)
    plt.close(fig)
    print(f"wrote {out}", file=sys.stderr)


def write_summary(df: pd.DataFrame, df_serial: pd.DataFrame | None = None) -> None:
    threads = sorted(df["threads"].unique())
    rows = []
    for t in threads:
        sub = df[df["threads"] == t]
        s = _mean(sub, "aggregate_mops", "senba_concurrent")
        mo = _mean(sub, "aggregate_mops", "moka")
        mi = _mean(sub, "aggregate_mops", "mini_moka")
        s_hit = _mean(sub, "hit_ratio", "senba_concurrent")
        mo_hit = _mean(sub, "hit_ratio", "moka")
        mi_hit = _mean(sub, "hit_ratio", "mini_moka")
        s_p99 = _mean(sub, "p99_chunk_ns", "senba_concurrent")
        mo_p99 = _mean(sub, "p99_chunk_ns", "moka")
        mi_p99 = _mean(sub, "p99_chunk_ns", "mini_moka")
        rows.append(
            {
                "T": t,
                "senba Mops": round(s, 2),
                "moka Mops": round(mo, 2),
                "mini Mops": round(mi, 2),
                "senba/moka": f"{s / mo:.2f}x",
                "senba/mini": f"{s / mi:.2f}x",
                "senba hit": round(s_hit, 4),
                "moka hit": round(mo_hit, 4),
                "mini hit": round(mi_hit, 4),
                "senba p99 ns": round(s_p99, 0),
                "moka p99 ns": round(mo_p99, 0),
                "mini p99 ns": round(mi_p99, 0),
            }
        )
    rt = pd.DataFrame(rows)

    out = FIG / "summary.md"
    with out.open("w") as f:
        f.write("# README headline — summary\n\n")
        f.write(
            "AWS c8i.2xlarge (Granite Rapids), 4 physical cores + SMT. "
            "Threads pinned to cpus 0..T-1 (one per physical core; SMT siblings 4-7 unused). "
            "Zipf α=1.0, cap=4096, keys=100k, read-heavy, value=u64. "
            "3 trials per cell, 2.4M ops + 240k warmup each.\n\n"
        )
        cols = list(rt.columns)
        f.write("| " + " | ".join(cols) + " |\n")
        f.write("| " + " | ".join("---" for _ in cols) + " |\n")
        for _, row in rt.iterrows():
            f.write("| " + " | ".join(str(row[c]) for c in cols) + " |\n")

        s1 = rt[rt["T"] == 1]["senba Mops"].iloc[0]
        s4 = rt[rt["T"] == 4]["senba Mops"].iloc[0]
        mo1 = rt[rt["T"] == 1]["moka Mops"].iloc[0]
        mo4 = rt[rt["T"] == 4]["moka Mops"].iloc[0]
        mi1 = rt[rt["T"] == 1]["mini Mops"].iloc[0]
        mi4 = rt[rt["T"] == 4]["mini Mops"].iloc[0]
        f.write("\n## Scaling 1T → 4T\n\n")
        f.write(f"- senba::concurrent : {s1:.2f} → {s4:.2f} Mops ({s4 / s1:.2f}x)\n")
        f.write(f"- moka              : {mo1:.2f} → {mo4:.2f} Mops ({mo4 / mo1:.2f}x)\n")
        f.write(f"- mini-moka         : {mi1:.2f} → {mi4:.2f} Mops ({mi4 / mi1:.2f}x)\n")

        if df_serial is not None:
            s_mops = _mean(df_serial, "aggregate_mops", "senba")
            mi_mops = _mean(df_serial, "aggregate_mops", "mini_moka_unsync")
            lr_mops = _mean(df_serial, "aggregate_mops", "lru")
            s_hit = _mean(df_serial, "hit_ratio", "senba")
            mi_hit = _mean(df_serial, "hit_ratio", "mini_moka_unsync")
            lr_hit = _mean(df_serial, "hit_ratio", "lru")
            f.write("\n## Single-thread (1 core, taskset -c 0)\n\n")
            f.write(
                "senba::Cache vs mini-moka (unsync) vs lru-rs. "
                "Zipf α=1.0, cap=4096, 100k keys, 2M ops, value=u64, 3 trials.\n\n"
            )
            f.write("| variant | Mops | hit ratio | senba ratio |\n")
            f.write("| --- | --- | --- | --- |\n")
            f.write(f"| senba::Cache       | {s_mops:.2f} | {s_hit:.4f} | 1.00x |\n")
            f.write(
                f"| mini-moka (unsync) | {mi_mops:.2f} | {mi_hit:.4f} | {s_mops / mi_mops:.2f}x |\n"
            )
            f.write(
                f"| lru-rs             | {lr_mops:.2f} | {lr_hit:.4f} | {s_mops / lr_mops:.2f}x |\n"
            )
    print(f"wrote {out}", file=sys.stderr)


def main() -> None:
    df = load()
    plot_throughput(df)
    plot_latency(df)
    plot_hit_ratio(df)
    df_serial = load_serial()
    if df_serial is not None:
        plot_serial_throughput(df_serial)
        plot_serial_hit_ratio(df_serial)
    write_summary(df, df_serial)


if __name__ == "__main__":
    main()
