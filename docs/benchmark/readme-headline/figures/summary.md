# README headline — summary

AWS c8i.2xlarge (Granite Rapids), 4 physical cores + SMT. Threads pinned to cpus 0..T-1 (one per physical core; SMT siblings 4-7 unused). Zipf α=1.0, cap=4096, keys=100k, read-heavy, value=u64. 3 trials per cell, 2.4M ops + 240k warmup each.

| T | senba Mops | moka Mops | mini Mops | senba/moka | senba/mini | senba hit | moka hit | mini hit | senba p99 ns | moka p99 ns | mini p99 ns |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 14.99 | 4.35 | 3.55 | 3.44x | 4.22x | 0.6577 | 0.6863 | 0.6864 | 77.0 | 254.0 | 305.0 |
| 2 | 22.92 | 3.32 | 2.09 | 6.90x | 10.96x | 0.658 | 0.6863 | 0.6865 | 101.0 | 1134.0 | 1000.0 |
| 3 | 32.65 | 3.57 | 2.67 | 9.13x | 12.24x | 0.6585 | 0.6857 | 0.6855 | 109.0 | 1656.0 | 1194.0 |
| 4 | 42.01 | 3.89 | 2.86 | 10.79x | 14.68x | 0.6586 | 0.6846 | 0.6864 | 113.0 | 2000.0 | 1502.0 |

## Scaling 1T → 4T

- senba::concurrent : 14.99 → 42.01 Mops (2.80x)
- moka              : 4.35 → 3.89 Mops (0.89x)
- mini-moka         : 3.55 → 2.86 Mops (0.81x)

## Single-thread (1 core, taskset -c 0)

senba::Cache vs mini-moka (unsync) vs lru-rs. Zipf α=1.0, cap=4096, 100k keys, read-heavy (95% get / 5% insert), value=u64, 2.4M ops + 240k warmup, 3 trials.

| variant | Mops | hit ratio | senba ratio |
| --- | --- | --- | --- |
| senba::Cache       | 51.53 | 0.7174 | 1.00x |
| mini-moka (unsync) | 25.41 | 0.7226 | 2.03x |
| lru-rs             | 64.64 | 0.6982 | 0.80x |
