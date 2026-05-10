"""lru / mini_moka_unsync / senba pareto (single combined grid).

Inputs (overwritten in place by sibling `run.sh` — git tracks history):
  data/arc.csv     (preset,variant,...,capacity,elapsed_ns,hits,misses,evictions)
  data/zipf.csv    (skew,variant,...,capacity,elapsed_ns,hits,misses,evictions)

Output: 1 figure containing every trace as a subplot.
  figures/pareto-grid.png   (ARC P1..P14 + Zipf α∈{0.8,1.0,1.2})

Each subplot: x = ns/op, y = HR, capacity sweep traced as a single line per variant.
Aggregates: median over `--repeat 3` runs per (trace, variant, capacity).
"""

from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"
FIGS = HERE / "figures"

VARIANTS = ["senba", "lru", "mini_moka_unsync"]
COLORS = {"senba": "#1f77b4", "lru": "#7f7f7f", "mini_moka_unsync": "#d62728"}
MARKERS = {"senba": "o", "lru": "s", "mini_moka_unsync": "^"}
LABELS = {
    # senba は SIEVE algorithm 自体は orig 一致 (oracle PASS) だが、実装が
    # set-associative tag array + AVX2 SIMD scan + visited bitmap で原典の
    # linked-list SIEVE とは別物。図中では「(SIEVE variant)」と明示する。
    "senba": "senba (SIEVE variant: set-assoc + SIMD)",
    "lru": "lru-rs (LRU)",
    "mini_moka_unsync": "mini_moka::unsync (W-TinyLFU)",
}


def _fmt_cap(c: int) -> str:
    c = int(c)
    if c >= 1_000_000:
        return f"{c / 1_000_000:g}M"
    # power-of-2 ≥ 1024 → binary k (1024 → "1k", 65536 → "64k")
    if c >= 1024 and (c & (c - 1)) == 0:
        return f"{c >> 10}k"
    if c >= 1000:
        return f"{c / 1000:g}k"
    return str(c)


def _arc_preset_sort_key(p: str) -> tuple[int, int]:
    if p.startswith("p") and p[1:].isdigit():
        return (0, int(p[1:]))
    return (1, hash(p) & 0xFFFF)


def _load_pareto() -> list[tuple[str, pd.DataFrame]]:
    """Return [(subplot_title, df_with_columns(variant,capacity,ns_per_op,hr))]."""
    panels: list[tuple[str, pd.DataFrame]] = []

    arc_csv = DATA / "arc.csv"
    if arc_csv.exists():
        arc = pd.read_csv(arc_csv)
        arc["ns_per_op"] = arc["elapsed_ns"] / (arc["hits"] + arc["misses"])
        arc["hr"] = arc["hits"] / (arc["hits"] + arc["misses"])
        agg = (
            arc.groupby(["preset", "variant", "capacity"], as_index=False)
            .agg(ns_per_op=("ns_per_op", "median"), hr=("hr", "median"))
        )
        for preset in sorted(agg["preset"].unique(), key=_arc_preset_sort_key):
            panels.append((f"ARC {preset.upper()}", agg[agg["preset"] == preset]))

    zipf_csv = DATA / "zipf.csv"
    if zipf_csv.exists():
        zipf = pd.read_csv(zipf_csv)
        zipf["ns_per_op"] = zipf["elapsed_ns"] / (zipf["hits"] + zipf["misses"])
        zipf["hr"] = zipf["hits"] / (zipf["hits"] + zipf["misses"])
        agg = (
            zipf.groupby(["skew", "variant", "capacity"], as_index=False)
            .agg(ns_per_op=("ns_per_op", "median"), hr=("hr", "median"))
        )
        for skew in sorted(agg["skew"].unique()):
            panels.append((f"Zipf α={skew}", agg[agg["skew"] == skew]))

    return panels


def _pareto_grid(panels: list[tuple[str, pd.DataFrame]], ncols: int, out_path: Path) -> None:
    nrows = (len(panels) + ncols - 1) // ncols
    header_h = 1.0  # suptitle + legend 帯
    fig, axes = plt.subplots(
        nrows, ncols,
        figsize=(3.6 * ncols, 2.9 * nrows + header_h),
        squeeze=False,
    )
    for idx, (title, sub) in enumerate(panels):
        ax = axes[idx // ncols][idx % ncols]
        for v in VARIANTS:
            s = sub[sub["variant"] == v].sort_values("capacity")
            if s.empty:
                continue
            ax.plot(
                s["ns_per_op"], s["hr"],
                marker=MARKERS[v], color=COLORS[v], label=LABELS[v],
                linewidth=1.4, markersize=5,
            )
            for _, row in s.iterrows():
                ax.annotate(
                    _fmt_cap(row["capacity"]),
                    xy=(row["ns_per_op"], row["hr"]),
                    xytext=(4, 3), textcoords="offset points",
                    fontsize=6, color=COLORS[v],
                )
        ax.set_title(title, fontsize=10)
        ax.set_xlabel("ns/op", fontsize=8)
        ax.set_ylabel("Hit ratio", fontsize=8)
        ax.tick_params(labelsize=7)
        ax.grid(True, which="both", alpha=0.3)
    for j in range(len(panels), nrows * ncols):
        axes[j // ncols][j % ncols].set_visible(False)

    handles, labels = axes[0][0].get_legend_handles_labels()
    total_h = 2.9 * nrows + header_h
    fig.suptitle(
        "lru-rs / mini_moka::unsync / senba — Pareto (ns/op vs HR, capacity sweep traced)",
        y=1.0 - 0.22 / total_h, fontsize=12,
    )
    fig.legend(
        handles, labels, loc="upper center", ncol=len(VARIANTS),
        bbox_to_anchor=(0.5, 1.0 - 0.62 / total_h), fontsize=10, frameon=False,
    )
    fig.tight_layout(rect=(0, 0, 1, 1.0 - header_h / total_h))
    fig.savefig(out_path, dpi=150, bbox_inches="tight")
    plt.close(fig)
    print(f"wrote {out_path}")


def main() -> None:
    FIGS.mkdir(parents=True, exist_ok=True)
    panels = _load_pareto()
    if not panels:
        raise SystemExit("no input CSVs under data/")
    _pareto_grid(panels, ncols=5, out_path=FIGS / "pareto-grid.png")


if __name__ == "__main__":
    main()
