# r1 sweep summary

- accept zone (HR drop ≤5.0pp, Mops gain ≥+20.0%) に乗った cell 数: **31** / 520

## accept zone cells (上位 20)

| workload | value | threads | ways | hr_drop_pp | mops_gain_pct | mops_r1 | mops_c17s |
|---|---|---|---|---|---|---|---|
| twitter_cluster019 | u64 | 16 | 8 | 2.310 | 77.685 | 20.273 | 11.410 |
| twitter_cluster019 | string | 16 | 8 | 1.880 | 66.861 | 18.953 | 11.359 |
| twitter_cluster019 | u64 | 8 | 8 | 0.600 | 40.860 | 13.530 | 9.605 |
| twitter_cluster019 | u64 | 8 | 4 | 0.460 | 37.899 | 13.245 | 9.605 |
| arc_DS1_cap4000 | u64 | 16 | 8 | 0.030 | 36.855 | 9.945 | 7.266 |
| twitter_cluster034 | u64 | 8 | 4 | 4.610 | 33.588 | 20.080 | 15.031 |
| twitter_cluster019 | string | 8 | 4 | 0.380 | 33.362 | 12.418 | 9.311 |
| twitter_cluster019 | string | 8 | 8 | 0.550 | 33.362 | 12.418 | 9.311 |
| arc_MergeP_cap4000 | u64 | 1 | 8 | 1.500 | 32.769 | 3.709 | 2.794 |
| twitter_cluster034 | string | 8 | 4 | 4.640 | 32.718 | 18.416 | 13.876 |
| zipf_s1.4_read-heavy | string | 16 | 8 | 3.750 | 31.784 | 121.882 | 92.487 |
| arc_MergeP_cap4000 | u64 | 8 | 4 | 0.460 | 31.216 | 17.423 | 13.278 |
| twitter_cluster019 | string | 4 | 8 | 2.780 | 30.966 | 8.964 | 6.845 |
| twitter_cluster034 | u64 | 16 | 4 | 4.650 | 30.705 | 21.495 | 16.446 |
| zipf_s1.4_read-heavy | string | 16 | 4 | 2.160 | 30.512 | 120.706 | 92.487 |
| twitter_cluster019 | u64 | 4 | 8 | 2.780 | 28.319 | 9.400 | 7.325 |
| arc_ConCat_cap4000 | u64 | 2 | 8 | 2.540 | 27.491 | 9.284 | 7.282 |
| zipf_s1.4_read-heavy | string | 16 | 2 | 0.930 | 27.327 | 117.760 | 92.487 |
| arc_P8_cap4000 | u64 | 8 | 2 | 1.750 | 25.065 | 19.472 | 15.569 |
| twitter_cluster019 | u64 | 16 | 4 | 1.460 | 24.956 | 14.257 | 11.410 |

## workload × value 別の best (T, ways) by Mops gain

| workload | value | threads | ways | mops_gain_pct | hr_drop_pp | mops_r1 | mops_c17s |
|---|---|---|---|---|---|---|---|
| zipf_s0.8_gim | string | 16 | 8 | 171.090 | 19.420 | 30.794 | 11.359 |
| zipf_s1.0_gim | string | 16 | 8 | 127.010 | 17.410 | 45.609 | 20.091 |
| twitter_cluster019 | u64 | 16 | 8 | 77.685 | 2.310 | 20.273 | 11.410 |
| twitter_cluster034 | u64 | 16 | 8 | 71.765 | 7.680 | 28.248 | 16.446 |
| twitter_cluster019 | string | 16 | 8 | 66.861 | 1.880 | 18.953 | 11.359 |
| twitter_cluster034 | string | 16 | 8 | 60.167 | 7.690 | 25.475 | 15.905 |
| arc_OLTP_cap4000 | u64 | 16 | 8 | 56.579 | 21.530 | 40.637 | 25.953 |
| twitter_cluster016 | string | 16 | 8 | 36.954 | 8.970 | 23.412 | 17.094 |
| arc_DS1_cap4000 | u64 | 16 | 8 | 36.855 | 0.030 | 9.945 | 7.266 |
| twitter_cluster016 | u64 | 16 | 8 | 35.039 | 9.140 | 23.784 | 17.613 |
| twitter_cluster018 | u64 | 16 | 8 | 34.281 | 15.890 | 37.333 | 27.802 |
| arc_MergeP_cap4000 | u64 | 1 | 8 | 32.769 | 1.500 | 3.709 | 2.794 |
| zipf_s1.4_read-heavy | string | 16 | 8 | 31.784 | 3.750 | 121.882 | 92.487 |
| arc_ConCat_cap4000 | u64 | 2 | 8 | 27.491 | 2.540 | 9.284 | 7.282 |
| zipf_s0.8_gim | u64 | 16 | 8 | 27.402 | 19.390 | 45.010 | 35.329 |
| arc_P8_cap4000 | u64 | 8 | 2 | 25.065 | 1.750 | 19.472 | 15.569 |
| twitter_cluster018 | string | 16 | 8 | 22.462 | 15.810 | 32.301 | 26.377 |
| arc_S1_cap4000 | u64 | 1 | 8 | 15.838 | 0.250 | 2.366 | 2.043 |
| zipf_s1.0_gim | u64 | 16 | 8 | 15.236 | 17.380 | 63.172 | 54.820 |
| zipf_s1.4_read-heavy | u64 | 16 | 4 | 13.620 | 2.190 | 163.291 | 143.717 |
| arc_P1_cap4000 | u64 | 1 | 4 | 11.908 | 2.000 | 5.683 | 5.078 |
| zipf_s1.4_gim | u64 | 16 | 1 | 9.039 | 0.000 | 174.258 | 159.812 |
| arc_S3_cap4000 | u64 | 1 | 8 | 8.528 | 0.220 | 2.087 | 1.923 |
| zipf_s1.4_gim | string | 1 | 1 | 6.252 | 0.000 | 16.300 | 15.341 |
| twitter_cluster006 | u64 | 16 | 8 | 5.771 | 27.550 | 32.988 | 31.188 |
| twitter_cluster006 | string | 8 | 1 | 3.938 | 0.010 | 25.355 | 24.394 |
