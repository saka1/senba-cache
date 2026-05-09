"""Visualize c15s (sloppy visited) Phase 1 results.

Two inputs (latest-by-date selection from `profiles/`):
- `c15s_phase1_<date>.csv` — concurrent throughput on uniform read-heavy 16T
  (bench_concurrent default columns).
- `c15s_hr_<date>.csv` — Twitter HR (5 cluster × 3 cap × 5 seed × 4 variant),
  columns: trial,variant,cluster,len,capacity,elapsed_ns,hits,misses,evictions.

Outputs (under `docs/figures/`):
- `c15s_phase1_thr.png`   — bar: aggregate Mops normalized to c14s baseline
- `c15s_phase1_hr.png`    — bar: HR delta (pp) vs c14s, per cluster × capacity
- `c15s_phase1_pareto.png`— scatter: HR loss (pp) vs throughput improvement (×)

Run: `uv run --project scripts python scripts/plot_c15s_phase1.py`
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


def latest(prefix: str) -> Path:
    cands = sorted(PROFILES.glob(f"{prefix}*.csv"))
    if not cands:
        raise SystemExit(f"no {prefix}*.csv in {PROFILES}")
    return cands[-1]


THR_CSV = latest("c15s_phase1_")
HR_CSV = latest("c15s_hr_")
print(f"thr: {THR_CSV.name}\nhr : {HR_CSV.name}")

sns.set_theme(style="whitegrid", context="talk")

# ---- Throughput (concurrent uniform read-heavy) ----------------------------

thr = pd.read_csv(THR_CSV)
thr_med = thr.groupby("variant", as_index=False)["aggregate_mops"].median()
thr_med = thr_med.set_index("variant")
baseline_mops = float(thr_med.loc["c14s", "aggregate_mops"])
thr_med["norm"] = thr_med["aggregate_mops"] / baseline_mops

VARIANTS = ["c14s", "c15s_16", "c15s_8", "c15s_4"]
LABELS = {
    "c14s": "c14s (baseline)",
    "c15s_16": "c15s 1/16",
    "c15s_8":  "c15s 1/8",
    "c15s_4":  "c15s 1/4",
}
COLORS = {
    "c14s":    "#444444",
    "c15s_16": "#1f78b4",
    "c15s_8":  "#33a02c",
    "c15s_4":  "#e31a1c",
}

fig, ax = plt.subplots(figsize=(8.5, 5.5))
xs = np.arange(len(VARIANTS))
ys = [float(thr_med.loc[v, "norm"]) for v in VARIANTS]
ax.bar(xs, ys, color=[COLORS[v] for v in VARIANTS])
for x, y, v in zip(xs, ys, VARIANTS):
    ax.text(x, y + 0.01, f"{y:.3f}×", ha="center", va="bottom", fontsize=10)
ax.axhline(1.0, color="#888888", linestyle="--", linewidth=1, label="c14s baseline")
ax.axhline(1.5, color="#cc3333", linestyle=":", linewidth=1,
           label="Phase 1 GO threshold (1.5×)")
ax.set_xticks(xs)
ax.set_xticklabels([LABELS[v] for v in VARIANTS])
ax.set_ylabel("aggregate Mops normalized to c14s")
ax.set_title("c15s sloppy visited — uniform read-heavy 16T throughput")
ax.legend(fontsize=10)
fig.tight_layout()
fig.savefig(OUT / "c15s_phase1_thr.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- HR (Twitter) ----------------------------------------------------------

hr = pd.read_csv(HR_CSV)
hr["hit_ratio"] = hr["hits"] / (hr["hits"] + hr["misses"])

# bench.rs variant naming -> short name
def to_short(v: str) -> str:
    return v.replace("_n64", "")


hr["short"] = hr["variant"].apply(to_short)
agg = (
    hr.groupby(["cluster", "capacity", "short"], as_index=False)
    .agg(hr_med=("hit_ratio", "median"),
         hr_p25=("hit_ratio", lambda s: float(np.quantile(s, 0.25))),
         hr_p75=("hit_ratio", lambda s: float(np.quantile(s, 0.75))),
         ns_med=("elapsed_ns", "median"))
)

# baseline c14s HR per (cluster, capacity)
base = agg[agg["short"] == "c14s"].rename(columns={
    "hr_med": "c14s_hr",
    "hr_p25": "_p25",
    "hr_p75": "_p75",
    "ns_med": "_ns"
})[["cluster", "capacity", "c14s_hr"]]
agg = agg.merge(base, on=["cluster", "capacity"], how="left")
agg["hr_loss_pp"] = (agg["c14s_hr"] - agg["hr_med"]) * 100.0

CLUSTERS = sorted(agg["cluster"].unique())
CAPS = sorted(agg["capacity"].unique())

# Plot: HR loss (pp) per cluster × capacity, grouped by sample rate.
fig, axes = plt.subplots(1, len(CLUSTERS), figsize=(4.5 * len(CLUSTERS), 5.0),
                         sharey=True)
if len(CLUSTERS) == 1:
    axes = [axes]
WIDTH = 0.25
SLOPPY = ["c15s_16", "c15s_8", "c15s_4"]
for ax, cluster in zip(axes, CLUSTERS):
    sub = agg[(agg["cluster"] == cluster) & (agg["short"].isin(SLOPPY))]
    x = np.arange(len(CAPS))
    for i, v in enumerate(SLOPPY):
        ssf = sub[sub["short"] == v].set_index("capacity")
        ys = [ssf.loc[c, "hr_loss_pp"] if c in ssf.index else np.nan for c in CAPS]
        ax.bar(x + (i - 1) * WIDTH, ys, WIDTH,
               color=COLORS[v], label=LABELS[v])
    ax.axhline(0.5, color="#cc3333", linestyle=":", linewidth=1,
               label="Phase 1 STOP threshold (0.5pp)")
    ax.set_xticks(x)
    ax.set_xticklabels([str(c) for c in CAPS])
    ax.set_xlabel("capacity")
    if cluster == CLUSTERS[0]:
        ax.set_ylabel("HR loss vs c14s (pp)")
    ax.set_title(cluster)
    ax.legend(fontsize=8, loc="best")
fig.suptitle("c15s sloppy visited — Twitter 5 cluster HR loss vs c14s", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "c15s_phase1_hr.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Pareto scatter: HR loss vs throughput improvement ---------------------

# x = mean HR loss across clusters/capacities, y = throughput improvement (×).
mean_hr_loss = (
    agg[agg["short"].isin(SLOPPY)]
    .groupby("short", as_index=False)["hr_loss_pp"]
    .mean()
    .set_index("short")
)
fig, ax = plt.subplots(figsize=(8.5, 6.0))
# c14s baseline at (0, 1.0)
ax.scatter([0.0], [1.0], color=COLORS["c14s"], s=140, marker="o",
           label=LABELS["c14s"], zorder=5)
ax.annotate(LABELS["c14s"], (0.0, 1.0), textcoords="offset points",
            xytext=(8, 6), fontsize=9)
for v in SLOPPY:
    x = float(mean_hr_loss.loc[v, "hr_loss_pp"])
    y = float(thr_med.loc[v, "norm"])
    ax.scatter([x], [y], color=COLORS[v], s=140, marker="o",
               label=LABELS[v], zorder=5)
    ax.annotate(LABELS[v], (x, y), textcoords="offset points",
                xytext=(8, 6), fontsize=9)
ax.axhline(1.0, color="#888888", linestyle="--", linewidth=1)
ax.axhline(1.5, color="#cc3333", linestyle=":", linewidth=1,
           label="Phase 1 GO (≥1.5× thr)")
ax.axvline(0.5, color="#cc3333", linestyle=":", linewidth=1,
           label="Phase 1 STOP (≥0.5pp HR loss)")
ax.set_xlabel("mean HR loss vs c14s across clusters/caps (pp)")
ax.set_ylabel("aggregate Mops × c14s (uniform read-heavy 16T)")
ax.set_title("c15s sloppy visited — HR loss vs throughput Pareto (Phase 1)")
ax.grid(True, which="both", alpha=0.4)
ax.legend(fontsize=9, loc="best")
fig.tight_layout()
fig.savefig(OUT / "c15s_phase1_pareto.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---- Summary tables to stdout ----------------------------------------------

print("\n## throughput (uniform read-heavy 16T, median over trials)\n")
print(thr_med[["aggregate_mops", "norm"]].round(4).to_string())

print("\n## HR loss (pp) per cluster × capacity, median over 5 seeds\n")
piv = (
    agg[agg["short"].isin(SLOPPY)]
    .pivot_table(index=["cluster", "capacity"], columns="short",
                 values="hr_loss_pp", aggfunc="first")
    .round(3)
)
print(piv.to_string())

print("\n## HR loss summary (mean / max across all (cluster,cap))\n")
mean_max = (
    agg[agg["short"].isin(SLOPPY)]
    .groupby("short")["hr_loss_pp"]
    .agg(["mean", "max", "min"])
    .round(3)
)
print(mean_max.to_string())
