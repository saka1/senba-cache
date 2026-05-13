# concurrent-full-sweep — senba::concurrent (v0.4.0) vs c17s vs moka vs mini_moka

`<variant>_pct` = (variant / c17s − 1) × 100, computed per (source, workload, value, op_mix, skew, threads) cell. Positive ⇒ variant is faster than c17s.

## Rollup — median / worst Δ% by (source, value)

### source=zipf, value=string

- **senba_concurrent**: median **-10.0%**, worst **-25.5%**
- **moka**: median **-81.8%**, worst **-91.0%**
- **mini_moka**: median **-81.7%**, worst **-93.7%**

### source=zipf, value=u64

- **senba_concurrent**: median **-1.3%**, worst **-6.9%**
- **moka**: median **-89.4%**, worst **-96.5%**
- **mini_moka**: median **-90.2%**, worst **-96.0%**

## Migration accept criterion

Goal: `senba_concurrent` (v0.4.0, r4-based) Pareto-dominates the prior lift (c17s-equivalent). Tolerance: median ≥ −5% on V=u64, worst ≥ −10% (within perf-gate noise); median ≥ +5% on V=String (the r4 design goal).

- **V=string**: median **-10.0%**, worst **-25.5%** → FAIL
- **V=u64**: median **-1.3%**, worst **-6.9%** → PASS

## Per-cell table

| source | workload_param | value | op_mix | skew | threads | c17s | senba_concurrent_pct | moka_pct | mini_moka_pct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| zipf | zipf | string | gim | 0.8 | 1 | 7.77 | -25.5 | -78.2 | -72.4 |
| zipf | zipf | string | gim | 0.8 | 4 | 14.17 | -21.6 | -83.4 | -72.6 |
| zipf | zipf | string | gim | 0.8 | 8 | 11.2 | 38.5 | -80.1 | -63.9 |
| zipf | zipf | string | gim | 0.8 | 16 | 10.3 | 121.9 | -80.6 | -63.9 |
| zipf | zipf | string | gim | 1.0 | 1 | 10.33 | -19.2 | -74.9 | -73.9 |
| zipf | zipf | string | gim | 1.0 | 4 | 22.65 | -20.3 | -81.5 | -76.2 |
| zipf | zipf | string | gim | 1.0 | 8 | 21.4 | 20.2 | -80.7 | -69.6 |
| zipf | zipf | string | gim | 1.0 | 16 | 18.4 | 104.6 | -80.2 | -67.3 |
| zipf | zipf | string | gim | 1.4 | 1 | 17.6 | -9.9 | -66.6 | -79.1 |
| zipf | zipf | string | gim | 1.4 | 4 | 59.05 | -15.6 | -86.7 | -89.6 |
| zipf | zipf | string | gim | 1.4 | 8 | 80.67 | -6.4 | -89.5 | -91.6 |
| zipf | zipf | string | gim | 1.4 | 16 | 124.56 | -15.8 | -91.0 | -90.5 |
| zipf | zipf | string | read-heavy | 0.8 | 1 | 13.36 | -7.3 | -67.1 | -72.2 |
| zipf | zipf | string | read-heavy | 0.8 | 4 | 43.88 | -11.0 | -83.8 | -85.7 |
| zipf | zipf | string | read-heavy | 0.8 | 8 | 59.39 | -5.2 | -85.0 | -87.9 |
| zipf | zipf | string | read-heavy | 0.8 | 16 | 66.28 | 3.9 | -78.7 | -87.2 |
| zipf | zipf | string | read-heavy | 1.0 | 1 | 13.24 | -13.2 | -65.3 | -72.3 |
| zipf | zipf | string | read-heavy | 1.0 | 4 | 41.01 | -10.1 | -82.3 | -84.2 |
| zipf | zipf | string | read-heavy | 1.0 | 8 | 57.27 | 0.4 | -85.3 | -87.4 |
| zipf | zipf | string | read-heavy | 1.0 | 16 | 67.31 | 16.3 | -82.0 | -87.6 |
| zipf | zipf | string | read-heavy | 1.4 | 1 | 16.81 | -14.8 | -67.1 | -77.7 |
| zipf | zipf | string | read-heavy | 1.4 | 4 | 51.53 | -9.6 | -85.1 | -88.6 |
| zipf | zipf | string | read-heavy | 1.4 | 8 | 73.65 | -10.2 | -88.4 | -91.2 |
| zipf | zipf | string | read-heavy | 1.4 | 16 | 106.86 | -18.4 | -90.7 | -93.7 |
| zipf | zipf | u64 | gim | 0.8 | 1 | 10.72 | -1.2 | -81.9 | -78.5 |
| zipf | zipf | u64 | gim | 0.8 | 4 | 29.45 | 2.3 | -91.3 | -84.4 |
| zipf | zipf | u64 | gim | 0.8 | 8 | 45.4 | 0.4 | -94.8 | -90.4 |
| zipf | zipf | u64 | gim | 0.8 | 16 | 60.11 | 0.3 | -96.5 | -93.6 |
| zipf | zipf | u64 | gim | 1.0 | 1 | 15.22 | -1.9 | -81.1 | -80.7 |
| zipf | zipf | u64 | gim | 1.0 | 4 | 41.67 | -1.5 | -88.8 | -83.5 |
| zipf | zipf | u64 | gim | 1.0 | 8 | 58.76 | 7.9 | -92.5 | -88.6 |
| zipf | zipf | u64 | gim | 1.0 | 16 | 85.2 | 5.1 | -95.5 | -92.7 |
| zipf | zipf | u64 | gim | 1.4 | 1 | 25.18 | -1.3 | -75.3 | -83.3 |
| zipf | zipf | u64 | gim | 1.4 | 4 | 84.6 | -2.2 | -90.8 | -92.8 |
| zipf | zipf | u64 | gim | 1.4 | 8 | 108.62 | 11.0 | -92.5 | -94.1 |
| zipf | zipf | u64 | gim | 1.4 | 16 | 179.3 | -5.3 | -94.1 | -94.1 |
| zipf | zipf | u64 | read-heavy | 0.8 | 1 | 16.13 | 1.5 | -71.3 | -75.6 |
| zipf | zipf | u64 | read-heavy | 0.8 | 4 | 54.39 | -1.5 | -85.7 | -87.7 |
| zipf | zipf | u64 | read-heavy | 0.8 | 8 | 69.76 | 12.2 | -86.5 | -90.1 |
| zipf | zipf | u64 | read-heavy | 0.8 | 16 | 104.95 | -6.9 | -86.6 | -92.4 |
| zipf | zipf | u64 | read-heavy | 1.0 | 1 | 17.51 | 0.4 | -71.4 | -77.4 |
| zipf | zipf | u64 | read-heavy | 1.0 | 4 | 59.2 | -4.3 | -87.5 | -89.1 |
| zipf | zipf | u64 | read-heavy | 1.0 | 8 | 81.81 | -1.8 | -89.6 | -91.9 |
| zipf | zipf | u64 | read-heavy | 1.0 | 16 | 119.65 | -1.3 | -89.2 | -94.1 |
| zipf | zipf | u64 | read-heavy | 1.4 | 1 | 24.96 | -3.6 | -75.3 | -82.9 |
| zipf | zipf | u64 | read-heavy | 1.4 | 4 | 79.71 | -1.7 | -89.9 | -92.5 |
| zipf | zipf | u64 | read-heavy | 1.4 | 8 | 108.2 | -0.6 | -92.4 | -94.5 |
| zipf | zipf | u64 | read-heavy | 1.4 | 16 | 154.04 | -4.4 | -93.8 | -96.0 |
