"""Visualize orig vs j8 (per_shard=32 champion) vs mini-moka (W-TinyLFU) on Twitter traces.

Source: profiles/minimoka_twitter_2026-05-06.csv (cluster × cap × variant × trial).
Variants in this CSV: orig, j8_n{32,128,512,2048} (= per_shard=32 champion), mini_moka.

Run via: `uv run --project scripts python scripts/plot_minimoka_twitter.py`
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

CSV = PROFILES / "minimoka_twitter_2026-05-06.csv"

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

# Normalize variant: all j8_n* → "j8" (we know per_shard=32 from the sweep script).
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
FAMILIES = ["orig", "j8", "mini_moka"]
COLORS = {"orig": "#888888", "j8": "#d7301f", "mini_moka": "#1f78b4"}
LABELS = {"orig": "orig (SIEVE C-port)",
          "j8": "j8 (SIEVE / per_shard=32)",
          "mini_moka": "mini-moka (W-TinyLFU)"}

# ---- Plot 1: HR bars per cluster, grouped by capacity ----
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.5 * len(CLUSTERS), 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.27
    for i, fam in enumerate(FAMILIES):
        ssf = sub[sub["family"] == fam].set_index("capacity")
        hr = [ssf.loc[c, "hit_ratio"] * 100 if c in ssf.index else np.nan for c in CAPS]
        ax.bar(x + (i - 1) * width, hr, width, color=COLORS[fam], label=LABELS[fam])
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in CAPS])
    ax.set_xlabel("capacity")
    ax.set_ylabel("hit ratio (%)" if cluster == CLUSTERS[0] else "")
    ax.set_title(cluster)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(fontsize=8, loc="upper left")
fig.suptitle("Hit ratio: orig vs j8 vs mini-moka (W-TinyLFU) on Twitter traces", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "minimoka_twitter_hr.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 2: ns/op bars (log y because mini-moka is ~10x slower) ----
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.5 * len(CLUSTERS), 5.5), sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.27
    for i, fam in enumerate(FAMILIES):
        ssf = sub[sub["family"] == fam].set_index("capacity")
        ns = [ssf.loc[c, "ns_per_op"] if c in ssf.index else np.nan for c in CAPS]
        ax.bar(x + (i - 1) * width, ns, width, color=COLORS[fam], label=LABELS[fam])
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in CAPS])
    ax.set_xlabel("capacity")
    ax.set_ylabel("ns / op (log)" if cluster == CLUSTERS[0] else "")
    ax.set_yscale("log")
    ax.set_title(cluster)
    ax.grid(True, axis="y", which="both", alpha=0.4)
    ax.legend(fontsize=8, loc="upper right")
fig.suptitle("ns/op (log scale): orig vs j8 vs mini-moka on Twitter traces", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "minimoka_twitter_nsop.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 3: Pareto scatter (HR vs ns/op) per cluster ----
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(6.0 * len(CLUSTERS), 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    for fam in FAMILIES:
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
    ax.set_xscale("log")
    ax.set_xlabel("ns / op (log; rightward = faster)")
    ax.set_ylabel("hit ratio (%)")
    ax.set_title(cluster)
    ax.grid(True, which="both", alpha=0.4)
    ax.legend(fontsize=9, loc="lower left")
fig.suptitle("Pareto: hit ratio vs ns/op — line traces capacity sweep per family", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "minimoka_twitter_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Print summary table to stdout ----
print(f"# orig vs j8 (per_shard=32) vs mini-moka — Twitter trace medians (5 trials)\n")
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
    t["dhr_j8_vs_mm"] = t["j8_hr"] - t["mini_moka_hr"]
    t["dns_j8_vs_mm"] = t["j8_ns"] - t["mini_moka_ns"]
    print(t.round({c: 2 for c in t.columns if c != "cap"}).to_string(index=False))
    print()
