"""Visualize orig vs senba::Cache (per_shard=32, per_shard=64) on Twitter traces with raw String keys.

Source: profiles/senba_twitter_string_<date>.csv (cluster × cap × variant × trial).
Clusters: cluster006, cluster016, cluster018, cluster019, cluster034.

Run: `uv run --project scripts python scripts/plot_senba_twitter_string.py`
"""

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import seaborn as sns

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "figures"
OUT.mkdir(parents=True, exist_ok=True)
PROFILES = ROOT / "profiles"

if len(sys.argv) > 1:
    CSV = Path(sys.argv[1])
else:
    csvs = sorted(PROFILES.glob("senba_twitter_string_*.csv"))
    if not csvs:
        raise SystemExit("no senba_twitter_string_*.csv in profiles/")
    CSV = csvs[-1]

print(f"# loading {CSV}", file=sys.stderr)

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

# family: orig / senba32 / senba64 (by per_shard, to keep 4-cap line distinct)
def fam(row):
    if row["variant"] == "orig":
        return "orig"
    return f"senba_ps{row['per_shard']}"

df["family"] = df.apply(fam, axis=1)

agg = (
    df.groupby(["cluster", "capacity", "family"], as_index=False)
    .agg(ns_per_op=("ns_per_op", "median"),
         hit_ratio=("hit_ratio", "median"))
)

CLUSTERS = sorted(df["cluster"].unique())
CAPS = sorted(df["capacity"].unique())
FAMILIES = ["orig", "senba_ps32", "senba_ps64"]
COLORS = {"orig": "#888888", "senba_ps32": "#d7301f", "senba_ps64": "#fdae6b"}
LABELS = {
    "orig": "orig (SIEVE C-port)",
    "senba_ps32": "senba::Cache (per_shard=32)",
    "senba_ps64": "senba::Cache (per_shard=64)",
}

NCOLS = len(CLUSTERS)

# ---- Plot 1: HR bars per cluster, grouped by capacity ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.0 * NCOLS, 5.5), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.26
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
    ax.legend(fontsize=7, loc="best")
fig.suptitle("HR: orig vs senba::Cache — Twitter (raw String key, 5 cluster)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "senba_twitter_string_hr.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 2: ns/op bars (log y) ----
fig, axes = plt.subplots(1, NCOLS, figsize=(5.0 * NCOLS, 5.5), sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[agg["cluster"] == cluster]
    x = np.arange(len(CAPS))
    width = 0.26
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
    ax.legend(fontsize=7, loc="best")
fig.suptitle("ns/op (log): orig vs senba::Cache — Twitter (raw String key)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "senba_twitter_string_nsop.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Plot 3: Pareto (HR vs ns/op) per cluster ----
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
fig.suptitle("Pareto: orig vs senba::Cache — Twitter (raw String key)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "senba_twitter_string_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Print summary table ----
print(f"# orig vs senba::Cache — Twitter raw String key, medians ({df['trial'].nunique()} trials)\n")
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
    t["dhr_s32_orig"] = t["senba_ps32_hr"] - t["orig_hr"]
    t["dns_s32_orig"] = t["senba_ps32_ns"] - t["orig_ns"]
    print(t.round({c: 2 for c in t.columns if c != "cap"}).to_string(index=False))
    print()
