"""Visualize orig vs j8 (per_shard=32) vs moka 0.12 vs mini-moka 0.10 on Twitter traces.

Source: profiles/st_twitter_5cluster_2026-05-06.csv (cluster × cap × variant × trial).
Clusters: cluster006, cluster016, cluster018, cluster019, cluster034.

Run: `uv run --project scripts python scripts/plot_st_twitter_5cluster.py`
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

CSV = PROFILES / "st_twitter_5cluster_2026-05-06.csv"

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

df["family"] = df["variant"].apply(
    lambda v: "j8" if v.startswith("j8_") else v
)

agg = (
    df.groupby(["cluster", "capacity", "family"], as_index=False)
    .agg(ns_per_op=("ns_per_op", "median"),
         hit_ratio=("hit_ratio", "median"))
)

CLUSTERS = sorted(df["cluster"].unique())
CAPS = sorted(df["capacity"].unique())
FAMILIES = ["orig", "j8", "moka", "mini_moka"]
COLORS = {"orig": "#888888", "j8": "#d7301f",
          "moka": "#33a02c", "mini_moka": "#1f78b4"}
LABELS = {"orig": "orig (SIEVE C-port)",
          "j8": "j8 (SIEVE / per_shard=32)",
          "moka": "moka 0.12 (W-TinyLFU + adaptive)",
          "mini_moka": "mini-moka 0.10 (W-TinyLFU)"}

NCOLS = len(CLUSTERS)

# ---- Plot 1: HR bars per cluster, grouped by capacity ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.0 * NCOLS, 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.20
    for i, fam in enumerate(FAMILIES):
        ssf = sub[sub["family"] == fam].set_index("capacity")
        hr = [ssf.loc[c, "hit_ratio"] * 100 if c in ssf.index else np.nan for c in CAPS]
        ax.bar(x + (i - 1.5) * width, hr, width, color=COLORS[fam], label=LABELS[fam])
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in CAPS])
    ax.set_xlabel("capacity")
    ax.set_ylabel("hit ratio (%)" if cluster == CLUSTERS[0] else "")
    ax.set_title(cluster)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(fontsize=7, loc="best")
fig.suptitle("Hit ratio: orig vs j8 vs moka 0.12 vs mini-moka — Twitter (5 cluster)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "st_twitter_5cluster_hr.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 2: ns/op bars (log y) ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.0 * NCOLS, 5.5), sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.20
    for i, fam in enumerate(FAMILIES):
        ssf = sub[sub["family"] == fam].set_index("capacity")
        ns = [ssf.loc[c, "ns_per_op"] if c in ssf.index else np.nan for c in CAPS]
        ax.bar(x + (i - 1.5) * width, ns, width, color=COLORS[fam], label=LABELS[fam])
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in CAPS])
    ax.set_xlabel("capacity")
    ax.set_ylabel("ns / op (log)" if cluster == CLUSTERS[0] else "")
    ax.set_yscale("log")
    ax.set_title(cluster)
    ax.grid(True, axis="y", which="both", alpha=0.4)
    ax.legend(fontsize=7, loc="best")
fig.suptitle("ns/op (log): orig vs j8 vs moka 0.12 vs mini-moka — Twitter (5 cluster)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "st_twitter_5cluster_nsop.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 3: Pareto scatter (HR vs ns/op) per cluster ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.5 * NCOLS, 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    for fam in FAMILIES:
        ssf = sub[sub["family"] == fam].sort_values("capacity")
        if ssf.empty:
            continue
        ax.plot(ssf["ns_per_op"], ssf["hit_ratio"] * 100,
                "-o", color=COLORS[fam], markersize=9, linewidth=2,
                label=LABELS[fam])
        for _, r in ssf.iterrows():
            ax.annotate(f"{int(r['capacity'])}",
                        (r["ns_per_op"], r["hit_ratio"] * 100),
                        textcoords="offset points",
                        xytext=(5, 5), fontsize=6, color=COLORS[fam])
    ax.invert_xaxis()
    ax.set_xscale("log")
    ax.set_xlabel("ns / op (log; right = faster)")
    ax.set_ylabel("hit ratio (%)")
    ax.set_title(cluster)
    ax.grid(True, which="both", alpha=0.4)
    ax.legend(fontsize=7, loc="best")
fig.suptitle("Pareto: hit ratio vs ns/op — line per family traces capacity sweep", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "st_twitter_5cluster_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 4: Pareto SIEVE-only (orig vs j8) for tighter readability ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.5 * NCOLS, 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    for fam in ["orig", "j8"]:
        ssf = sub[sub["family"] == fam].sort_values("capacity")
        if ssf.empty:
            continue
        ax.plot(ssf["ns_per_op"], ssf["hit_ratio"] * 100,
                "-o", color=COLORS[fam], markersize=10, linewidth=2,
                label=LABELS[fam])
        for _, r in ssf.iterrows():
            ax.annotate(f"cap={int(r['capacity'])}",
                        (r["ns_per_op"], r["hit_ratio"] * 100),
                        textcoords="offset points",
                        xytext=(6, 6), fontsize=7, color=COLORS[fam])
    ax.invert_xaxis()
    ax.set_xlabel("ns / op (right = faster)")
    ax.set_ylabel("hit ratio (%)")
    ax.set_title(cluster)
    ax.grid(True, which="both", alpha=0.4)
    ax.legend(fontsize=8, loc="best")
fig.suptitle("Pareto (SIEVE only): orig vs j8 — Twitter (5 cluster)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "st_twitter_5cluster_pareto_sieve.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Print summary table to stdout ----
print(f"# orig vs j8 vs moka vs mini_moka — Twitter trace medians (5 trials)\n")
for cluster in CLUSTERS:
    print(f"## {cluster}\n")
    sub = agg[agg["cluster"] == cluster]
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
    t["dhr_j8_orig"] = t["j8_hr"] - t["orig_hr"]
    t["dns_j8_orig"] = t["j8_ns"] - t["orig_ns"]
    print(t.round({c: 2 for c in t.columns if c != "cap"}).to_string(index=False))
    print()
