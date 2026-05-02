#!/usr/bin/env python3
"""criterion の estimates.json を読んで orig vs v0 の表を出す。

`cargo bench --bench micro` を走らせた後で実行する。各ケースを
target/criterion/<group>/<impl>_skew<X>/<cap>/new/estimates.json から拾う。
"""
import json
import os
import sys

ROOT = "target/criterion"
GROUPS = ["insert_only", "mixed_80r_20w"]
IMPLS = ["orig", "v0"]
SKEWS = ["1.05", "1.2"]   # benches/micro.rs の SKEWS と一致させる
CAPS = ["1024", "16384"]  # benches/micro.rs の CAPS と一致させる
TRACE_LEN = 50_000        # benches/micro.rs の TRACE_LEN と一致させる


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

    print("\n=== orig vs v0 (same group/skew/cap) ===")
    print(f"{'group':<14} {'skew':<5} {'cap':>6} {'orig(ms)':>10} {'v0(ms)':>10} {'v0/orig':>8}")
    keys = sorted({(r["group"], r["skew"], r["cap"]) for r in rows})
    for grp, skew, cap in keys:
        o = next((r for r in rows if r["group"] == grp and r["impl"] == "orig"
                  and r["skew"] == skew and r["cap"] == cap), None)
        v = next((r for r in rows if r["group"] == grp and r["impl"] == "v0"
                  and r["skew"] == skew and r["cap"] == cap), None)
        if o and v:
            print(f"{grp:<14} {skew:<5} {cap:>6} {o['mean_ms']:>10.3f} "
                  f"{v['mean_ms']:>10.3f} {v['mean_ms']/o['mean_ms']:>7.2f}x")


if __name__ == "__main__":
    main()
