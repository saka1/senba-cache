"""Visualize sieve_j3 bench results from docs/reports/2026-05-04-sieve-j3-bench.md.

Run via: `uv run python scripts/plot_j3.py`
Outputs PNGs to docs/figures/.
"""

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns

OUT = Path(__file__).resolve().parent.parent / "docs" / "figures"
OUT.mkdir(parents=True, exist_ok=True)

sns.set_theme(style="whitegrid", context="talk")

# ---------------------------------------------------------------------------
# Stage 1: initial j3 (FxHash-less, naive impl) vs all variants — line 55-68.
# Columns: skew, cap, orig, v0, v1, v2, v3, j3
# ---------------------------------------------------------------------------
INITIAL_ALL = [
    (0.6, 100, 38.17, 42.60, 46.87, 42.55, 46.36, 36.50),
    (0.6, 1000, 35.53, 40.22, 42.27, 38.06, 41.90, 78.96),
    (0.6, 10000, 34.96, 39.04, 41.51, 37.93, 40.21, 559.12),
    (0.8, 100, 35.95, 39.99, 43.05, 39.55, 42.37, 35.46),
    (0.8, 1000, 32.03, 34.76, 38.50, 34.62, 37.04, 70.98),
    (0.8, 10000, 30.39, 34.00, 35.39, 33.74, 35.31, 466.89),
    (1.0, 100, 33.14, 36.40, 37.85, 36.17, 38.72, 33.95),
    (1.0, 1000, 25.88, 28.16, 29.05, 27.62, 29.44, 55.21),
    (1.0, 10000, 21.52, 24.02, 24.63, 23.68, 24.81, 239.93),
    (1.2, 100, 23.20, 25.37, 26.22, 25.77, 25.96, 24.06),
    (1.2, 1000, 16.24, 17.84, 18.28, 18.23, 18.76, 33.47),
    (1.2, 10000, 14.56, 15.99, 16.03, 15.86, 15.96, 108.38),
]

# ---------------------------------------------------------------------------
# Stage progression for j3 (and orig where applicable).
# Stages:
#   initial       — first impl, scalar tail bug + SipHash overkill
#   after_fix     — order_cap aligned + FxHash; orig still on SipHash
#   xxh3_fair     — both on XXH3 (paper-faithful)
#   after_refactor — Option->MaybeUninit, drop dead counter, hash_one
# ---------------------------------------------------------------------------
STAGE_ROWS = []
# initial (j3 vs orig from main table)
initial_pairs = [
    (0.6, 100, 38.17, 36.50), (0.6, 1000, 35.53, 78.96), (0.6, 10000, 34.96, 559.12),
    (0.8, 100, 35.95, 35.46), (0.8, 1000, 32.03, 70.98), (0.8, 10000, 30.39, 466.89),
    (1.0, 100, 33.14, 33.95), (1.0, 1000, 25.88, 55.21), (1.0, 10000, 21.52, 239.93),
    (1.2, 100, 23.20, 24.06), (1.2, 1000, 16.24, 33.47), (1.2, 10000, 14.56, 108.38),
]
for skew, cap, o, j in initial_pairs:
    STAGE_ROWS.append(("initial", skew, cap, "orig", o))
    STAGE_ROWS.append(("initial", skew, cap, "j3", j))

after_fix_pairs = [
    (0.6, 100, 37.05, 19.37), (0.6, 1000, 35.40, 63.93), (0.6, 10000, 33.50, 644.27),
    (0.8, 100, 34.89, 19.49), (0.8, 1000, 31.22, 59.94), (0.8, 10000, 30.18, 437.98),
    (1.0, 100, 32.03, 19.52), (1.0, 1000, 24.72, 46.73), (1.0, 10000, 21.49, 250.94),
    (1.2, 100, 22.65, 16.62), (1.2, 1000, 17.04, 31.61), (1.2, 10000, 16.97, 102.23),
]
for skew, cap, o, j in after_fix_pairs:
    STAGE_ROWS.append(("after_fix", skew, cap, "orig", o))
    STAGE_ROWS.append(("after_fix", skew, cap, "j3", j))

xxh3_rows = [
    (0.6, 100, 38.59, 46.93, 27.16), (0.6, 1000, 42.22, 48.64, 73.30), (0.6, 10000, 34.68, 42.12, 606.12),
    (0.8, 100, 37.74, 43.10, 27.54), (0.8, 1000, 32.45, 40.18, 67.65), (0.8, 10000, 30.81, 36.44, 467.20),
    (1.0, 100, 30.91, 37.78, 28.43), (1.0, 1000, 26.14, 28.96, 55.98), (1.0, 10000, 20.97, 23.81, 275.26),
    (1.2, 100, 21.52, 27.98, 21.90), (1.2, 1000, 16.92, 19.02, 34.31), (1.2, 10000, 15.79, 16.74, 109.46),
]
for skew, cap, o, v3, j in xxh3_rows:
    STAGE_ROWS.append(("xxh3_fair", skew, cap, "orig", o))
    STAGE_ROWS.append(("xxh3_fair", skew, cap, "v3", v3))
    STAGE_ROWS.append(("xxh3_fair", skew, cap, "j3", j))

refactor_rows = [
    (0.6, 100, 37.18, 50.70, 25.23), (0.6, 1000, 39.44, 50.45, 72.64), (0.6, 10000, 34.58, 44.68, 655.92),
    (0.8, 100, 35.76, 47.71, 27.03), (0.8, 1000, 31.87, 40.88, 66.22), (0.8, 10000, 29.23, 36.36, 515.13),
    (1.0, 100, 30.44, 39.78, 26.61), (1.0, 1000, 24.59, 30.34, 53.56), (1.0, 10000, 21.21, 24.74, 288.08),
    (1.2, 100, 21.34, 27.36, 20.85), (1.2, 1000, 17.41, 20.26, 32.50), (1.2, 10000, 15.54, 17.07, 120.22),
]
for skew, cap, o, v3, j in refactor_rows:
    STAGE_ROWS.append(("after_refactor", skew, cap, "orig", o))
    STAGE_ROWS.append(("after_refactor", skew, cap, "v3", v3))
    STAGE_ROWS.append(("after_refactor", skew, cap, "j3", j))

stages = pd.DataFrame(STAGE_ROWS, columns=["stage", "skew", "cap", "variant", "ms"])
stages["stage"] = pd.Categorical(
    stages["stage"],
    categories=["initial", "after_fix", "xxh3_fair", "after_refactor"],
    ordered=True,
)

# ---------------------------------------------------------------------------
# Plot 1: All-variants barplot per (skew, cap) — STAGE_INITIAL
# ---------------------------------------------------------------------------
df_init = pd.DataFrame(
    INITIAL_ALL,
    columns=["skew", "cap", "orig", "v0", "v1", "v2", "v3", "j3"],
).melt(id_vars=["skew", "cap"], var_name="variant", value_name="ms")

g = sns.catplot(
    data=df_init,
    kind="bar",
    x="variant",
    y="ms",
    col="cap",
    row="skew",
    hue="variant",
    order=["orig", "v0", "v1", "v2", "v3", "j3"],
    palette="deep",
    sharey=False,
    height=2.6,
    aspect=1.4,
    legend=False,
)
g.set_titles("skew={row_name}  cap={col_name}")
g.set_axis_labels("variant", "median ms (insert_only, 1M ops)")
g.figure.suptitle("Initial bench — all variants, per (skew, cap)", y=1.02)
g.figure.savefig(OUT / "j3_initial_all_variants.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# ---------------------------------------------------------------------------
# Plot 2: j3/orig ratio heatmap, after_refactor (final fair-fight state)
# ---------------------------------------------------------------------------
final = stages[stages["stage"] == "after_refactor"].pivot_table(
    index="skew", columns=["cap", "variant"], values="ms"
)
ratio = pd.DataFrame(
    {cap: final[(cap, "j3")] / final[(cap, "orig")] for cap in [100, 1000, 10000]}
)
ratio.columns.name = "cap"

fig, ax = plt.subplots(figsize=(7, 4))
sns.heatmap(
    ratio,
    annot=True,
    fmt=".2f",
    cmap="RdYlGn_r",
    center=1.0,
    vmin=0.5,
    vmax=2.5,
    cbar_kws={"label": "j3 / orig (lower = j3 wins)"},
    ax=ax,
)
ax.set_title("j3 / orig speed ratio (after refactor, XXH3 fair fight)")
fig.savefig(OUT / "j3_vs_orig_ratio_heatmap.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 3: j3 evolution across stages (cap=100 — j3's win zone)
# ---------------------------------------------------------------------------
j3_stage = stages[(stages["variant"] == "j3") & (stages["cap"] == 100)]

fig, ax = plt.subplots(figsize=(8, 5))
sns.lineplot(
    data=j3_stage,
    x="stage",
    y="ms",
    hue="skew",
    marker="o",
    palette="viridis",
    ax=ax,
)
ax.set_title("j3 ms per stage (cap=100) — optimization journey")
ax.set_ylabel("median ms")
fig.savefig(OUT / "j3_evolution_cap100.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 4: j3/orig ratio across stages (cap=100, all skews)
# ---------------------------------------------------------------------------
ratio_stages = (
    stages[stages["cap"] == 100]
    .pivot_table(index=["stage", "skew"], columns="variant", values="ms")
    .reset_index()
)
ratio_stages["j3_over_orig"] = ratio_stages["j3"] / ratio_stages["orig"]

fig, ax = plt.subplots(figsize=(8, 5))
sns.lineplot(
    data=ratio_stages,
    x="stage",
    y="j3_over_orig",
    hue="skew",
    marker="o",
    palette="viridis",
    ax=ax,
)
ax.axhline(1.0, color="black", linewidth=1, linestyle="--", alpha=0.6)
ax.set_title("j3/orig ratio per stage (cap=100) — <1 means j3 wins")
ax.set_ylabel("j3 / orig")
fig.savefig(OUT / "j3_ratio_evolution_cap100.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# ---------------------------------------------------------------------------
# Plot 5: After refactor — absolute ms, log scale, all caps
# ---------------------------------------------------------------------------
final_long = stages[stages["stage"] == "after_refactor"]
g = sns.catplot(
    data=final_long,
    kind="bar",
    x="cap",
    y="ms",
    hue="variant",
    col="skew",
    palette="deep",
    height=3.8,
    aspect=0.95,
    col_wrap=2,
)
for ax in g.axes.flat:
    ax.set_yscale("log")
g.set_axis_labels("capacity (log scale on y)", "median ms")
g.figure.suptitle("After refactor — orig vs v3 vs j3 across (skew, cap)", y=1.02)
g.figure.savefig(OUT / "j3_after_refactor_abs.png", dpi=150, bbox_inches="tight")
plt.close(g.figure)

# ---------------------------------------------------------------------------
# Plot 6: orig vs j3 only — sweep over cap and skew (after_refactor)
# ---------------------------------------------------------------------------
pair = stages[
    (stages["stage"] == "after_refactor") & (stages["variant"].isin(["orig", "j3"]))
].copy()

fig, axes = plt.subplots(1, 2, figsize=(13, 5))

# (a) ms vs cap, faceted by skew via hue, log-log
sns.lineplot(
    data=pair,
    x="cap",
    y="ms",
    hue="skew",
    style="variant",
    markers=True,
    dashes={"orig": "", "j3": (4, 2)},
    palette="viridis",
    ax=axes[0],
)
axes[0].set_xscale("log")
axes[0].set_yscale("log")
axes[0].set_title("orig (solid) vs j3 (dashed) — ms vs cap")
axes[0].set_xlabel("capacity")
axes[0].set_ylabel("median ms (log)")

# (b) ms vs skew, faceted by cap via hue
sns.lineplot(
    data=pair,
    x="skew",
    y="ms",
    hue="cap",
    style="variant",
    markers=True,
    dashes={"orig": "", "j3": (4, 2)},
    palette="rocket",
    ax=axes[1],
)
axes[1].set_yscale("log")
axes[1].set_title("orig (solid) vs j3 (dashed) — ms vs skew")
axes[1].set_xlabel("Zipf skew")
axes[1].set_ylabel("median ms (log)")

fig.suptitle("orig vs j3 across (cap, skew) — after refactor, XXH3 fair fight", y=1.02)
fig.tight_layout()
fig.savefig(OUT / "j3_vs_orig_sweep.png", dpi=150, bbox_inches="tight")
plt.close(fig)

# (c) j3/orig ratio vs cap per skew — same data, ratio view
ratio_df = pair.pivot_table(index=["skew", "cap"], columns="variant", values="ms").reset_index()
ratio_df["j3_over_orig"] = ratio_df["j3"] / ratio_df["orig"]

ratio_df2 = ratio_df[ratio_df["cap"].isin([100, 1000])].copy()
ratio_df2["cap_label"] = ratio_df2["cap"].astype(str)

fig, ax = plt.subplots(figsize=(7, 5))
sns.barplot(
    data=ratio_df2,
    x="cap_label",
    y="j3_over_orig",
    hue="skew",
    order=["100", "1000"],
    palette="viridis",
    ax=ax,
)
ax.axhline(1.0, color="black", linewidth=1.2, linestyle="--", alpha=0.7)
ax.set_title("j3 / orig ratio by cap (after refactor) — <1.0 means j3 wins")
ax.set_xlabel("capacity")
ax.set_ylabel("j3 / orig")
fig.savefig(OUT / "j3_vs_orig_ratio_sweep.png", dpi=150, bbox_inches="tight")
plt.close(fig)

print(f"wrote figures to {OUT}")
for p in sorted(OUT.glob("*.png")):
    print(" -", p.name)
