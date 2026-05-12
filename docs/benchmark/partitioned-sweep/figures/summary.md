# partitioned sweep summary

## 採用領域 (accept zone)

基準: HR drop ≤ 5.0pp **AND** Mops gain ≥ +20.0% (vs c17s)

| variant | total cells | accept cells | accept rate |
|---|---|---|---|
| partitioned | 225 | 44 | 19.6% |
| r1 | 135 | 8 | 5.9% |

## 鍵となる contrast cells (設計書 §sweep)

| note | workload | T | N|w | Mops | c17s_Mops | Mops_gain_% | HR_drop_pp |
|---|---|---|---|---|---|---|---|
| uncontended ceiling | zipf_s1.4_read-heavy | 16 | 16 | 75.410 | 133.100 | -43.300 | 5.980 |
| degenerate (1 mutex) | zipf_s1.4_read-heavy | 16 | 1 | 2.350 | 133.100 | -98.200 | -0.020 |
| T<N surplus | zipf_s1.4_read-heavy | 4 | 16 | 26.330 | 77.720 | -66.100 | 5.610 |
| HR-sensitive | arc_OLTP_cap4000 | 16 | 16 | 53.700 | 25.750 | 108.500 | 28.040 |
| HR-tolerant | twitter_cluster019 | 16 | 16 | 52.850 | 10.780 | 390.500 | 2.600 |

## variant 別 best cell (Mops gain top-1 per workload/value)

### partitioned

| workload | value | threads | variant | axis | hr_drop_pp | mops_gain_pct | mops | mops_c17s |
|---|---|---|---|---|---|---|---|---|
| twitter_cluster019 | u64 | 1 | partitioned | 1 | 0.000 | 1086.417 | 26.230 | 2.211 |
| twitter_cluster034 | u64 | 1 | partitioned | 1 | 0.000 | 582.524 | 25.933 | 3.800 |
| arc_DS1_cap4000 | u64 | 16 | partitioned | 16 | 0.240 | 383.130 | 45.667 | 9.452 |
| arc_OLTP_cap4000 | u64 | 1 | partitioned | 1 | 0.000 | 360.272 | 27.383 | 5.949 |
| twitter_cluster006 | u64 | 1 | partitioned | 1 | 0.000 | 326.566 | 25.240 | 5.917 |
| zipf_s0.8_gim | u64 | 1 | partitioned | 16 | 23.480 | 55.353 | 11.640 | 7.492 |
| zipf_s1.0_gim | u64 | 1 | partitioned | 1 | 0.000 | 36.562 | 15.297 | 11.201 |
| zipf_s1.4_read-heavy | u64 | 1 | partitioned | 2 | 0.750 | 2.782 | 23.992 | 23.343 |
| zipf_s1.4_gim | u64 | 1 | partitioned | 2 | 0.910 | 2.335 | 23.273 | 22.742 |

### r1

| workload | value | threads | variant | axis | hr_drop_pp | mops_gain_pct | mops | mops_c17s |
|---|---|---|---|---|---|---|---|---|
| twitter_cluster034 | u64 | 16 | r1 | 16 | 11.130 | 91.898 | 32.189 | 16.774 |
| arc_OLTP_cap4000 | u64 | 16 | r1 | 16 | 28.040 | 86.027 | 47.898 | 25.748 |
| twitter_cluster019 | u64 | 16 | r1 | 16 | 2.600 | 82.002 | 19.612 | 10.776 |
| zipf_s0.8_gim | u64 | 16 | r1 | 16 | 24.310 | 44.976 | 47.139 | 32.515 |
| twitter_cluster006 | u64 | 16 | r1 | 16 | 30.620 | 38.986 | 34.222 | 24.623 |
| zipf_s1.4_read-heavy | u64 | 16 | r1 | 16 | 5.980 | 29.349 | 172.159 | 133.096 |
| zipf_s1.0_gim | u64 | 16 | r1 | 16 | 23.280 | 19.097 | 62.779 | 52.713 |
| arc_DS1_cap4000 | u64 | 16 | r1 | 16 | 0.240 | 19.057 | 11.254 | 9.452 |
| zipf_s1.4_gim | u64 | 16 | r1 | 1 | 0.000 | 3.712 | 152.668 | 147.204 |

