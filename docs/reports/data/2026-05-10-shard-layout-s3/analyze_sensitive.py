#!/usr/bin/env python3
"""Aggregate sensitive workload sweep (Zipf high-skew, ARC OLTP, ARC DS1).

Each cell has multi-second measurement to push above per-trial noise floor.
"""
import csv
import math
from collections import defaultdict
from pathlib import Path
from statistics import median, stdev

data_dir = Path(__file__).resolve().parent

def aggregate(csv_path, key_fields):
    """Load CSV; bucket by (config, *key_fields) -> [ns/op].
    Returns cells: dict[key_tuple] -> {cfg: median_ns}, with stats."""
    buckets = defaultdict(list)
    hits_buckets = defaultdict(list)
    with open(csv_path) as f:
        for row in csv.DictReader(f):
            key = tuple(row[k] for k in key_fields)
            full_key = (row["config"],) + key
            ns_per_op = int(row["elapsed_ns"]) / int(row["len"])
            buckets[full_key].append(ns_per_op)
            hits_buckets[full_key].append(int(row["hits"]))
    cells = defaultdict(dict)
    cells_stats = defaultdict(dict)
    hits_cells = defaultdict(dict)
    for (cfg, *rest), vals in buckets.items():
        k = tuple(rest)
        cells[k][cfg] = median(vals)
        cells_stats[k][cfg] = (
            min(vals), max(vals),
            stdev(vals) if len(vals) > 1 else 0.0
        )
        hits_cells[k][cfg] = median(hits_buckets[(cfg, *rest)])
    return cells, cells_stats, hits_cells


def report(name, cells, cells_stats, hits_cells, key_fields):
    print(f"\n=== {name} ===")
    head = " ".join(f"{k:<12}" for k in key_fields)
    print(f"{head} | {'before ns':>10} {'after ns':>10} {'after-stdev':>12} | {'after/before':>13} | hits | trials")
    print("-" * (len(head) + 80))
    ratios = []
    for key in sorted(cells.keys()):
        row = cells[key]
        if "before" not in row or "after" not in row:
            continue
        b, a = row["before"], row["after"]
        b_min, b_max, b_sd = cells_stats[key]["before"]
        a_min, a_max, a_sd = cells_stats[key]["after"]
        delta = (a - b) / b * 100
        flag = ""
        if delta <= -3.0: flag = " ▼ improved"
        elif delta >= 3.0: flag = " ▲ regressed"
        cv_a = (a_sd / a * 100) if a > 0 else 0
        kvals = " ".join(f"{v:<12}" for v in key)
        b_hits = hits_cells[key].get("before", 0)
        a_hits = hits_cells[key].get("after", 0)
        hr_match = "OK" if b_hits == a_hits else f"DIFF({b_hits}vs{a_hits})"
        print(
            f"{kvals} | {b:>9.2f}n {a:>9.2f}n {cv_a:>10.2f}%cv | {delta:>+11.2f}% | {hr_match:>4} | b{b_min:.1f}-{b_max:.1f} a{a_min:.1f}-{a_max:.1f}{flag}"
        )
        ratios.append(a / b)
    if ratios:
        g = math.exp(sum(math.log(x) for x in ratios) / len(ratios))
        print(f"\n  geomean (after/before): {(g - 1) * 100:+.2f}%   over {len(ratios)} cells")
        improved = sum(1 for r in ratios if (r - 1) * 100 <= -1.0)
        regressed = sum(1 for r in ratios if (r - 1) * 100 >= 1.0)
        neutral = len(ratios) - improved - regressed
        print(f"  win/loss (>=1% threshold): improved={improved}, regressed={regressed}, neutral={neutral}")


# Zipf
cells, stats, hits = aggregate(
    data_dir / "sensitive_zipf.csv",
    ["skew", "per_shard", "shards", "capacity"],
)
report("Zipf high-skew (len=10M, repeat=10, ~3s/trial, 3 trials)",
       cells, stats, hits,
       ["skew", "per_shard", "shards", "cap"])

# OLTP
cells, stats, hits = aggregate(
    data_dir / "sensitive_oltp.csv",
    ["capacity"],
)
report("ARC OLTP (--repeat 20, ~1s/trial, 3 trials)",
       cells, stats, hits,
       ["cap"])

# DS1
cells, stats, hits = aggregate(
    data_dir / "sensitive_ds1.csv",
    ["capacity"],
)
report("ARC DS1 (no repeat, ~5s/trial, 3 trials)",
       cells, stats, hits,
       ["cap"])
