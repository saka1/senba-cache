#!/usr/bin/env python3
"""Aggregate AlignedTags A/B/C sweep CSVs and compute per-cell deltas vs A."""
import csv
from collections import defaultdict
from pathlib import Path
from statistics import median

data_dir = Path(__file__).resolve().parent
paths = [str(data_dir / f"aligned_{c}.csv") for c in "ABC"]

# (config, source, cluster, cap, per_shard) -> [elapsed_ns_per_op, ...]
buckets = defaultdict(list)
hits_buckets = defaultdict(list)

for p in paths:
    with open(p) as f:
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

# Index by (source, cluster, cap, per_shard)
cells = defaultdict(dict)  # (s,cl,cap,ps) -> {config: median_ns}
hits_cells = defaultdict(dict)
for (cfg, s, cl, cap, ps), vals in buckets.items():
    cells[(s, cl, cap, ps)][cfg] = median(vals)
    hits_cells[(s, cl, cap, ps)][cfg] = median(hits_buckets[(cfg, s, cl, cap, ps)])

# Print per-cell comparison
print(
    f"{'source':<14} {'cluster':<10} {'cap':>6} {'ps':>3} | "
    f"{'A ns/op':>8} {'B ns/op':>8} {'C ns/op':>8} | "
    f"{'B vs A':>7} {'C vs A':>7} | hits_A"
)
print("-" * 100)
sorted_keys = sorted(cells.keys())
for (s, cl, cap, ps) in sorted_keys:
    row = cells[(s, cl, cap, ps)]
    if not all(c in row for c in "ABC"):
        continue
    a, b, c = row["A"], row["B"], row["C"]
    db = (b - a) / a * 100
    dc = (c - a) / a * 100
    h = hits_cells[(s, cl, cap, ps)]["A"]
    print(
        f"{s:<14} {cl:<10} {cap:>6} {ps:>3} | "
        f"{a:>8.2f} {b:>8.2f} {c:>8.2f} | "
        f"{db:>+6.2f}% {dc:>+6.2f}% | {int(h)}"
    )

# Per-source aggregate (geomean of per-cell ratios)
print()
print(
    f"{'source':<14} | {'B/A geomean':>12} {'C/A geomean':>12} | n_cells"
)
print("-" * 60)
import math
by_source = defaultdict(list)
for (s, cl, cap, ps), row in cells.items():
    if not all(c in row for c in "ABC"):
        continue
    by_source[s].append((row["B"] / row["A"], row["C"] / row["A"]))
for s, ratios in sorted(by_source.items()):
    bs = [r[0] for r in ratios]
    cs = [r[1] for r in ratios]
    bg = math.exp(sum(math.log(x) for x in bs) / len(bs))
    cg = math.exp(sum(math.log(x) for x in cs) / len(cs))
    print(
        f"{s:<14} | {(bg-1)*100:>+11.2f}% {(cg-1)*100:>+11.2f}% | {len(ratios)}"
    )
