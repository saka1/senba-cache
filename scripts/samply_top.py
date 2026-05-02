#!/usr/bin/env python3
"""samply (Firefox Profiler 形式) JSON から self-time top-N を出す。

frameTable.address はバイナリ相対オフセット (string array では '0x...' の hex 文字列)。
binutils の addr2line で関数名 + ファイル:行 に解決する。
"""
import json, sys, subprocess, collections, shutil

def resolve_addresses(binary_path, addrs):
    """addr2line で複数アドレスを一括解決。{addr: (func, file:line)}"""
    if not addrs:
        return {}
    if not shutil.which("addr2line"):
        return {a: ("?", "?") for a in addrs}
    args = ["addr2line", "-e", binary_path, "-f", "-C", "-i", "-p"] + [f"0x{a:x}" for a in addrs]
    try:
        out = subprocess.run(args, capture_output=True, text=True, check=True).stdout.splitlines()
    except subprocess.CalledProcessError:
        return {a: ("?", "?") for a in addrs}
    res = {}
    # -i (inlines) means each addr can produce multiple lines starting with 'fn at file:line\n   (inlined by) ...'
    # but with -p, lines are formatted as 'fn at file:line' or '... (inlined by) fn at file:line'
    # We map line-by-line in input order; subsequent inlined lines start with '(inlined by)'.
    i = 0
    for line in out:
        line = line.strip()
        if not line:
            continue
        if line.startswith("(inlined by)"):
            continue  # 主呼び出しだけ採る (leaf にしたいなら逆)
        # parse 'name at file:line'
        if " at " in line:
            fn, loc = line.split(" at ", 1)
        else:
            fn, loc = line, "?"
        res[addrs[i]] = (fn.strip(), loc.strip())
        i += 1
        if i >= len(addrs):
            break
    return res

def analyze(path, binary_path, n=25):
    p = json.load(open(path))
    main_lib_idx = None
    for i, lib in enumerate(p["libs"]):
        if binary_path.endswith(lib["name"]) or lib["path"] == binary_path:
            main_lib_idx = i
            break
    if main_lib_idx is None:
        print(f"warn: binary {binary_path} not in libs", file=sys.stderr)

    for ti, t in enumerate(p["threads"]):
        if t["samples"]["length"] == 0:
            continue
        ft = t["frameTable"]
        funcs = t["funcTable"]
        rt = t["resourceTable"]
        strs = t["stringArray"]
        stacks = t["stackTable"]
        samples = t["samples"]

        # frame -> (lib_index_or_None, address)
        frame_meta = []
        for fi in range(ft["length"]):
            func_idx = ft["func"][fi]
            res_idx = funcs["resource"][func_idx]
            lib_idx = rt["lib"][res_idx] if res_idx is not None and res_idx >= 0 else None
            addr = ft["address"][fi]
            frame_meta.append((lib_idx, addr, strs[funcs["name"][func_idx]]))

        # leaf-only self time, restricted to the main binary
        weight = samples.get("weight") or [1] * samples["length"]
        self_t = collections.Counter()
        for s_idx, w in zip(samples["stack"], weight):
            if s_idx is None:
                continue
            leaf_frame = stacks["frame"][s_idx]
            lib_idx, addr, raw_name = frame_meta[leaf_frame]
            if lib_idx == main_lib_idx and addr >= 0:
                self_t[addr] += w
            else:
                self_t[("__other__", raw_name)] += w

        total = sum(self_t.values())
        print(f"\n=== {path}  thread[{ti}] {t['name']!r}  total={total} ===")

        # Resolve top addresses
        top_items = self_t.most_common(n * 2)  # take extras since some might be __other__
        addrs_to_resolve = [a for a, _ in top_items if isinstance(a, int)]
        resolved = resolve_addresses(binary_path, addrs_to_resolve)

        print(f"{'self%':>6} {'self':>8}  symbol  (file:line)")
        shown = 0
        for key, w in top_items:
            if shown >= n:
                break
            if isinstance(key, int):
                fn, loc = resolved.get(key, ("?", "?"))
                fn_short = fn if len(fn) <= 100 else fn[:97] + "..."
                print(f"{w*100/total:>6.2f} {w:>8}  {fn_short}\n                 {loc}")
            else:
                _, raw = key
                print(f"{w*100/total:>6.2f} {w:>8}  [other lib] {raw}")
            shown += 1

if __name__ == "__main__":
    binary = "target/release/deps/micro-d82976a2858802c6"
    if len(sys.argv) > 1:
        binary = sys.argv[1]
    files = sys.argv[2:] or ["profiles/v0_worst.json", "profiles/orig_worst.json"]
    for f in files:
        print(f"\n########## {f} ##########")
        analyze(f, binary, n=20)
