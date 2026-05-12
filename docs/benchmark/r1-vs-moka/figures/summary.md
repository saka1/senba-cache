# r1 vs moka sweep summary

variants: c17s (baseline @ ways=1) / r1@ways={1,8} / moka 0.12 sync / mini_moka 0.10 sync

observations: 400 cells aggregated from raw CSV

## T=16 Mops

| workload | value | c17s | r1@1 | r1@8 | moka | mini_moka | r1@8/moka |
|---|---|---|---|---|---|---|---|
| twitter_cluster006 | string | 29.45 | 30.12 | 28.09 | 1.84 | 3.50 | 15.27 |
| twitter_cluster019 | string | 11.02 | 11.03 | 18.38 | 1.53 | 2.98 | 12.01 |
| twitter_cluster034 | string | 16.48 | 16.18 | 24.88 | 1.76 | 3.59 | 14.14 |
| zipf_s0.8_gim | string | 11.32 | 11.38 | 32.41 | 2.07 | 3.72 | 15.66 |
| zipf_s1.0_gim | string | 20.08 | 20.87 | 44.25 | 3.85 | 6.07 | 11.49 |
| zipf_s1.4_gim | string | 101.96 | 123.85 | 111.74 | 12.95 | 11.21 | 8.63 |
| zipf_s1.4_read-heavy | string | 105.99 | 95.46 | 123.54 | 10.43 | 6.49 | 11.84 |
| arc_DS1_cap4000 | u64 | 7.49 | 9.49 | 9.68 | 1.34 | 2.75 | 7.22 |
| arc_OLTP_cap4000 | u64 | 26.68 | 24.97 | 40.44 | 2.09 | 4.29 | 19.35 |
| twitter_cluster006 | u64 | 29.89 | 29.01 | 31.95 | 1.89 | 3.76 | 16.90 |
| twitter_cluster019 | u64 | 12.05 | 12.04 | 20.20 | 1.59 | 3.16 | 12.70 |
| twitter_cluster034 | u64 | 16.74 | 17.29 | 21.06 | 1.88 | 3.76 | 11.20 |
| zipf_s0.8_gim | u64 | 32.81 | 30.82 | 46.33 | 2.14 | 3.96 | 21.65 |
| zipf_s1.0_gim | u64 | 54.92 | 56.25 | 64.12 | 3.93 | 6.40 | 16.32 |
| zipf_s1.4_gim | u64 | 193.05 | 147.22 | 150.74 | 10.54 | 11.37 | 14.30 |
| zipf_s1.4_read-heavy | u64 | 145.72 | 163.61 | 207.69 | 9.09 | 6.61 | 22.85 |

## T=16 p99 chunk latency (ns)

| workload | value | c17s | r1@1 | r1@8 | moka | mini_moka | moka/r1@8 |
|---|---|---|---|---|---|---|---|
| twitter_cluster006 | string | 940 | 954 | 1041 | 15231 | 8182 | 14.63 |
| twitter_cluster019 | string | 2053 | 2195 | 1473 | 17669 | 9581 | 12.00 |
| twitter_cluster034 | string | 1391 | 1551 | 1116 | 17044 | 7256 | 15.27 |
| zipf_s0.8_gim | string | 2221 | 2156 | 1226 | 14033 | 7713 | 11.45 |
| zipf_s1.0_gim | string | 1448 | 1553 | 891 | 8855 | 5432 | 9.94 |
| zipf_s1.4_gim | string | 252 | 184 | 221 | 3339 | 4755 | 15.11 |
| zipf_s1.4_read-heavy | string | 290 | 299 | 192 | 3498 | 6112 | 18.22 |
| arc_DS1_cap4000 | u64 | 2935 | 3220 | 2762 | 20203 | 10912 | 7.31 |
| arc_OLTP_cap4000 | u64 | 1017 | 1249 | 735 | 15503 | 6663 | 21.09 |
| twitter_cluster006 | u64 | 976 | 1193 | 1084 | 16038 | 8064 | 14.80 |
| twitter_cluster019 | u64 | 1854 | 1912 | 1271 | 17417 | 8410 | 13.70 |
| twitter_cluster034 | u64 | 1462 | 1387 | 2576 | 15912 | 7447 | 6.18 |
| zipf_s0.8_gim | u64 | 2020 | 2756 | 734 | 14365 | 8593 | 19.57 |
| zipf_s1.0_gim | u64 | 1364 | 1414 | 630 | 9873 | 6498 | 15.67 |
| zipf_s1.4_gim | u64 | 128 | 146 | 194 | 3313 | 5201 | 17.08 |
| zipf_s1.4_read-heavy | u64 | 134 | 186 | 129 | 5075 | 4688 | 39.34 |

## T=16 hit ratio

| workload | value | c17s | r1@1 | r1@8 | moka | mini_moka |
|---|---|---|---|---|---|---|
| twitter_cluster006 | string | 0.35 | 0.35 | 0.07 | 0.36 | 0.35 |
| twitter_cluster019 | string | 0.24 | 0.24 | 0.22 | 0.17 | 0.16 |
| twitter_cluster034 | string | 0.35 | 0.35 | 0.27 | 0.34 | 0.34 |
| zipf_s0.8_gim | string | 0.45 | 0.45 | 0.26 | 0.45 | 0.45 |
| zipf_s1.0_gim | string | 0.72 | 0.72 | 0.54 | 0.72 | 0.72 |
| zipf_s1.4_gim | string | 0.97 | 0.97 | 0.94 | 0.97 | 0.97 |
| zipf_s1.4_read-heavy | string | 0.93 | 0.93 | 0.89 | 0.92 | 0.92 |
| arc_DS1_cap4000 | u64 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| arc_OLTP_cap4000 | u64 | 0.44 | 0.44 | 0.23 | 0.45 | 0.45 |
| twitter_cluster006 | u64 | 0.35 | 0.35 | 0.07 | 0.36 | 0.35 |
| twitter_cluster019 | u64 | 0.24 | 0.24 | 0.22 | 0.17 | 0.16 |
| twitter_cluster034 | u64 | 0.35 | 0.35 | 0.27 | 0.34 | 0.34 |
| zipf_s0.8_gim | u64 | 0.45 | 0.45 | 0.26 | 0.45 | 0.45 |
| zipf_s1.0_gim | u64 | 0.72 | 0.72 | 0.54 | 0.72 | 0.72 |
| zipf_s1.4_gim | u64 | 0.97 | 0.97 | 0.94 | 0.97 | 0.97 |
| zipf_s1.4_read-heavy | u64 | 0.93 | 0.93 | 0.89 | 0.92 | 0.92 |

## r1@8 / moka 比 top-10 (どの cell で r1@8 が moka より勝つか)

| workload | value | threads | r1@8 | moka | r1@8/moka |
|---|---|---|---|---|---|
| zipf_s1.4_read-heavy | u64 | 16 | 207.69 | 9.09 | 22.85 |
| zipf_s0.8_gim | u64 | 16 | 46.33 | 2.14 | 21.60 |
| arc_OLTP_cap4000 | u64 | 16 | 40.44 | 2.09 | 19.36 |
| twitter_cluster006 | u64 | 16 | 31.95 | 1.89 | 16.88 |
| zipf_s0.8_gim | u64 | 8 | 38.46 | 2.32 | 16.56 |
| zipf_s1.0_gim | u64 | 16 | 64.12 | 3.93 | 16.30 |
| zipf_s0.8_gim | string | 16 | 32.41 | 2.07 | 15.63 |
| twitter_cluster006 | string | 16 | 28.09 | 1.84 | 15.28 |
| zipf_s1.4_gim | u64 | 16 | 150.74 | 10.54 | 14.30 |
| twitter_cluster034 | string | 16 | 24.88 | 1.76 | 14.13 |

## r1@8 / moka 比 bottom-10 (どの cell で r1@8 が moka に近い/負ける)

| workload | value | threads | r1@8 | moka | r1@8/moka |
|---|---|---|---|---|---|
| zipf_s1.0_gim | string | 1 | 7.32 | 2.69 | 2.72 |
| twitter_cluster019 | string | 2 | 4.69 | 1.73 | 2.72 |
| arc_OLTP_cap4000 | u64 | 1 | 6.36 | 2.36 | 2.70 |
| zipf_s1.4_gim | string | 1 | 15.44 | 5.86 | 2.63 |
| twitter_cluster006 | u64 | 1 | 4.85 | 2.04 | 2.38 |
| twitter_cluster006 | string | 1 | 4.25 | 1.82 | 2.34 |
| twitter_cluster034 | string | 1 | 3.70 | 1.82 | 2.03 |
| twitter_cluster034 | u64 | 1 | 4.07 | 2.07 | 1.97 |
| twitter_cluster019 | string | 1 | 2.48 | 1.55 | 1.60 |
| twitter_cluster019 | u64 | 1 | 2.66 | 1.76 | 1.51 |

