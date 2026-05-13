# sieve_r4 vs c17s vs senba::concurrent — 432-cell sweep

cap=4096, shards=512, zipf, 3 trials/cell, value=u64+string, threads=1/4/8/16, skew=0.8/1.0/1.4, mix=gim/read-heavy

`r4_vs_c17s_pct` = (r4/c17s - 1) × 100; `r4_vs_senba_pct` = (r4/senba - 1) × 100

| value | mix | skew | T | c17s | r4 | senba | r4_vs_c17s_pct | r4_vs_senba_pct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| string | gim | 0.8 | 1 | 7.32 | 5.99 | 5.17 | -18.1 | 15.8 |
| string | gim | 0.8 | 4 | 13.88 | 11.47 | 9.31 | -17.4 | 23.3 |
| string | gim | 0.8 | 8 | 11.56 | 15.73 | 13.08 | 36.1 | 20.3 |
| string | gim | 0.8 | 16 | 10.14 | 22.91 | 19.1 | 125.9 | 19.9 |
| string | gim | 1.0 | 1 | 10.06 | 8.3 | 7.28 | -17.4 | 14.1 |
| string | gim | 1.0 | 4 | 21.62 | 17.72 | 14.9 | -18.0 | 18.9 |
| string | gim | 1.0 | 8 | 21.89 | 26.65 | 22.87 | 21.7 | 16.6 |
| string | gim | 1.0 | 16 | 19.41 | 36.93 | 31.57 | 90.2 | 17.0 |
| string | gim | 1.4 | 1 | 16.99 | 16.13 | 13.36 | -5.0 | 20.8 |
| string | gim | 1.4 | 4 | 56.88 | 53.45 | 35.18 | -6.0 | 51.9 |
| string | gim | 1.4 | 8 | 87.35 | 73.9 | 52.4 | -15.4 | 41.0 |
| string | gim | 1.4 | 16 | 110.62 | 100.94 | 57.76 | -8.7 | 74.8 |
| string | read-heavy | 0.8 | 1 | 13.46 | 12.47 | 11.22 | -7.4 | 11.1 |
| string | read-heavy | 0.8 | 4 | 39.52 | 37.81 | 33.91 | -4.3 | 11.5 |
| string | read-heavy | 0.8 | 8 | 54.06 | 57.17 | 49.64 | 5.7 | 15.2 |
| string | read-heavy | 0.8 | 16 | 65.75 | 78.95 | 60.72 | 20.1 | 30.0 |
| string | read-heavy | 1.0 | 1 | 12.79 | 11.97 | 10.86 | -6.4 | 10.2 |
| string | read-heavy | 1.0 | 4 | 39.42 | 37.36 | 27.17 | -5.2 | 37.5 |
| string | read-heavy | 1.0 | 8 | 56.8 | 57.25 | 44.25 | 0.8 | 29.4 |
| string | read-heavy | 1.0 | 16 | 65.68 | 70.84 | 56.98 | 7.9 | 24.3 |
| string | read-heavy | 1.4 | 1 | 16.82 | 15.44 | 12.71 | -8.2 | 21.5 |
| string | read-heavy | 1.4 | 4 | 43.43 | 48.81 | 30.03 | 12.4 | 62.5 |
| string | read-heavy | 1.4 | 8 | 73.43 | 67.94 | 45.69 | -7.5 | 48.7 |
| string | read-heavy | 1.4 | 16 | 103.06 | 83.95 | 53.04 | -18.5 | 58.3 |
| u64 | gim | 0.8 | 1 | 10.35 | 10.4 | 7.62 | 0.4 | 36.3 |
| u64 | gim | 0.8 | 4 | 29.2 | 29.79 | 17.3 | 2.0 | 72.2 |
| u64 | gim | 0.8 | 8 | 45.45 | 42.66 | 24.34 | -6.1 | 75.3 |
| u64 | gim | 0.8 | 16 | 60.04 | 60.11 | 31.2 | 0.1 | 92.7 |
| u64 | gim | 1.0 | 1 | 14.3 | 13.9 | 11.11 | -2.8 | 25.1 |
| u64 | gim | 1.0 | 4 | 37.94 | 40.44 | 25.46 | 6.6 | 58.9 |
| u64 | gim | 1.0 | 8 | 63.11 | 63.26 | 37.68 | 0.2 | 67.9 |
| u64 | gim | 1.0 | 16 | 77.74 | 78.61 | 50.69 | 1.1 | 55.1 |
| u64 | gim | 1.4 | 1 | 24.56 | 24.06 | 18.86 | -2.0 | 27.6 |
| u64 | gim | 1.4 | 4 | 81.9 | 75.3 | 43.72 | -8.1 | 72.2 |
| u64 | gim | 1.4 | 8 | 109.14 | 114.92 | 55.82 | 5.3 | 105.9 |
| u64 | gim | 1.4 | 16 | 159.24 | 192.11 | 69.32 | 20.6 | 177.1 |
| u64 | read-heavy | 0.8 | 1 | 16.14 | 15.4 | 13.61 | -4.6 | 13.1 |
| u64 | read-heavy | 0.8 | 4 | 46.98 | 48.52 | 43.3 | 3.3 | 12.1 |
| u64 | read-heavy | 0.8 | 8 | 71.12 | 74.98 | 61.62 | 5.4 | 21.7 |
| u64 | read-heavy | 0.8 | 16 | 103.26 | 105.49 | 72.24 | 2.2 | 46.0 |
| u64 | read-heavy | 1.0 | 1 | 17.06 | 16.81 | 13.97 | -1.5 | 20.3 |
| u64 | read-heavy | 1.0 | 4 | 56.38 | 52.65 | 40.96 | -6.6 | 28.5 |
| u64 | read-heavy | 1.0 | 8 | 81.45 | 79.56 | 61.01 | -2.3 | 30.4 |
| u64 | read-heavy | 1.0 | 16 | 110.78 | 128.7 | 81.89 | 16.2 | 57.2 |
| u64 | read-heavy | 1.4 | 1 | 21.85 | 24.54 | 18.92 | 12.3 | 29.7 |
| u64 | read-heavy | 1.4 | 4 | 76.5 | 70.09 | 40.8 | -8.4 | 71.8 |
| u64 | read-heavy | 1.4 | 8 | 109.32 | 102.25 | 60.63 | -6.5 | 68.6 |
| u64 | read-heavy | 1.4 | 16 | 138.93 | 153.67 | 74.2 | 10.6 | 107.1 |

## Accept 基準達否 (設計 §G4)

### V=string

- r4 vs c17s   : median **-5.6%**, worst **-18.5%**
- r4 vs senba  : median **+21.1%**, worst **+10.2%**
- accept (V=string: median ≥ +30%, worst ≥ +20% vs senba): **FAIL**

### V=u64

- r4 vs c17s   : median **+0.3%**, worst **-8.4%**
- r4 vs senba  : median **+56.2%**, worst **+12.1%**
- accept (V=u64: median ≥ -5%, worst ≥ -10% vs c17s): **PASS**


## Δ% breakdown by axis

### V=string

#### by mix

- **mix=gim**: r4_vs_c17s median -7.3% / worst -18.1%, r4_vs_senba median +20.1% / worst +14.1%
- **mix=read-heavy**: r4_vs_c17s median -4.8% / worst -18.5%, r4_vs_senba median +26.9% / worst +10.2%

#### by skew

- **skew=0.8**: r4_vs_c17s median +0.7% / worst -18.1%, r4_vs_senba median +17.9% / worst +11.1%
- **skew=1.0**: r4_vs_c17s median -2.2% / worst -18.0%, r4_vs_senba median +17.9% / worst +10.2%
- **skew=1.4**: r4_vs_c17s median -7.8% / worst -18.5%, r4_vs_senba median +50.3% / worst +20.8%

#### by T

- **T=1**: r4_vs_c17s median -7.8% / worst -18.1%, r4_vs_senba median +14.9% / worst +10.2%
- **T=4**: r4_vs_c17s median -5.6% / worst -18.0%, r4_vs_senba median +30.4% / worst +11.5%
- **T=8**: r4_vs_c17s median +3.2% / worst -15.4%, r4_vs_senba median +24.9% / worst +15.2%
- **T=16**: r4_vs_c17s median +14.0% / worst -18.5%, r4_vs_senba median +27.1% / worst +17.0%

### V=u64

#### by mix

- **mix=gim**: r4_vs_c17s median +0.3% / worst -8.1%, r4_vs_senba median +70.1% / worst +25.1%
- **mix=read-heavy**: r4_vs_c17s median +0.4% / worst -8.4%, r4_vs_senba median +30.0% / worst +12.1%

#### by skew

- **skew=0.8**: r4_vs_c17s median +1.2% / worst -6.1%, r4_vs_senba median +41.1% / worst +12.1%
- **skew=1.0**: r4_vs_c17s median -0.7% / worst -6.6%, r4_vs_senba median +42.8% / worst +20.3%
- **skew=1.4**: r4_vs_c17s median +1.6% / worst -8.4%, r4_vs_senba median +72.0% / worst +27.6%

#### by T

- **T=1**: r4_vs_c17s median -1.8% / worst -4.6%, r4_vs_senba median +26.4% / worst +13.1%
- **T=4**: r4_vs_c17s median -2.3% / worst -8.4%, r4_vs_senba median +65.3% / worst +12.1%
- **T=8**: r4_vs_c17s median -1.0% / worst -6.5%, r4_vs_senba median +68.2% / worst +21.7%
- **T=16**: r4_vs_c17s median +6.4% / worst +0.1%, r4_vs_senba median +75.0% / worst +46.0%

