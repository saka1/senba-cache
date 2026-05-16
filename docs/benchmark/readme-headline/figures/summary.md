# README headline — summary

AWS c8i.2xlarge (Granite Rapids), 4 physical cores + SMT. Threads pinned to cpus 0..T-1 (one per physical core; SMT siblings 4-7 unused). Zipf α=1.0, cap=4096, keys=100k, read-heavy, value=u64. 3 trials per cell, 2.4M ops + 240k warmup each.

| T | senba Mops | moka Mops | mini Mops | senba/moka | senba/mini | senba hit | moka hit | mini hit | senba p99 ns | moka p99 ns | mini p99 ns |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 14.93 | 4.36 | 3.55 | 3.43x | 4.20x | 0.6577 | 0.6863 | 0.6864 | 83.0 | 260.0 | 310.0 |
| 2 | 22.86 | 3.36 | 2.06 | 6.81x | 11.07x | 0.6585 | 0.6865 | 0.6862 | 104.0 | 1101.0 | 1026.0 |
| 3 | 32.31 | 3.65 | 2.49 | 8.84x | 12.96x | 0.6583 | 0.6857 | 0.6858 | 114.0 | 1740.0 | 1281.0 |
| 4 | 40.61 | 3.84 | 3.03 | 10.57x | 13.42x | 0.6584 | 0.6847 | 0.6858 | 123.0 | 1845.0 | 1419.0 |

## Scaling 1T → 4T

- senba::concurrent : 14.93 → 40.61 Mops (2.72x)
- moka              : 4.36 → 3.84 Mops (0.88x)
- mini-moka         : 3.55 → 3.03 Mops (0.85x)

## Single-thread (1 core, taskset -c 0)

senba::Cache vs mini-moka (unsync) vs lru-rs. Zipf α=1.0, cap=4096, 100k keys, 2M ops, value=u64, 3 trials.

| variant | Mops | hit ratio | senba ratio |
| --- | --- | --- | --- |
| senba::Cache       | 39.24 | 0.7131 | 1.00x |
| mini-moka (unsync) | 18.41 | 0.7111 | 2.13x |
| lru-rs             | 38.64 | 0.6449 | 1.02x |
