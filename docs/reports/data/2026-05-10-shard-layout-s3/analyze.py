#!/usr/bin/env python3
"""Aggregate before/after Twitter sweep CSVs and compute per-cell deltas.

After/Before ratio < 1.0 means the S3 + capacity-removal change is faster
on that cell.
"""
import csv
import math
from collections import defaultdict
from pathlib import Path
from statistics import median

data_dir = Path(__file__).resolve().parent
buckets = defaultdict(list)  # (config, source, cluster, cap, per_shard) -> [ns/op]
hits_buckets = defaultdict(list)

for cfg in ("before", "after"):
    with open(data_dir / f"{cfg}.csv") as f:
        for row in csv.DictReader(f):
            key = (
                row["config"],
                row["source"],
                row["cluster"],
                int(row["capacity"]),
                int(row["per_shard"]),
            )
            ns_per_op = int(row["elapsed_ns"]) / int(row["len"])
            buckets[key].append(ns_per_op)
            hits_buckets[key].append(int(row["hits"]))

# Index by (source, cluster, cap, per_shard) -> {cfg: median_ns}
cells = defaultdict(dict)
hits_cells = defaultdict(dict)
for (cfg, s, cl, cap, ps), vals in buckets.items():
    cells[(s, cl, cap, ps)][cfg] = median(vals)
    hits_cells[(s, cl, cap, ps)][cfg] = median(hits_buckets[(cfg, s, cl, cap, ps)])

# --------- per-cell table ---------
print(
    f"{'source':<14} {'cluster':<10} {'cap':>6} {'ps':>3} | "
    f"{'before':>9} {'after':>9} | {'after/before':>13} | hits"
)
print("-" * 90)
for key in sorted(cells.keys()):
    row = cells[key]
    if "before" not in row or "after" not in row:
        continue
    s, cl, cap, ps = key
    b, a = row["before"], row["after"]
    delta = (a - b) / b * 100
    flag = ""
    if delta <= -3.0:
        flag = "  <-- improved"
    elif delta >= 3.0:
        flag = "  <-- regressed"
    print(
        f"{s:<14} {cl:<10} {cap:>6} {ps:>3} | "
        f"{b:>8.2f}n {a:>8.2f}n | {delta:>+11.2f}% | "
        f"{int(hits_cells[key]['before'])}{flag}"
    )

# --------- per-source geomean ---------
print()
print("Per-source geomean of (after / before) ratio:")
print(f"{'source':<14} | {'after/before':>14} | n_cells")
print("-" * 50)
by_source = defaultdict(list)
for key, row in cells.items():
    if "before" not in row or "after" not in row:
        continue
    by_source[key[0]].append(row["after"] / row["before"])
for s, ratios in sorted(by_source.items()):
    g = math.exp(sum(math.log(x) for x in ratios) / len(ratios))
    print(f"{s:<14} | {(g - 1) * 100:>+13.2f}% | {len(ratios)}")

# overall
all_ratios = [r for ratios in by_source.values() for r in ratios]
overall = math.exp(sum(math.log(x) for x in all_ratios) / len(all_ratios))
print(f"{'OVERALL':<14} | {(overall - 1) * 100:>+13.2f}% | {len(all_ratios)}")

# --------- Win/Loss summary ---------
print()
imp = sum(1 for ratios in by_source.values() for r in ratios if (r - 1) * 100 <= -1.0)
reg = sum(1 for ratios in by_source.values() for r in ratios if (r - 1) * 100 >= 1.0)
neu = sum(1 for ratios in by_source.values() for r in ratios if abs((r - 1) * 100) < 1.0)
print(f"cells improved (>=1%):  {imp}")
print(f"cells regressed (>=1%): {reg}")
print(f"cells neutral (<1%):    {neu}")

# HR equivalence sanity (oracle): hits should match between before and after
print()
mismatch = 0
for key in cells.keys():
    if "before" in hits_cells[key] and "after" in hits_cells[key]:
        if hits_cells[key]["before"] != hits_cells[key]["after"]:
            mismatch += 1
            print(f"  HR MISMATCH: {key} before={hits_cells[key]['before']} after={hits_cells[key]['after']}")
if mismatch == 0:
    print(f"hit-count equivalence: PASS ({len(cells)} cells, before==after)")
