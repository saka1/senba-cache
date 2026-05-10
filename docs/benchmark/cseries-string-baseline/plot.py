"""c-series baseline sweep 結果 (`data/sweep.csv`) を読み込み、
markdown 要約 (`summary.md`) と比較図 (`figures/aggregate_mops.png`) を出力する。

要約は (value, op_mix, threads) を行、variant を列とした
aggregate_mops の median テーブル。c14s/c16s/c17s の相対差が読めれば十分なので、
詳細統計 (CI / p99 latency / hit ratio) は CSV を直接見る前提で省く。
"""
from __future__ import annotations

import csv
from pathlib import Path
from statistics import median

import matplotlib.pyplot as plt

HERE = Path(__file__).parent
DATA = HERE / "data" / "sweep.csv"
SUMMARY = HERE / "summary.md"
FIG_DIR = HERE / "figures"
FIG_DIR.mkdir(parents=True, exist_ok=True)

VARIANTS = ["c14s", "c16s", "c17s"]
VALUES = ["u64", "string"]
OP_MIXES = ["gim", "read-heavy"]
THREADS = [4, 8, 16]


def load_rows() -> list[dict]:
    with DATA.open() as f:
        return list(csv.DictReader(f))


def median_mops(
    rows: list[dict], variant: str, value: str, op_mix: str, threads: int
) -> float | None:
    matching = [
        float(r["aggregate_mops"])
        for r in rows
        if r["variant"] == variant
        and r["value"] == value
        and r["op_mix"] == op_mix
        and int(r["threads"]) == threads
    ]
    return median(matching) if matching else None


def median_hr(
    rows: list[dict], variant: str, value: str, op_mix: str, threads: int
) -> float | None:
    matching = [
        float(r["hit_ratio"])
        for r in rows
        if r["variant"] == variant
        and r["value"] == value
        and r["op_mix"] == op_mix
        and int(r["threads"]) == threads
    ]
    return median(matching) if matching else None


def emit_summary(rows: list[dict]) -> None:
    lines: list[str] = []
    lines.append("# c-series baseline sweep summary\n")
    lines.append(
        "`aggregate_mops` (median of 3 trials), c14s vs c16s vs c17s。"
        "c17s の Δ% は c16s 比 (直近 baseline)。HR は median (Δ vs c16s)。\n"
    )

    for value in VALUES:
        for op_mix in OP_MIXES:
            skew = "1.0" if op_mix == "gim" else "1.4"
            lines.append(
                f"\n## value=`{value}`, op-mix=`{op_mix}` (skew={skew})\n"
            )
            lines.append("| T | c14s Mops | c16s Mops | c17s Mops | c17s Δ% vs c16s | c16s HR | c17s HR |")
            lines.append("| ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
            for t in THREADS:
                m14 = median_mops(rows, "c14s", value, op_mix, t)
                m16 = median_mops(rows, "c16s", value, op_mix, t)
                m17 = median_mops(rows, "c17s", value, op_mix, t)
                hr16 = median_hr(rows, "c16s", value, op_mix, t)
                hr17 = median_hr(rows, "c17s", value, op_mix, t)

                def fmt_mops(x: float | None) -> str:
                    return f"{x:.2f}" if x is not None else "✗"

                def fmt_hr(x: float | None) -> str:
                    return f"{x:.4f}" if x is not None else "✗"

                if m16 is not None and m17 is not None:
                    delta_str = f"{(m17 - m16) / m16 * 100.0:+.1f}%"
                else:
                    delta_str = "—"
                lines.append(
                    f"| {t} | {fmt_mops(m14)} | {fmt_mops(m16)} | "
                    f"{fmt_mops(m17)} | {delta_str} | "
                    f"{fmt_hr(hr16)} | {fmt_hr(hr17)} |"
                )
    lines.append("")
    lines.append(
        "`✗` = crash (memory corruption — c14s/c16s seqlock-via-tag racing "
        "window で `ManuallyDrop<String>` の半上書き header を drop して "
        "tcache free が壊れる)。data/crashes.log を参照。"
    )

    SUMMARY.write_text("\n".join(lines) + "\n")
    print(f"wrote {SUMMARY}")


def plot_aggregate(rows: list[dict]) -> None:
    fig, axes = plt.subplots(2, 2, figsize=(11, 7), sharey=False)
    width = 0.25
    x = list(range(len(THREADS)))

    for i, value in enumerate(VALUES):
        for j, op_mix in enumerate(OP_MIXES):
            ax = axes[i][j]
            for k, variant in enumerate(VARIANTS):
                ys = [
                    median_mops(rows, variant, value, op_mix, t) or 0.0
                    for t in THREADS
                ]
                ax.bar(
                    [xi + (k - 1) * width for xi in x],
                    ys,
                    width=width,
                    label=variant,
                )
            skew = "1.0" if op_mix == "gim" else "1.4"
            ax.set_title(f"value={value}, op-mix={op_mix} (skew={skew})")
            ax.set_xticks(x)
            ax.set_xticklabels([f"T={t}" for t in THREADS])
            ax.set_ylabel("aggregate Mops/s (median)")
            ax.legend(loc="best", fontsize=8)
            ax.grid(axis="y", alpha=0.3)

    plt.tight_layout()
    out = FIG_DIR / "aggregate_mops.png"
    plt.savefig(out, dpi=120)
    print(f"wrote {out}")


def main() -> None:
    rows = load_rows()
    print(f"loaded {len(rows)} rows from {DATA}")
    emit_summary(rows)
    plot_aggregate(rows)


if __name__ == "__main__":
    main()
