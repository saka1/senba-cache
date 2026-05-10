# lru-vs-minimoka-vs-senba pareto

単スレ pareto: `lru::LruCache` (LRU) / `mini_moka::unsync::Cache` (W-TinyLFU) / `senba::Cache` (SIEVE) を ARC paper trace P1..P14 + Zipf 合成で並べる。
最新分析は [`docs/reports/2026-05-11-lru-vs-minimoka-vs-senba-pareto.md`](../../reports/2026-05-11-lru-vs-minimoka-vs-senba-pareto.md) を参照。

このディレクトリは**最新スナップショット**の置き場で、過去版は git history が持つ (上書き運用)。

## 構成

- [`run.sh`](run.sh) — release build → ARC + Zipf を流して `data/{arc,zipf}.csv` を上書き、`plot.py` まで一括
- [`plot.py`](plot.py) — `data/*.csv` から `figures/*.png` を再生成 (uv project は `scripts/`)
- `data/arc.csv` — preset (P1..P14) × variant × capacity
- `data/zipf.csv` — α∈{0.8,1.0,1.2} × variant × capacity sweep {1k,4k,16k,64k,256k}
- `figures/pareto-grid.png` — 全 trace を 1 figure に統合した Pareto grid (ns/op vs HR)

## 再現

```bash
docs/benchmark/lru-vs-minimoka-vs-senba-pareto/run.sh
```

事前: `git submodule update --init external/mokabench` (ARC zst trace の取得)。
