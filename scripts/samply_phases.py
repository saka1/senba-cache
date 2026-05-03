#!/usr/bin/env python3
"""samply JSON のサンプルを「hit-path / evict / compact / new-insert / other」で分類。

各サンプルの leaf address を addr2line -i で inline チェーンに展開し、
チェーンに含まれる「マーカー行」で分類する。マーカーは各 phase に一意な
コード行 (= その行はその phase でしか実行されない) を選ぶ。

例: v3 の "hit path" マーカーは `self.visited.set(qpos)` の行
    (existing-key の if-let の中にしか出ない)。

Usage:
  scripts/samply_phases.py <impl> <profile.json>
  impl ∈ {orig, v3}
"""
import json, sys, subprocess, collections, os

BIN = "target/release/deps/micro-d82976a2858802c6"

# 各 phase に一意な行。複数行を許す (どれかが chain にあれば match)。
# 0..n のチェックで先勝ち。
MARKERS = {
    "orig": [
        ("HIT",        ("sieve_orig.rs", {90, 91})),     # node.freq = 1; return None;
        ("EVICT",      ("sieve_orig.rs", set(range(195, 223)))),  # evict_one body
        ("UNLINK",     ("sieve_orig.rs", set(range(173, 193)))),  # unlink (called from evict + remove)
        ("INSERT-NEW", ("sieve_orig.rs", set(range(100, 113)))),  # alloc + link_at_head call site
        ("ALLOC",      ("sieve_orig.rs", set(range(139, 154)))),  # alloc_node / free_node
        ("LINK-HEAD",  ("sieve_orig.rs", set(range(157, 172)))),  # link_at_head body
    ],
    "v3": [
        ("HIT",        ("sieve_v3.rs", {152, 153})),   # visited.set(qpos); return None;
        ("COMPACT",    ("sieve_v3.rs", set(range(305, 346)))),  # compact body
        ("EVICT-SCAN", ("sieve_v3.rs", set(range(200, 255)))),  # find_victim_in_range
        ("DO-EVICT",   ("sieve_v3.rs", set(range(283, 298)))),  # do_evict body
        ("EVICT-CALL", ("sieve_v3.rs", set(range(256, 282)))),  # evict_one shell
        ("INSERT-NEW", ("sieve_v3.rs", set(range(162, 180)))),  # post-evict insert tail (incl maybe_compact branch)
        ("ALLOC",      ("sieve_v3.rs", set(range(182, 192)))),  # alloc_entry
        ("BITSET",     ("sieve_v3.rs", set(range(20, 66)))),    # BitSet helpers
    ],
}

def addr2line_chain(binary, addrs):
    """{addr: [(fn, file, line), ...]}  inline 階層も含む。"""
    res = {}
    for a in addrs:
        out = subprocess.run(
            ["addr2line", "-e", binary, "-f", "-C", "-i", f"0x{a:x}"],
            capture_output=True, text=True, check=True
        ).stdout.splitlines()
        chain = []
        for i in range(0, len(out), 2):
            if i + 1 >= len(out):
                break
            fn = out[i].strip()
            loc = out[i+1].strip()
            if ":" in loc:
                f, ln = loc.rsplit(":", 1)
                ln = int(ln) if ln.isdigit() else 0
            else:
                f, ln = loc, 0
            chain.append((fn, os.path.basename(f), ln))
        res[a] = chain
    return res

def classify(chain, markers):
    """chain (innermost-first) に対し、マーカー順に最初に match した phase を返す。"""
    chain_lines = {(f, ln) for _, f, ln in chain}
    for phase, (target_file, target_lines) in markers:
        for ln in target_lines:
            if (target_file, ln) in chain_lines:
                return phase
    return "OTHER"

def analyze(prof_path, impl_name):
    p = json.load(open(prof_path))
    main_lib_idx = next((i for i, L in enumerate(p["libs"]) if BIN.endswith(L["name"])), None)
    markers = MARKERS[impl_name]

    addr_self = collections.Counter()
    other_lib_total = 0
    for t in p["threads"]:
        if t["samples"]["length"] == 0:
            continue
        ft, funcs, rt = t["frameTable"], t["funcTable"], t["resourceTable"]
        stacks, samples = t["stackTable"], t["samples"]
        weight = samples.get("weight") or [1] * samples["length"]

        for s_idx, w in zip(samples["stack"], weight):
            if s_idx is None:
                continue
            leaf = stacks["frame"][s_idx]
            func_idx = ft["func"][leaf]
            res_idx = funcs["resource"][func_idx]
            lib_idx = rt["lib"][res_idx] if res_idx is not None and res_idx >= 0 else None
            addr = ft["address"][leaf]
            if lib_idx == main_lib_idx and addr >= 0:
                addr_self[addr] += w
            else:
                other_lib_total += w

    addrs = list(addr_self.keys())
    print(f"resolving {len(addrs)} unique addrs...", file=sys.stderr)
    resolved = addr2line_chain(BIN, addrs)

    by_phase = collections.Counter()
    for a, w in addr_self.items():
        chain = resolved.get(a, [])
        phase = classify(chain, markers)
        by_phase[phase] += w

    total = sum(by_phase.values()) + other_lib_total
    print(f"\n=== {prof_path}  ({impl_name})  total={total} ===")
    print("phase は inline チェーンに含まれる '一意マーカー行' で分類。")
    print("OTHER は HashMap 系 (hashbrown / siphash / Option::unwrap leaf 等)。\n")
    print(f"{'phase':<14} {'self':>8} {'self%':>7}")
    print("-" * 32)
    for phase, w in by_phase.most_common():
        print(f"{phase:<14} {w:>8} {w*100/total:>6.2f}%")
    print(f"{'(other-lib)':<14} {other_lib_total:>8} {other_lib_total*100/total:>6.2f}%")
    return by_phase, total

def main():
    if len(sys.argv) >= 3:
        impl = sys.argv[1]
        path = sys.argv[2]
        analyze(path, impl)
    else:
        for impl, path in [
            ("orig", "profiles/orig_skew1_cap10000.json"),
            ("v3",   "profiles/v3_skew1_cap10000.json"),
        ]:
            analyze(path, impl)

if __name__ == "__main__":
    main()
