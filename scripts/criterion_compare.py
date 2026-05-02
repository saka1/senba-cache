#!/usr/bin/env python3
"""criterion の estimates.json を読んで orig vs v0 の表を出す。

`cargo bench --bench micro` を走らせた後で実行する。各ケースを
target/criterion/<group>/<impl>_skew<X>/<cap>/new/estimates.json から拾う。
"""
import json
import os
import sys

ROOT = "target/criterion"
GROUPS = ["insert_only"]
IMPLS = ["orig", "v0", "v1", "v2"]
# 以下 3 定数は benches/micro.rs の SKEWS / CAP_RATIOS / TRACE_LEN と一致させる。
SKEWS = ["0.6", "0.8", "1", "1.2"]
N_KEYS = 100_000
CAP_RATIOS = [0.001, 0.01, 0.1]
CAPS = [str(round(N_KEYS * r)) for r in CAP_RATIOS]
TRACE_LEN = 1_000_000


def load(group, impl, skew, cap):
    p = os.path.join(ROOT, group, f"{impl}_skew{skew}", cap, "new", "estimates.json")
    if not os.path.exists(p):
        return None
    return json.load(open(p))


def main():
    rows = []
    for group in GROUPS:
        for impl in IMPLS:
            for skew in SKEWS:
                for cap in CAPS:
                    est = load(group, impl, skew, cap)
                    if est is None:
                        print(f"missing: {group}/{impl}/skew{skew}/{cap}", file=sys.stderr)
                        continue
                    mean_ns = est["mean"]["point_estimate"]
                    rows.append({
                        "group": group,
                        "impl": impl,
                        "skew": skew,
                        "cap": cap,
                        "mean_ms": mean_ns / 1e6,
                        "ns_per_op": mean_ns / TRACE_LEN,
                        "mops_s": TRACE_LEN / mean_ns * 1e3,
                    })

    print(f"{'group':<14} {'impl':<5} {'skew':<5} {'cap':>6} {'mean(ms)':>10} {'ns/op':>8} {'Mops/s':>8}")
    for r in rows:
        print(f"{r['group']:<14} {r['impl']:<5} {r['skew']:<5} {r['cap']:>6} "
              f"{r['mean_ms']:>10.3f} {r['ns_per_op']:>8.1f} {r['mops_s']:>8.1f}")

    def find(grp, impl, skew, cap):
        return next((r for r in rows if r["group"] == grp and r["impl"] == impl
                     and r["skew"] == skew and r["cap"] == cap), None)

    print("\n=== orig / v0 / v1 / v2 並べて比較 (same group/skew/cap) ===")
    print(f"{'group':<14} {'skew':<5} {'cap':>6} "
          f"{'orig(ms)':>9} {'v0(ms)':>9} {'v1(ms)':>9} {'v2(ms)':>9} "
          f"{'v0/orig':>8} {'v1/orig':>8} {'v2/orig':>8} {'v2/v0':>7}")
    keys = sorted({(r["group"], r["skew"], r["cap"]) for r in rows})
    for grp, skew, cap in keys:
        o = find(grp, "orig", skew, cap)
        v = find(grp, "v0", skew, cap)
        w = find(grp, "v1", skew, cap)
        x = find(grp, "v2", skew, cap)
        if o and v and w and x:
            print(f"{grp:<14} {skew:<5} {cap:>6} "
                  f"{o['mean_ms']:>9.3f} {v['mean_ms']:>9.3f} "
                  f"{w['mean_ms']:>9.3f} {x['mean_ms']:>9.3f} "
                  f"{v['mean_ms']/o['mean_ms']:>7.2f}x "
                  f"{w['mean_ms']/o['mean_ms']:>7.2f}x "
                  f"{x['mean_ms']/o['mean_ms']:>7.2f}x "
                  f"{x['mean_ms']/v['mean_ms']:>6.2f}x")


if __name__ == "__main__":
    main()
