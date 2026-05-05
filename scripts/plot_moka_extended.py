"""Visualize extended sweep: orig vs j8 vs mini-moka vs moka 0.12.

Workloads: 3 Twitter cluster + 4 Zipf skew. Source: profiles/moka_extended_<date>.csv.
"""

from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import seaborn as sns

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "figures"
OUT.mkdir(parents=True, exist_ok=True)
PROFILES = ROOT / "profiles"

CSV = PROFILES / "moka_extended_2026-05-06.csv"

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])
df["family"] = df["variant"].apply(lambda v: "j8" if v.startswith("j8_") else v)

# workload label: cluster name or "zipf skew=N"
def wl_label(row):
    if row["workload"] == "zipf":
        return f"zipf {row['skew']}"
    return row["workload"]
df["wl"] = df.apply(wl_label, axis=1)

agg = (
    df.groupby(["wl", "capacity", "family"], as_index=False)
    .agg(ns_per_op=("ns_per_op", "median"),
         hit_ratio=("hit_ratio", "median"))
)

WORKLOADS = sorted(df["wl"].unique())
CAPS = sorted(df["capacity"].unique())
FAMILIES = ["orig", "j8", "mini_moka", "moka"]
COLORS = {"orig": "#888888", "j8": "#d7301f",
          "mini_moka": "#1f78b4", "moka": "#33a02c"}
LABELS = {"orig": "orig (SIEVE)", "j8": "j8 (SIEVE/ps=32)",
          "mini_moka": "mini-moka 0.10", "moka": "moka 0.12"}

# ---- Plot 1: HR heatmap, rows=workload, cols=cap, one panel per family ----
fig, axes = plt.subplots(1, len(FAMILIES), figsize=(4.0 * len(FAMILIES), 6.5),
                         sharey=True)
for ax, fam in zip(axes, FAMILIES):
    sub = agg[agg["family"] == fam]
    pivot = sub.pivot(index="wl", columns="capacity", values="hit_ratio") * 100
    pivot = pivot.reindex(WORKLOADS)
    sns.heatmap(pivot, annot=True, fmt=".1f", cmap="viridis",
                vmin=0, vmax=100, ax=ax, cbar=ax is axes[-1],
                cbar_kws={"label": "HR (%)"} if ax is axes[-1] else None,
                linewidths=0.4, linecolor="white")
    ax.set_title(LABELS[fam])
    ax.set_xlabel("capacity")
    ax.set_ylabel("workload" if ax is axes[0] else "")
fig.suptitle("Hit ratio (%) — workload × capacity × family", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "moka_extended_hr_grid.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 2: Δ HR vs j8 heatmap (mini-moka and moka deltas) ----
fig, axes = plt.subplots(1, 2, figsize=(11, 6.5), sharey=True)
for ax, fam in zip(axes, ["mini_moka", "moka"]):
    sub_fam = agg[agg["family"] == fam].set_index(["wl", "capacity"])["hit_ratio"]
    sub_j8 = agg[agg["family"] == "j8"].set_index(["wl", "capacity"])["hit_ratio"]
    delta = ((sub_fam - sub_j8) * 100).reset_index().rename(
        columns={"hit_ratio": "dhr"})
    pivot = delta.pivot(index="wl", columns="capacity", values="dhr")
    pivot = pivot.reindex(WORKLOADS)
    vmax = max(abs(pivot.min().min()), abs(pivot.max().max()), 1.0)
    sns.heatmap(pivot, annot=True, fmt=".2f", cmap="RdBu_r", center=0,
                vmin=-vmax, vmax=vmax, ax=ax,
                cbar_kws={"label": f"Δ HR pp ({LABELS[fam]} − j8)"},
                linewidths=0.4, linecolor="white")
    ax.set_title(f"{LABELS[fam]} − j8 (pp)")
    ax.set_xlabel("capacity")
    ax.set_ylabel("workload" if fam == "mini_moka" else "")
fig.suptitle("ΔHR vs j8 (negative = j8 wins; pp)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "moka_extended_dhr_vs_j8.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 3: ns/op log-scale grid ----
fig, axes = plt.subplots(1, len(FAMILIES), figsize=(4.0 * len(FAMILIES), 6.5),
                         sharey=True)
all_ns = agg["ns_per_op"].values
vmin, vmax = all_ns.min(), all_ns.max()
import matplotlib.colors as mcolors
norm = mcolors.LogNorm(vmin=vmin, vmax=vmax)
for ax, fam in zip(axes, FAMILIES):
    sub = agg[agg["family"] == fam]
    pivot = sub.pivot(index="wl", columns="capacity", values="ns_per_op")
    pivot = pivot.reindex(WORKLOADS)
    sns.heatmap(pivot, annot=True, fmt=".0f", cmap="rocket_r",
                norm=norm, ax=ax, cbar=ax is axes[-1],
                cbar_kws={"label": "ns/op (log)"} if ax is axes[-1] else None,
                linewidths=0.4, linecolor="white")
    ax.set_title(LABELS[fam])
    ax.set_xlabel("capacity")
    ax.set_ylabel("workload" if ax is axes[0] else "")
fig.suptitle("ns/op (log color scale) — workload × capacity × family", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "moka_extended_nsop_grid.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Print summary ----
print("# Extended sweep summary (medians, 5 trials)\n")
for wl in WORKLOADS:
    print(f"## {wl}\n")
    sub = agg[agg["wl"] == wl]
    rows = []
    for cap in CAPS:
        row = {"cap": cap}
        for fam in FAMILIES:
            f = sub[(sub["capacity"] == cap) & (sub["family"] == fam)]
            if f.empty:
                row[f"{fam}_hr"] = np.nan
                row[f"{fam}_ns"] = np.nan
            else:
                row[f"{fam}_hr"] = f["hit_ratio"].iloc[0] * 100
                row[f"{fam}_ns"] = f["ns_per_op"].iloc[0]
        rows.append(row)
    t = pd.DataFrame(rows)
    t["dhr_mm_vs_j8"] = t["mini_moka_hr"] - t["j8_hr"]
    t["dhr_moka_vs_j8"] = t["moka_hr"] - t["j8_hr"]
    print(t.round({c: 2 for c in t.columns if c != "cap"}).to_string(index=False))
    print()
