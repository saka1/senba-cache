# partitioned sweep summary

## 採用領域 (accept zone)

基準: HR drop ≤ 5.0pp **AND** Mops gain ≥ +20.0% (vs c17s)

| variant | total cells | accept cells | accept rate |
|---|---|---|---|
| partitioned | 225 | 17 | 7.6% |
| r1 | 135 | 10 | 7.4% |

## 鍵となる contrast cells (設計書 §sweep)

| note | workload | T | N|w | Mops | c17s_Mops | Mops_gain_% | HR_drop_pp |
|---|---|---|---|---|---|---|---|
| uncontended ceiling | zipf_s1.4_read-heavy | 16 | 16 | 73.040 | 155.210 | -52.900 | 9.920 |
| degenerate (1 mutex) | zipf_s1.4_read-heavy | 16 | 1 | 2.590 | 155.210 | -98.300 | -0.290 |
| T<N surplus | zipf_s1.4_read-heavy | 4 | 16 | 25.800 | 76.700 | -66.400 | 9.970 |
| HR-sensitive | arc_OLTP_cap1000 | 16 | 16 | 50.660 | 41.050 | 23.400 | 25.860 |
| HR-tolerant | twitter_cluster019 | 16 | 16 | 51.230 | 27.140 | 88.800 | 10.470 |

## variant 別 best cell (Mops gain top-1 per workload/value)

### partitioned

| workload | value | threads | variant | axis | hr_drop_pp | mops_gain_pct | mops | mops_c17s |
|---|---|---|---|---|---|---|---|---|
| twitter_cluster019 | u64 | 1 | partitioned | 2 | 3.270 | 250.566 | 26.336 | 7.513 |
| twitter_cluster034 | u64 | 1 | partitioned | 1 | -0.240 | 138.183 | 26.184 | 10.993 |
| twitter_cluster006 | u64 | 1 | partitioned | 4 | 8.770 | 121.359 | 24.600 | 11.113 |
| arc_DS1_cap1000 | u64 | 16 | partitioned | 16 | 0.010 | 117.706 | 43.381 | 19.926 |
| arc_OLTP_cap1000 | u64 | 1 | partitioned | 1 | 0.290 | 97.866 | 26.534 | 13.410 |
| zipf_s0.8_gim | u64 | 1 | partitioned | 16 | 16.670 | 32.661 | 13.137 | 9.903 |
| zipf_s1.0_gim | u64 | 1 | partitioned | 16 | 21.660 | 2.065 | 13.709 | 13.432 |
| zipf_s1.4_read-heavy | u64 | 1 | partitioned | 1 | -0.300 | -3.751 | 23.953 | 24.886 |
| zipf_s1.4_gim | u64 | 1 | partitioned | 1 | -0.290 | -4.535 | 23.531 | 24.648 |

### r1

| workload | value | threads | variant | axis | hr_drop_pp | mops_gain_pct | mops | mops_c17s |
|---|---|---|---|---|---|---|---|---|
| twitter_cluster019 | u64 | 16 | r1 | 16 | 4.770 | 183.199 | 76.862 | 27.140 |
| arc_OLTP_cap1000 | u64 | 16 | r1 | 16 | 23.850 | 118.837 | 89.839 | 41.053 |
| twitter_cluster034 | u64 | 16 | r1 | 16 | 14.010 | 111.014 | 75.103 | 35.592 |
| twitter_cluster006 | u64 | 16 | r1 | 16 | 10.410 | 104.214 | 74.616 | 36.538 |
| arc_DS1_cap1000 | u64 | 16 | r1 | 16 | 0.010 | 93.451 | 38.548 | 19.926 |
| zipf_s0.8_gim | u64 | 16 | r1 | 16 | 17.280 | 92.580 | 72.276 | 37.531 |
| zipf_s1.0_gim | u64 | 16 | r1 | 16 | 22.660 | 52.151 | 78.561 | 51.634 |
| zipf_s1.4_read-heavy | u64 | 16 | r1 | 16 | 10.780 | 28.289 | 199.112 | 155.206 |
| zipf_s1.4_gim | u64 | 4 | r1 | 4 | 4.160 | 7.641 | 81.470 | 75.687 |

