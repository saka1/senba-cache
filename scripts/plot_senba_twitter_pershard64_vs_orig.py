"""Senba::Cache (per_shard=64) vs sieve_orig on Twitter cluster018.

Mirror of `plot_j8_twitter.py` Plot 6, adapted for the current `senba::Cache`
(Slot32, per-shard fixed at 64 by design).

Source: profiles/senba_twitter_pareto_2026-05-06.csv
Out:    docs/figures/senba_twitter_pershard64_vs_orig.png

Run: `uv run --project scripts python scripts/plot_senba_twitter_pershard64_vs_orig.py`
"""
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import seaborn as sns

ROOT = Path(__file__).resolve().parent.parent
CSV = ROOT / "profiles" / "senba_twitter_pareto_2026-05-06.csv"
OUT = ROOT / "docs" / "figures" / "senba_twitter_pershard64_vs_orig.png"
PER_SHARD = 64

sns.set_theme(style="whitegrid", context="talk")

df = pd.read_csv(CSV)
df["ns_per_op"] = df["elapsed_ns"] / (df["hits"] + df["misses"])
df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])

agg = (
    df.groupby(["cluster", "capacity", "variant"], as_index=False)
      .agg(ns_per_op=("ns_per_op", "median"),
           hit_ratio=("hit_ratio", "median"))
)

orig = agg[agg["variant"] == "orig"].rename(
    columns={"ns_per_op": "ns_orig", "hit_ratio": "hr_orig"}
)[["cluster", "capacity", "ns_orig", "hr_orig"]]
senba = agg[agg["variant"].str.startswith("senba_")].copy()

CLUSTERS = sorted(df["cluster"].unique())
fig, axes = plt.subplots(2, len(CLUSTERS), figsize=(5.6 * len(CLUSTERS), 8.0),
                         sharey=False, squeeze=False)

for col, cluster in enumerate(CLUSTERS):
    sub_o = orig[orig["cluster"] == cluster].sort_values("capacity")
    sub_s = senba[senba["cluster"] == cluster].sort_values("capacity")
    caps = sub_o["capacity"].tolist()
    x = np.arange(len(caps))
    width = 0.38

    ax = axes[0, col]
    hr_o = sub_o["hr_orig"].values * 100
    hr_s = sub_s.set_index("capacity").loc[caps, "hit_ratio"].values * 100
    ax.bar(x - width / 2, hr_o, width, color="#888888", label="orig")
    ax.bar(x + width / 2, hr_s, width, color="#d7301f",
           label=f"senba::Cache (per_shard={PER_SHARD})")
    for i, (a, b) in enumerate(zip(hr_o, hr_s)):
        ax.text(i, max(a, b) + 1.0, f"{b - a:+.2f}pp",
                ha="center", va="bottom", fontsize=10, color="#d7301f")
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in caps])
    ax.set_xlabel("capacity")
    ax.set_ylabel("hit ratio (%)" if col == 0 else "")
    ax.set_title(cluster)
    ax.set_ylim(0, max(hr_o.max(), hr_s.max()) * 1.18)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(loc="lower right", fontsize=9)

    ax = axes[1, col]
    ns_o = sub_o["ns_orig"].values
    ns_s = sub_s.set_index("capacity").loc[caps, "ns_per_op"].values
    ax.bar(x - width / 2, ns_o, width, color="#888888", label="orig")
    ax.bar(x + width / 2, ns_s, width, color="#1f78b4",
           label=f"senba::Cache (per_shard={PER_SHARD})")
    for i, (a, b) in enumerate(zip(ns_o, ns_s)):
        d = b - a
        ax.text(i, max(a, b) + 0.6, f"{d:+.1f} ns",
                ha="center", va="bottom", fontsize=10, color="#1f78b4")
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in caps])
    ax.set_xlabel("capacity")
    ax.set_ylabel("ns / op" if col == 0 else "")
    ax.set_ylim(0, max(ns_o.max(), ns_s.max()) * 1.18)
    ax.grid(True, axis="y", alpha=0.4)
    ax.legend(loc="lower right", fontsize=9)

fig.suptitle(
    f"orig vs senba::Cache (per_shard={PER_SHARD}) on Twitter — "
    f"hit ratio (top) and ns/op (bottom)",
    y=1.00,
)
fig.tight_layout()
OUT.parent.mkdir(parents=True, exist_ok=True)
fig.savefig(OUT, dpi=150, bbox_inches="tight")
print(f"wrote {OUT}")
