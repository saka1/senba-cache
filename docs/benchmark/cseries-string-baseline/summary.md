# c-series baseline sweep summary

`aggregate_mops` (median of 3 trials), c14s vs c16s vs c17s。c17s の Δ% は c16s 比 (直近 baseline)。HR は median (Δ vs c16s)。


## value=`u64`, op-mix=`gim` (skew=1.0)

| T | c14s Mops | c16s Mops | c17s Mops | c17s Δ% vs c16s | c16s HR | c17s HR |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 32.47 | 34.92 | 33.91 | -2.9% | 0.7196 | 0.7196 |
| 8 | 45.16 | 48.91 | 49.76 | +1.8% | 0.7194 | 0.7196 |
| 16 | 64.41 | 64.77 | 62.73 | -3.2% | 0.7195 | 0.7196 |

## value=`u64`, op-mix=`read-heavy` (skew=1.4)

| T | c14s Mops | c16s Mops | c17s Mops | c17s Δ% vs c16s | c16s HR | c17s HR |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 68.60 | 68.24 | 75.89 | +11.2% | 0.9279 | 0.9282 |
| 8 | 102.55 | 100.84 | 116.22 | +15.3% | 0.9272 | 0.9281 |
| 16 | 129.62 | 132.06 | 160.73 | +21.7% | 0.9247 | 0.9281 |

## value=`string`, op-mix=`gim` (skew=1.0)

| T | c14s Mops | c16s Mops | c17s Mops | c17s Δ% vs c16s | c16s HR | c17s HR |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 23.51 | 24.85 | 23.75 | -4.5% | 0.7195 | 0.7196 |
| 8 | 33.00 | 33.04 | 34.65 | +4.9% | 0.7194 | 0.7195 |
| 16 | 44.63 | ✗ | 43.56 | — | ✗ | 0.7195 |

## value=`string`, op-mix=`read-heavy` (skew=1.4)

| T | c14s Mops | c16s Mops | c17s Mops | c17s Δ% vs c16s | c16s HR | c17s HR |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | ✗ | ✗ | 52.00 | — | ✗ | 0.9282 |
| 8 | ✗ | ✗ | 79.63 | — | ✗ | 0.9281 |
| 16 | ✗ | ✗ | 105.51 | — | ✗ | 0.9277 |

`✗` = crash (memory corruption — c14s/c16s seqlock-via-tag racing window で `ManuallyDrop<String>` の半上書き header を drop して tcache free が壊れる)。data/crashes.log を参照。
