# README headline — summary

AWS c8i.2xlarge (Granite Rapids), 4 physical cores + SMT. Threads pinned to cpus 0..T-1 (one per physical core; SMT siblings 4-7 unused). Zipf α=1.0, cap=4096, keys=100k, read-heavy, value=u64. 3 trials per cell, 2M ops + 200k warmup each.

| T | senba Mops | moka Mops | mini Mops | senba/moka | senba/mini | senba hit | moka hit | mini hit | senba p99 ns | moka p99 ns | mini p99 ns |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 15.13 | 4.37 | 3.55 | 3.46x | 4.26x | 0.6572 | 0.685 | 0.6851 | 75.0 | 250.0 | 303.0 |
| 2 | 22.91 | 3.36 | 2.1 | 6.81x | 10.90x | 0.6583 | 0.686 | 0.6861 | 99.0 | 1081.0 | 1003.0 |
| 4 | 42.15 | 3.9 | 2.78 | 10.82x | 15.19x | 0.6584 | 0.6842 | 0.6853 | 111.0 | 1930.0 | 1555.0 |

## Scaling 1T → 4T

- senba::concurrent : 15.13 → 42.15 Mops (2.79x)
- moka              : 4.37 → 3.90 Mops (0.89x)
- mini-moka         : 3.55 → 2.78 Mops (0.78x)

## Single-thread (1 core, taskset -c 0)

senba::Cache vs mini-moka (unsync) vs lru-rs. Zipf α=1.0, cap=4096, 100k keys, 2M ops, value=u64, 3 trials.

| variant | Mops | hit ratio | senba ratio |
| --- | --- | --- | --- |
| senba::Cache       | 39.68 | 0.7131 | 1.00x |
| mini-moka (unsync) | 18.23 | 0.7111 | 2.18x |
| lru-rs             | 38.73 | 0.6449 | 1.02x |
