"""Visualize sieve_j4 bench results.

Sources:
- profiles/j4_capsweep_2026-05-05.csv     — cap sweep (N=8, varying cap)
- profiles/j4_shardsweep_2026-05-05.csv   — SHARDS sweep (cap=1024, varying N)

Run via: `uv run --project scripts python scripts/plot_j4.py`
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

sns.set_theme(style="whitegrid", context="talk")


def _read_bench_csv(path: Path) -> pd.DataFrame:
    """The bench CLI emits a header per `--variant` invocation, so duplicates exist."""
    raw = path.read_text().splitlines()
    rows = [r for r in raw if not r.startswith("variant,") and r.strip()]
    df = pd.read_csv(
        Path("/dev/stdin") if False else path,
        comment=None,
        skip_blank_lines=True,
    )
    # Drop rows that are header-repeats
    df = df[df["variant"] != "variant"].copy()
    df["skew"] = df["skew"].astype(float)
    df["capacity"] = df["capacity"].astype(int)
    df["elapsed_ns"] = df["elapsed_ns"].astype(int)
    df["hits"] = df["hits"].astype(int)
    df["misses"] = df["misses"].astype(int)
    df["elapsed_ms"] = df["elapsed_ns"] / 1e6
    df["hit_ratio"] = df["hits"] / (df["hits"] + df["misses"])
    return df


cap_df = _read_bench_csv(PROFILES / "j4_capsweep_2026-05-05.csv")
shard_df = _read_bench_csv(PROFILES / "j4_shardsweep_2026-05-05.csv")

# ---------------------------------------------------------------------------
# Plot 1: cap sweep throughput — orig vs j3 vs j4 across cap, faceted by skew.
# log-log axis to make the j3 explosion visible without dwarfing j4/orig.
# ---------------------------------------------------------------------------
g = sns.relplot(
    data=cap_df,
    kind="line",
    x="capacity",
    y="elapsed_ms",
    hue="variant",
    style="variant",
    markers=True,
    dashes=False,
    col="skew",
    palette={"orig": "#1f77b4", "j3": "#d62728", "j4": "#2ca02c"},
    hue_order=["orig", "j3", "j4"],
    height=4.2,
    aspect=1.0,
    facet_kws={"sharey": True},
)
for ax in g.axes.flat:
    ax.set_xscale("log")
    ax.set_yscale("log")
g.set_axis_labels("capacity (log)", "ms / 1M ops (log)")
g.set_titles("skew={col_name}")
g.figure.suptitle(
    "j4 cap sweep — throughput across cap (N=8 fixed)", y=1.03
)
g.figure.savefig(OUT / "j4_capsweep_throughput.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# ---------------------------------------------------------------------------
# Plot 2: cap sweep — j4/orig and j4/j3 ratio, shows crossover.
# ---------------------------------------------------------------------------
pivot = cap_df.pivot_table(
    index=["skew", "capacity"], columns="variant", values="elapsed_ms"
).reset_index()
pivot["j4_over_orig"] = pivot["j4"] / pivot["orig"]
pivot["j4_over_j3"] = pivot["j4"] / pivot["j3"]

fig, axes = plt.subplots(1, 2, figsize=(14, 5.5), sharey=False)
for ax, col, title in [
    (axes[0], "j4_over_orig", "j4 / orig — <1.0 means j4 wins"),
    (axes[1], "j4_over_j3", "j4 / j3 — <1.0 means j4 wins"),
]:
    sns.lineplot(
        data=pivot,
        x="capacity",
        y=col,
        hue="skew",
        marker="o",
        palette="viridis",
        ax=ax,
    )
    ax.set_xscale("log")
    ax.axhline(1.0, color="black", linewidth=1, linestyle="--", alpha=0.6)
    ax.set_title(title)
    ax.set_xlabel("capacity (log)")
    ax.set_ylabel("speed ratio")
fig.suptitle("j4 throughput crossover map (N=8, varying cap)", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "j4_capsweep_ratio.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 3: cap sweep — hit ratio Δ between j4 and orig.
# Bar plot per skew, x = cap. Negative bars = set-associative tax.
# ---------------------------------------------------------------------------
hr_pivot = cap_df.pivot_table(
    index=["skew", "capacity"], columns="variant", values="hit_ratio"
).reset_index()
hr_pivot["delta_pp"] = (hr_pivot["j4"] - hr_pivot["orig"]) * 100

fig, ax = plt.subplots(figsize=(11, 5))
sns.barplot(
    data=hr_pivot,
    x="capacity",
    y="delta_pp",
    hue="skew",
    palette="viridis",
    ax=ax,
)
ax.axhline(0, color="black", linewidth=1, linestyle="-", alpha=0.7)
ax.set_title(
    "Set-associative tax — (j4 − orig) hit ratio in pp\n"
    "below 0 = j4 loses (tax), above 0 = j4 wins"
)
ax.set_xlabel("capacity")
ax.set_ylabel("Δ hit ratio (percentage points)")
fig.savefig(OUT / "j4_capsweep_hitratio_delta.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 4: shard sweep — throughput vs SHARDS at cap=1024.
# ---------------------------------------------------------------------------
def _shard_n(v: str) -> int | None:
    if v.startswith("j4_n"):
        return int(v[4:])
    return None


shard_df["shards"] = shard_df["variant"].map(_shard_n)
ss = shard_df.dropna(subset=["shards"]).copy()
ss["shards"] = ss["shards"].astype(int)
ss["per_shard_cap"] = (ss["capacity"] / ss["shards"]).astype(int)

# orig is the reference line per skew
orig_ref = shard_df[shard_df["variant"] == "orig"].set_index("skew")["elapsed_ms"]

fig, axes = plt.subplots(1, 2, figsize=(14, 5.5), sharey=False)
sns.lineplot(
    data=ss,
    x="shards",
    y="elapsed_ms",
    hue="skew",
    marker="o",
    palette="viridis",
    ax=axes[0],
)
for skew, ref in orig_ref.items():
    color = sns.color_palette("viridis", n_colors=3)[
        sorted(orig_ref.index).index(skew)
    ]
    axes[0].axhline(
        ref, color=color, linestyle="--", linewidth=1.2, alpha=0.8
    )
axes[0].set_xscale("log", base=2)
axes[0].set_xticks([1, 2, 4, 8, 16, 32])
axes[0].set_xticklabels(["1", "2", "4", "8", "16", "32"])
axes[0].set_title("j4 throughput vs SHARDS (cap=1024 fixed)\n"
                  "dashed = orig reference per skew")
axes[0].set_xlabel("SHARDS (log scale)")
axes[0].set_ylabel("ms / 1M ops")

# Hit ratio sweet spot
sns.lineplot(
    data=ss,
    x="shards",
    y="hit_ratio",
    hue="skew",
    marker="o",
    palette="viridis",
    ax=axes[1],
)
for skew, _ in orig_ref.items():
    ref_hr = shard_df[(shard_df["variant"] == "orig") & (shard_df["skew"] == skew)][
        "hit_ratio"
    ].iloc[0]
    color = sns.color_palette("viridis", n_colors=3)[
        sorted(orig_ref.index).index(skew)
    ]
    axes[1].axhline(ref_hr, color=color, linestyle="--", linewidth=1.2, alpha=0.8)
axes[1].set_xscale("log", base=2)
axes[1].set_xticks([1, 2, 4, 8, 16, 32])
axes[1].set_xticklabels(["1", "2", "4", "8", "16", "32"])
axes[1].set_title("Hit ratio vs SHARDS (cap=1024 fixed)\n"
                  "dashed = orig reference per skew")
axes[1].set_xlabel("SHARDS (log scale)")
axes[1].set_ylabel("hit ratio")
fig.suptitle("j4 SHARDS sweep at cap=1024 — throughput vs hit-ratio trade-off", y=1.03)
fig.tight_layout()
fig.savefig(OUT / "j4_shardsweep_thrput_hitratio.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 5: trade-off scatter — throughput vs hit ratio, marker = N.
# Highlights the (skew=0.6, N=32) point that beats orig on both axes.
# ---------------------------------------------------------------------------
fig, ax = plt.subplots(figsize=(9, 6.5))
palette = sns.color_palette("viridis", n_colors=3)
skews_sorted = sorted(ss["skew"].unique())

for i, skew in enumerate(skews_sorted):
    sub = ss[ss["skew"] == skew].sort_values("shards")
    ax.plot(
        sub["elapsed_ms"],
        sub["hit_ratio"] * 100,
        "-o",
        color=palette[i],
        label=f"j4 skew={skew}",
        linewidth=2,
        markersize=8,
    )
    for _, row in sub.iterrows():
        ax.annotate(
            f"N={int(row['shards'])}",
            (row["elapsed_ms"], row["hit_ratio"] * 100),
            textcoords="offset points",
            xytext=(6, 6),
            fontsize=8,
            color=palette[i],
        )
    ref_row = shard_df[(shard_df["variant"] == "orig") & (shard_df["skew"] == skew)].iloc[0]
    ax.scatter(
        ref_row["elapsed_ms"],
        ref_row["hit_ratio"] * 100,
        marker="*",
        s=300,
        edgecolor="black",
        linewidth=1.2,
        color=palette[i],
        zorder=5,
        label=f"orig skew={skew}",
    )

ax.set_xlabel("ms / 1M ops (lower is better →)")
ax.set_ylabel("hit ratio (%) (higher is better ↑)")
ax.set_title(
    "Throughput vs hit-ratio trade-off at cap=1024\n"
    "★ = orig reference, line = j4 sweep over SHARDS"
)
ax.invert_xaxis()  # rightward = better throughput intuitively
ax.legend(fontsize=9, loc="lower left")
fig.tight_layout()
fig.savefig(OUT / "j4_tradeoff_scatter.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 6: cap sweep — hit ratio levels (orig vs j4) per skew, log cap axis.
# Companion to Plot 3 — shows absolute hit ratio shape, not just delta.
# ---------------------------------------------------------------------------
hr_long = hr_pivot.melt(
    id_vars=["skew", "capacity"],
    value_vars=["orig", "j4"],
    var_name="variant",
    value_name="hit_ratio",
)

g = sns.relplot(
    data=hr_long,
    kind="line",
    x="capacity",
    y="hit_ratio",
    hue="variant",
    style="variant",
    markers=True,
    dashes={"orig": "", "j4": (4, 2)},
    col="skew",
    palette={"orig": "#1f77b4", "j4": "#2ca02c"},
    height=4.0,
    aspect=1.0,
)
for ax in g.axes.flat:
    ax.set_xscale("log")
g.set_axis_labels("capacity (log)", "hit ratio")
g.set_titles("skew={col_name}")
g.figure.suptitle("Hit ratio level — orig (solid) vs j4 (dashed)", y=1.03)
g.figure.savefig(OUT / "j4_capsweep_hitratio_level.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# ---------------------------------------------------------------------------
# L1d-footprint addendum (2026-05-05): is per_shard or total footprint
# the dominant variable? Three sweeps:
#   - cap-sweep at SHARDS=8  (per_shard scales with cap)
#   - cap=256 fixed, SHARDS sweep (total stays L1d-resident)
#   - large-cap × multi-SHARDS (isolate per_shard at fixed total)
# ---------------------------------------------------------------------------

# Entry width assumption for footprint estimate. SieveCache<u64,u64>:
# Entry { key: u64, value: u64, visited: bool } padded to 24 B.
# tags array adds 1 B per slot. order_cap = 2 * capacity (32-aligned).
ENTRY_BYTES = 24
TAG_BYTES = 1
PER_SLOT_BYTES = ENTRY_BYTES + TAG_BYTES  # 25 B per slot, ×2 for order_cap


def _footprint_kb(per_shard_cap: int, shards: int) -> float:
    return per_shard_cap * 2 * PER_SLOT_BYTES * shards / 1024.0


# Sweep A: cap fine sweep at SHARDS=8 ----------------------------------------
sweepA = _read_bench_csv(PROFILES / "j4_capsweep_n8_2026-05-05.csv")
sweepA["per_shard"] = sweepA.apply(
    lambda r: r["capacity"] // 8 if r["variant"] == "j4_n8" else r["capacity"],
    axis=1,
)
sweepA["shards"] = sweepA["variant"].map(
    {"orig": 1, "j3": 1, "j4_n8": 8}
)
sweepA["footprint_kb"] = sweepA.apply(
    lambda r: _footprint_kb(r["per_shard"], int(r["shards"])), axis=1
)
sweepA["ns_per_op"] = sweepA["elapsed_ns"] / (sweepA["hits"] + sweepA["misses"])

fig, axes = plt.subplots(1, 2, figsize=(14, 5.5), sharey=False)
for ax, skew in zip(axes, [0.6, 1.0]):
    sub = sweepA[sweepA["skew"] == skew]
    sns.lineplot(
        data=sub,
        x="capacity",
        y="ns_per_op",
        hue="variant",
        style="variant",
        markers=True,
        dashes=False,
        palette={"orig": "#1f77b4", "j3": "#d62728", "j4_n8": "#2ca02c"},
        hue_order=["orig", "j3", "j4_n8"],
        ax=ax,
    )
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_title(f"skew={skew}")
    ax.set_xlabel("total capacity (log)")
    ax.set_ylabel("ns / op")
    # Mark approximate L1d boundary (i5-12600K P-core L1d = 48 KB).
    # cap that puts SHARDS=8 footprint at 48 KB: cap × 50 / 1024 = 48 → cap≈983.
    ax.axvline(983, color="grey", linestyle=":", linewidth=1, alpha=0.7)
    ax.text(
        983 * 1.05, ax.get_ylim()[1] * 0.6, "L1d ≈ 48 KB", rotation=90,
        fontsize=9, color="grey",
    )
fig.suptitle(
    "Sweep A — ns/op vs cap (SHARDS=8). j3/j4 scale with per_shard, orig stays flat.",
    y=1.02,
)
fig.tight_layout()
fig.savefig(OUT / "j4_l1d_sweepA_capfine.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# Sweep B: cap=256 fixed, SHARDS sweep ---------------------------------------
sweepB = _read_bench_csv(PROFILES / "j4_smalltotal_shardsweep_2026-05-05.csv")
sweepB["shards"] = sweepB["variant"].map(_shard_n)
sweepB.loc[sweepB["variant"] == "orig", "shards"] = 0  # plot as horizontal line ref
sweepB.loc[sweepB["variant"] == "j3", "shards"] = 1
sweepB["ns_per_op"] = sweepB["elapsed_ns"] / (sweepB["hits"] + sweepB["misses"])

fig, ax = plt.subplots(figsize=(10, 5.5))
shard_only = sweepB[sweepB["shards"] >= 1].copy()
sns.lineplot(
    data=shard_only,
    x="shards",
    y="ns_per_op",
    hue="skew",
    style="skew",
    markers=True,
    dashes=False,
    palette="viridis",
    ax=ax,
)
# orig reference lines per skew
orig_only = sweepB[sweepB["variant"] == "orig"]
for _, row in orig_only.iterrows():
    ax.axhline(
        row["ns_per_op"], color="grey", linestyle="--", linewidth=1, alpha=0.5,
    )
    ax.text(
        ax.get_xlim()[1] * 0.7,
        row["ns_per_op"] * 1.02,
        f"orig (skew={row['skew']})",
        fontsize=9, color="grey",
    )
ax.set_xscale("log", base=2)
ax.set_xlabel("SHARDS (cap=256 fixed, total ≈ 12 KB ⊂ L1d)")
ax.set_ylabel("ns / op")
ax.set_title(
    "Sweep B — small-total shard sweep. Total stays in L1d at every N.\n"
    "Time approaches a floor as N grows (per_shard → constant fixed cost)."
)
fig.tight_layout()
fig.savefig(OUT / "j4_l1d_sweepB_smalltotal.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# Sweep C: large cap, vary SHARDS — per_shard isolation ----------------------
sweepC = _read_bench_csv(PROFILES / "j4_pershard_isolation_2026-05-05.csv")
sweepC["shards"] = sweepC["variant"].map(_shard_n).astype(int)
sweepC["per_shard"] = sweepC["capacity"] // sweepC["shards"]
sweepC["ns_per_op"] = sweepC["elapsed_ns"] / (sweepC["hits"] + sweepC["misses"])

fig, axes = plt.subplots(1, 2, figsize=(14, 5.5), sharey=False)
for ax, skew in zip(axes, [0.6, 1.0]):
    sub = sweepC[sweepC["skew"] == skew]
    sns.lineplot(
        data=sub,
        x="per_shard",
        y="ns_per_op",
        hue="capacity",
        style="capacity",
        markers=True,
        dashes=False,
        palette="rocket_r",
        ax=ax,
    )
    ax.set_xscale("log", base=2)
    ax.set_xlabel("per_shard (= cap / SHARDS)")
    ax.set_ylabel("ns / op")
    ax.set_title(f"skew={skew}")
fig.suptitle(
    "Sweep C — at fixed total cap, ns/op tracks per_shard, not total footprint.\n"
    "Curves for cap ∈ {1024,4096,8192,16384} collapse onto a single per_shard curve.",
    y=1.02,
)
fig.tight_layout()
fig.savefig(OUT / "j4_l1d_sweepC_pershard_isolation.png", dpi=150, bbox_inches="tight")
plt.close(fig)

print(f"wrote figures to {OUT}")
for p in sorted(OUT.glob("j4_*.png")):
    print(" -", p.name)
