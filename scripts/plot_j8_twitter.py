"""Visualize sieve_j8 vs orig Twitter trace bench results.

Source: profiles/j8_twitter_full_2026-05-06.csv (cluster × cap × per_shard × trial).
Mirrors plot_j7_twitter.py to keep figure conventions identical.

j8 has MAX_PER_SHARD=64, so per_shard ∈ {16, 32, 64} (vs j7's {32, 64, 128, 256}).
Champion = per_shard=32 (matches the j7 chart's choice for direct visual comparison).

Run via: `uv run python scripts/plot_j8_twitter.py`
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

CSV = PROFILES / "j8_twitter_full_2026-05-06.csv"
CHAMP_PER_SHARD = 32

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

agg = (
    df.groupby(["cluster", "capacity", "per_shard", "variant"], as_index=False)
    .agg(ns_per_op=("ns_per_op", "median"),
         hit_ratio=("hit_ratio", "median"),
         shards=("shards", "first"))
)

orig = agg[agg["variant"] == "orig"].rename(
    columns={"ns_per_op": "ns_orig", "hit_ratio": "hr_orig"}
)[["cluster", "capacity", "ns_orig", "hr_orig"]]
j8 = agg[agg["variant"].str.startswith("j8_")].copy()
j8 = j8.merge(orig, on=["cluster", "capacity"], how="left")
j8["dns_vs_orig"] = j8["ns_per_op"] - j8["ns_orig"]
j8["dhr_pp"] = (j8["hit_ratio"] - j8["hr_orig"]) * 100

CLUSTERS = sorted(df["cluster"].unique())
CAPS = sorted(df["capacity"].unique())
PER_SHARDS_J8 = sorted(j8["per_shard"].unique())

# Plot 1: ns/op vs per_shard, faceted by (cluster, capacity).
g = sns.relplot(
    data=j8, kind="line", x="per_shard", y="ns_per_op",
    hue="cluster", style="cluster", markers=True, dashes=False,
    col="capacity", row="cluster", palette="tab10",
    height=3.6, aspect=1.15,
    facet_kws={"sharey": False, "sharex": True},
)
for (row_val, col_val), ax in g.axes_dict.items():
    ref = orig[(orig["cluster"] == row_val) & (orig["capacity"] == col_val)]
    if not ref.empty:
        ax.axhline(ref["ns_orig"].iloc[0], color="black",
                   linestyle="--", linewidth=1.2, alpha=0.7, label="orig")
    ax.set_xscale("log", base=2)
    ax.set_xticks(PER_SHARDS_J8)
    ax.set_xticklabels([str(s) for s in PER_SHARDS_J8])
g.set_axis_labels("per_shard (log₂)", "ns / op (median of 5)")
g.set_titles("{row_name} | cap={col_name}")
g.figure.suptitle(
    "j8 ns/op on Twitter traces — solid = j8 across per_shard, dashed = orig",
    y=1.02,
)
g.figure.savefig(OUT / "j8_twitter_nsop_grid.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# Plot 2: Δns heatmap.
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.2 * len(CLUSTERS), 4.6),
                         sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = j8[j8["cluster"] == cluster]
    pivot = sub.pivot_table(index="capacity", columns="per_shard",
                            values="dns_vs_orig", aggfunc="median").sort_index(ascending=False)
    sns.heatmap(pivot, annot=True, fmt=".1f", center=0, cmap="RdBu_r",
                cbar=ax is axes[-1],
                cbar_kws={"label": "Δns/op (j8 − orig)"} if ax is axes[-1] else None,
                ax=ax, linewidths=0.4, linecolor="white")
    ax.set_title(f"{cluster}")
    ax.set_xlabel("per_shard")
    ax.set_ylabel("capacity" if ax is axes[0] else "")
fig.suptitle("Δns/op = j8 − orig on Twitter traces (negative = j8 faster)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "j8_twitter_delta_nsop_heatmap.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# Plot 3: Δhr heatmap.
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.2 * len(CLUSTERS), 4.6),
                         sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = j8[j8["cluster"] == cluster]
    pivot = sub.pivot_table(index="capacity", columns="per_shard",
                            values="dhr_pp", aggfunc="median").sort_index(ascending=False)
    sns.heatmap(pivot, annot=True, fmt=".2f", center=0, cmap="RdBu_r",
                cbar=ax is axes[-1],
                cbar_kws={"label": "Δhit ratio (pp)"} if ax is axes[-1] else None,
                ax=ax, linewidths=0.4, linecolor="white")
    ax.set_title(f"{cluster}")
    ax.set_xlabel("per_shard")
    ax.set_ylabel("capacity" if ax is axes[0] else "")
fig.suptitle("Δhit ratio = j8 − orig on Twitter traces (positive = j8 wins; pp)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "j8_twitter_delta_hr_heatmap.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# Plot 4: Pareto scatter.
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(6.5 * len(CLUSTERS), 5.6), sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub_j8 = j8[j8["cluster"] == cluster]
    sub_orig = orig[orig["cluster"] == cluster]
    palette = sns.color_palette("viridis", n_colors=len(CAPS))
    for i, cap in enumerate(CAPS):
        ssj = sub_j8[sub_j8["capacity"] == cap].sort_values("per_shard")
        if not ssj.empty:
            ax.plot(ssj["ns_per_op"], ssj["hit_ratio"] * 100,
                    "-o", color=palette[i], markersize=8, linewidth=2,
                    label=f"j8 cap={cap}")
            for _, r in ssj.iterrows():
                ax.annotate(f"ps={int(r['per_shard'])}",
                            (r["ns_per_op"], r["hit_ratio"] * 100),
                            textcoords="offset points",
                            xytext=(5, 5), fontsize=7, color=palette[i])
        sso = sub_orig[sub_orig["capacity"] == cap]
        if not sso.empty:
            ax.scatter(sso["ns_orig"], sso["hr_orig"] * 100,
                       marker="*", s=300, edgecolor="black", linewidth=1.0,
                       color=palette[i], zorder=5, label=f"orig cap={cap}")
    ax.invert_xaxis()
    ax.set_xlabel("ns / op (rightward = faster)")
    ax.set_ylabel("hit ratio (%)")
    ax.set_title(cluster)
    ax.legend(fontsize=7, loc="best", ncol=2)
fig.suptitle("Pareto on Twitter traces — ★ = orig, line = j8 sweep over per_shard", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "j8_twitter_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# Plot 5: trial spread.
df_j8 = df[df["variant"].str.startswith("j8_")].copy()
g = sns.catplot(
    data=df_j8, kind="box", x="per_shard", y="ns_per_op",
    col="capacity", row="cluster", color="#6baed6",
    height=3.0, aspect=1.2, sharey=False,
)
g.set_titles("{row_name} | cap={col_name}")
g.set_axis_labels("per_shard", "ns / op")
g.figure.suptitle("j8 trial spread (5 trials per cell)", y=1.02)
g.figure.savefig(OUT / "j8_twitter_trial_spread.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# Plot 6: per_shard=CHAMP champion vs orig (mirrors j7_twitter_pershard32_vs_orig.png).
champ = j8[j8["per_shard"] == CHAMP_PER_SHARD].copy()
fig, axes = plt.subplots(2, len(CLUSTERS), figsize=(5.2 * len(CLUSTERS), 8.0), sharey=False)
for col, cluster in enumerate(CLUSTERS):
    sub_o = orig[orig["cluster"] == cluster].sort_values("capacity")
    sub_c = champ[champ["cluster"] == cluster].sort_values("capacity")
    caps = sub_o["capacity"].tolist()
    x = np.arange(len(caps))
    width = 0.38

    ax = axes[0, col]
    hr_o = sub_o["hr_orig"].values * 100
    hr_c = sub_c.set_index("capacity").loc[caps, "hit_ratio"].values * 100
    ax.bar(x - width / 2, hr_o, width, color="#888888", label="orig")
    ax.bar(x + width / 2, hr_c, width, color="#d7301f", label=f"j8 (per_shard={CHAMP_PER_SHARD})")
    for i, (a, b) in enumerate(zip(hr_o, hr_c)):
        ax.text(i, max(a, b) + 1.0, f"{b - a:+.2f}pp",
                ha="center", va="bottom", fontsize=9, color="#d7301f")
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in caps])
    ax.set_xlabel("capacity")
    ax.set_ylabel("hit ratio (%)" if col == 0 else "")
    ax.set_title(cluster)
    ax.set_ylim(0, max(hr_o.max(), hr_c.max()) * 1.18)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(loc="lower right", fontsize=9)

    ax = axes[1, col]
    ns_o = sub_o["ns_orig"].values
    ns_c = sub_c.set_index("capacity").loc[caps, "ns_per_op"].values
    ax.bar(x - width / 2, ns_o, width, color="#888888", label="orig")
    ax.bar(x + width / 2, ns_c, width, color="#1f78b4", label=f"j8 (per_shard={CHAMP_PER_SHARD})")
    for i, (a, b) in enumerate(zip(ns_o, ns_c)):
        d = b - a
        ax.text(i, max(a, b) + 0.6, f"{d:+.1f} ns",
                ha="center", va="bottom", fontsize=9, color="#1f78b4")
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in caps])
    ax.set_xlabel("capacity")
    ax.set_ylabel("ns / op" if col == 0 else "")
    ax.set_ylim(0, max(ns_o.max(), ns_c.max()) * 1.18)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(loc="lower right", fontsize=9)

fig.suptitle(f"orig vs j8 (per_shard={CHAMP_PER_SHARD}) by cluster — hit ratio (top) and ns/op (bottom)", y=1.00)
fig.tight_layout()
fig.savefig(OUT / f"j8_twitter_pershard{CHAMP_PER_SHARD}_vs_orig.png", dpi=150, bbox_inches="tight")
plt.close(fig)
