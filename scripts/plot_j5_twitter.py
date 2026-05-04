"""Visualize sieve_j5 Twitter trace bench results.

Source: profiles/j5_twitter_pareto_<date>.csv (cluster × cap × per_shard × trial).

Run via: `uv run python scripts/plot_j5_twitter.py`
Outputs PNGs to docs/figures/.
"""

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "figures"
OUT.mkdir(parents=True, exist_ok=True)
PROFILES = ROOT / "profiles"

CSV = PROFILES / "j5_twitter_pareto_2026-05-05.csv"

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

# Median over trials per (cluster, capacity, per_shard, variant).
agg = (
    df.groupby(["cluster", "capacity", "per_shard", "variant"], as_index=False)
    .agg(ns_per_op=("ns_per_op", "median"),
         hit_ratio=("hit_ratio", "median"),
         shards=("shards", "first"))
)

# Split orig (one row per (cluster, capacity)) from j5 sweep.
orig = agg[agg["variant"] == "orig"].rename(
    columns={"ns_per_op": "ns_orig", "hit_ratio": "hr_orig"}
)[["cluster", "capacity", "ns_orig", "hr_orig"]]
j5 = agg[agg["variant"].str.startswith("j5_")].copy()
j5 = j5.merge(orig, on=["cluster", "capacity"], how="left")
j5["dns_vs_orig"] = j5["ns_per_op"] - j5["ns_orig"]
j5["dhr_pp"] = (j5["hit_ratio"] - j5["hr_orig"]) * 100

CLUSTERS = sorted(df["cluster"].unique())
CAPS = sorted(df["capacity"].unique())
PER_SHARDS = sorted(df["per_shard"].unique())
PER_SHARDS_J5 = sorted(j5["per_shard"].unique())

# ---------------------------------------------------------------------------
# Plot 1: ns/op vs per_shard, faceted by (cluster, capacity).
# Dashed orig reference per facet.
# ---------------------------------------------------------------------------
g = sns.relplot(
    data=j5,
    kind="line",
    x="per_shard",
    y="ns_per_op",
    hue="cluster",
    style="cluster",
    markers=True,
    dashes=False,
    col="capacity",
    row="cluster",
    palette="tab10",
    height=3.6,
    aspect=1.15,
    facet_kws={"sharey": False, "sharex": True},
)
for (row_val, col_val), ax in g.axes_dict.items():
    ref = orig[(orig["cluster"] == row_val) & (orig["capacity"] == col_val)]
    if not ref.empty:
        ax.axhline(ref["ns_orig"].iloc[0], color="black",
                   linestyle="--", linewidth=1.2, alpha=0.7,
                   label="orig")
    ax.set_xscale("log", base=2)
    ax.set_xticks(PER_SHARDS_J5)
    ax.set_xticklabels([str(s) for s in PER_SHARDS_J5])
g.set_axis_labels("per_shard (log₂)", "ns / op (median of 5)")
g.set_titles("{row_name} | cap={col_name}")
g.figure.suptitle(
    "j5 ns/op on Twitter traces — solid line = j5 across per_shard, dashed = orig",
    y=1.02,
)
g.figure.savefig(OUT / "j5_twitter_nsop_grid.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# ---------------------------------------------------------------------------
# Plot 2: Δns vs orig as heatmap-like, x=per_shard, y=capacity, panel=cluster.
# Negative cells (blue) = j5 wins; positive (red) = j5 loses.
# ---------------------------------------------------------------------------
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.2 * len(CLUSTERS), 4.6),
                         sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = j5[j5["cluster"] == cluster]
    pivot = sub.pivot_table(
        index="capacity", columns="per_shard", values="dns_vs_orig", aggfunc="median"
    ).sort_index(ascending=False)
    sns.heatmap(
        pivot,
        annot=True,
        fmt=".1f",
        center=0,
        cmap="RdBu_r",
        cbar=ax is axes[-1],
        cbar_kws={"label": "Δns/op (j5 − orig)"} if ax is axes[-1] else None,
        ax=ax,
        linewidths=0.4,
        linecolor="white",
    )
    ax.set_title(f"{cluster}")
    ax.set_xlabel("per_shard")
    ax.set_ylabel("capacity" if ax is axes[0] else "")
fig.suptitle(
    "Δns/op = j5 − orig on Twitter traces (negative = j5 faster)",
    y=1.02,
)
fig.tight_layout()
fig.savefig(OUT / "j5_twitter_delta_nsop_heatmap.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 3: Δhit_ratio (pp) heatmap — does shard subdivision damage hit ratio?
# ---------------------------------------------------------------------------
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(5.2 * len(CLUSTERS), 4.6),
                         sharey=True)
for ax, cluster in zip(axes, CLUSTERS):
    sub = j5[j5["cluster"] == cluster]
    pivot = sub.pivot_table(
        index="capacity", columns="per_shard", values="dhr_pp", aggfunc="median"
    ).sort_index(ascending=False)
    sns.heatmap(
        pivot,
        annot=True,
        fmt=".2f",
        center=0,
        cmap="RdBu_r",
        cbar=ax is axes[-1],
        cbar_kws={"label": "Δhit ratio (pp)"} if ax is axes[-1] else None,
        ax=ax,
        linewidths=0.4,
        linecolor="white",
    )
    ax.set_title(f"{cluster}")
    ax.set_xlabel("per_shard")
    ax.set_ylabel("capacity" if ax is axes[0] else "")
fig.suptitle(
    "Δhit ratio = j5 − orig on Twitter traces (positive = j5 wins; pp)",
    y=1.02,
)
fig.tight_layout()
fig.savefig(OUT / "j5_twitter_delta_hr_heatmap.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 4: Pareto scatter (ns/op × hit ratio). Per cluster, all (cap, per_shard)
# points; orig as ★. Lower-left is dominated; upper-right (faster + higher hr)
# is the win region.
# ---------------------------------------------------------------------------
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(6.5 * len(CLUSTERS), 5.6),
                         sharey=False)
for ax, cluster in zip(axes, CLUSTERS):
    sub_j5 = j5[j5["cluster"] == cluster]
    sub_orig = orig[orig["cluster"] == cluster]
    palette = sns.color_palette("viridis", n_colors=len(CAPS))
    for i, cap in enumerate(CAPS):
        ssj = sub_j5[sub_j5["capacity"] == cap].sort_values("per_shard")
        if not ssj.empty:
            ax.plot(
                ssj["ns_per_op"], ssj["hit_ratio"] * 100,
                "-o", color=palette[i], markersize=8, linewidth=2,
                label=f"j5 cap={cap}",
            )
            for _, r in ssj.iterrows():
                ax.annotate(
                    f"ps={int(r['per_shard'])}",
                    (r["ns_per_op"], r["hit_ratio"] * 100),
                    textcoords="offset points",
                    xytext=(5, 5), fontsize=7, color=palette[i],
                )
        sso = sub_orig[sub_orig["capacity"] == cap]
        if not sso.empty:
            ax.scatter(
                sso["ns_orig"], sso["hr_orig"] * 100,
                marker="*", s=300, edgecolor="black", linewidth=1.0,
                color=palette[i], zorder=5,
                label=f"orig cap={cap}",
            )
    ax.invert_xaxis()
    ax.set_xlabel("ns / op (rightward = faster)")
    ax.set_ylabel("hit ratio (%)")
    ax.set_title(cluster)
    ax.legend(fontsize=7, loc="best", ncol=2)
fig.suptitle(
    "Pareto on Twitter traces — ★ = orig, line = j5 sweep over per_shard",
    y=1.02,
)
fig.tight_layout()
fig.savefig(OUT / "j5_twitter_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 5: per-trial spread sanity (boxplot of ns/op by per_shard, faceted).
# Confirms median-of-5 is not papering over noise.
# ---------------------------------------------------------------------------
df_j5 = df[df["variant"].str.startswith("j5_")].copy()
g = sns.catplot(
    data=df_j5,
    kind="box",
    x="per_shard",
    y="ns_per_op",
    col="capacity",
    row="cluster",
    color="#6baed6",
    height=3.0,
    aspect=1.2,
    sharey=False,
)
g.set_titles("{row_name} | cap={col_name}")
g.set_axis_labels("per_shard", "ns / op")
g.figure.suptitle("j5 trial spread (5 trials per cell)", y=1.02)
g.figure.savefig(OUT / "j5_twitter_trial_spread.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

print(f"wrote figures to {OUT}")
for p in sorted(OUT.glob("j5_twitter_*.png")):
    print(" -", p.name)
