# README headline — summary

AWS c8i.2xlarge (Granite Rapids), 4 physical cores + SMT. Threads pinned to cpus 0..T-1 (one per physical core; SMT siblings 4-7 unused). Zipf α=1.0, cap=4096, keys=100k, read-heavy, value=u64. 3 trials per cell, 2M ops + 200k warmup each.

| T | senba Mops | moka Mops | mini Mops | senba/moka | senba/mini | senba hit | moka hit | mini hit | senba p99 ns | moka p99 ns | mini p99 ns |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 14.76 | 4.31 | 3.51 | 3.42x | 4.21x | 0.6577 | 0.6863 | 0.6864 | 84.0 | 258.0 | 316.0 |
| 2 | 22.02 | 3.32 | 2.11 | 6.63x | 10.45x | 0.6583 | 0.6865 | 0.6863 | 119.0 | 1558.0 | 1007.0 |
| 3 | 32.19 | 3.55 | 2.48 | 9.06x | 12.98x | 0.6584 | 0.6849 | 0.6861 | 109.0 | 1735.0 | 1302.0 |
| 4 | 40.42 | 3.62 | 2.62 | 11.16x | 15.42x | 0.6584 | 0.6852 | 0.6861 | 125.0 | 1922.0 | 1656.0 |

## Scaling 1T → 4T

- senba::concurrent : 14.76 → 40.42 Mops (2.74x)
- moka              : 4.31 → 3.62 Mops (0.84x)
- mini-moka         : 3.51 → 2.62 Mops (0.75x)
