#!/usr/bin/env python3
"""samply JSON のサンプルを (file:line) 単位で集計。

- 各サンプルの leaf address を addr2line でソース行に解決
- sieve_v0.rs / sieve_orig.rs / その他で集計
- 同じケースで orig vs v0 を並べる
"""
import json, sys, subprocess, collections, shutil, os

BIN = "target/release/deps/micro-d82976a2858802c6"
TARGETS = ["sieve_v0.rs", "sieve_v1.rs", "sieve_v2.rs", "sieve_v3.rs", "sieve_orig.rs"]

def addr2line_batch(binary, addrs):
    """{addr: [(fn, file, line), ...]}  inline 階層も含む。"""
    if not addrs:
        return {}
    args = ["addr2line", "-e", binary, "-f", "-C", "-i"] + [f"0x{a:x}" for a in addrs]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    # 出力は addr ごとに「fn\nfile:line」のペアが並ぶ。-i ありだと複数組続く。
    # 区切りが無いのでアドレス順に切る必要がある: 各 addr に対し最低1組、
    # その後 inline 階層が任意数続いて、次の addr の出力が始まる。
    # → 一度に走らせると区切りが曖昧になるため 1 addr ずつ。
    res = {}
    for a in addrs:
        out1 = subprocess.run(
            ["addr2line", "-e", binary, "-f", "-C", "-i", f"0x{a:x}"],
            capture_output=True, text=True, check=True
        ).stdout.splitlines()
        chain = []
        for i in range(0, len(out1), 2):
            if i + 1 >= len(out1):
                break
            fn = out1[i].strip()
            loc = out1[i+1].strip()
            if ":" in loc:
                f, ln = loc.rsplit(":", 1)
                try:
                    ln = int(ln) if ln.isdigit() else 0
                except ValueError:
                    ln = 0
            else:
                f, ln = loc, 0
            chain.append((fn, f, ln))
        res[a] = chain
    return res

def categorize(path):
    base = os.path.basename(path)
    if base in TARGETS:
        return base
    if "rustlib/src/rust" in path:
        return "[std/core]"
    if "hashbrown" in path:
        return "[hashbrown]"
    return "[other]"

def analyze(prof_path, binary, label):
    p = json.load(open(prof_path))
    main_lib_idx = next((i for i, L in enumerate(p["libs"]) if binary.endswith(L["name"])), None)

    addr_self = collections.Counter()
    other_self = collections.Counter()
    total = 0
    for t in p["threads"]:
        if t["samples"]["length"] == 0:
            continue
        ft = t["frameTable"]
        funcs = t["funcTable"]
        rt = t["resourceTable"]
        stacks = t["stackTable"]
        samples = t["samples"]
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
                other_self[t["name"]] += w
            total += w

    addrs = list(addr_self.keys())
    print(f"resolving {len(addrs)} unique addrs...", file=sys.stderr)
    resolved = addr2line_batch(binary, addrs)

    by_cat = collections.Counter()       # innermost のカテゴリ別
    by_inner_line = collections.Counter()  # innermost frame の (cat, file, line)
    by_outer_line = collections.Counter()  # outermost frame の (cat, file, line)
    # 自ソースを通る inline 階層に何回現れたか (= 累積で何 % が触れたか)
    self_in_chain = collections.Counter()

    for a, w in addr_self.items():
        chain = resolved.get(a, [])
        if not chain:
            by_cat["[unresolved]"] += w
            continue
        innermost = chain[0]
        outermost = chain[-1]
        in_cat = categorize(innermost[1])
        out_cat = categorize(outermost[1])
        by_cat[in_cat] += w
        by_inner_line[(in_cat, os.path.basename(innermost[1]), innermost[2])] += w
        by_outer_line[(out_cat, os.path.basename(outermost[1]), outermost[2])] += w
        # inline 階層に自ソース行が含まれていれば、その行に self time を加算
        seen_self = set()
        for fn, f, ln in chain:
            base = os.path.basename(f)
            if base in TARGETS and (base, ln) not in seen_self:
                self_in_chain[(base, ln)] += w
                seen_self.add((base, ln))

    print(f"\n=== {label}  total={total} samples ===")
    print("カテゴリ別 self time:")
    for cat, w in by_cat.most_common():
        print(f"  {w*100/total:>6.2f}%  {w:>6}  {cat}")

    print("\n自実装ファイルが inline 階層に登場する行 (= その行を実行中):")
    print(f"{'self%':>6} {'self':>6}  file:line")
    for (fn, ln), w in self_in_chain.most_common(20):
        print(f"{w*100/total:>6.2f} {w:>6}  {fn}:{ln}")

    print("\nleaf (innermost) hot lines:")
    print(f"{'self%':>6} {'self':>6}  cat / file:line")
    for (cat, fn, ln), w in by_inner_line.most_common(15):
        print(f"{w*100/total:>6.2f} {w:>6}  {cat} {fn}:{ln}")

    return total, by_cat, dict(self_in_chain)

def main():
    # CLI: scripts/samply_lines.py [profile.json[:label] ...]
    # Default: compare orig/v1/v2/v3 at insert_only/skew1/cap10000.
    if len(sys.argv) > 1:
        cases = []
        for arg in sys.argv[1:]:
            if ":" in arg:
                p, label = arg.split(":", 1)
            else:
                p = arg
                label = os.path.basename(arg).replace(".json", "")
            cases.append((p, label))
    else:
        cases = [
            ("profiles/orig_skew1_cap10000.json", "orig (skew1/cap10000)"),
            ("profiles/v1_skew1_cap10000.json",   "v1   (skew1/cap10000)"),
            ("profiles/v2_skew1_cap10000.json",   "v2   (skew1/cap10000)"),
            ("profiles/v3_skew1_cap10000.json",   "v3   (skew1/cap10000)"),
        ]
    results = []
    for path, label in cases:
        results.append((label, *analyze(path, BIN, label)))

    # 並列比較表
    print("\n\n=== カテゴリ別の正規化比較 ===")
    all_cats = set()
    for _, _, by_cat, _ in results:
        all_cats.update(by_cat.keys())
    print(f"{'category':<14}", end="")
    for label, total, _, _ in results:
        print(f"{label:>40}", end="")
    print()
    for cat in sorted(all_cats):
        print(f"{cat:<14}", end="")
        for label, total, by_cat, _ in results:
            pct = by_cat[cat] * 100 / total if total else 0
            print(f"{pct:>40.2f}%", end="")
        print()

if __name__ == "__main__":
    main()
