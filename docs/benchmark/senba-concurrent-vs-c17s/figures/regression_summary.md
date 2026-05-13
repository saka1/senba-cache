# senba::concurrent vs c17s — regression table

cap=4096, shards=512, zipf, 3 trials, machine: 12600K (16 threads)

`ratio` = senba_concurrent / c17s; `delta_pct` = (ratio - 1) * 100

| mix | skew | T | c17s_Mops | senba_Mops | ratio | delta_pct |
| --- | --- | --- | --- | --- | --- | --- |
| gim | 0.8 | 1 | 10.63 | 7.88 | 0.742 | -25.8 |
| gim | 0.8 | 4 | 29.03 | 17.69 | 0.609 | -39.1 |
| gim | 0.8 | 8 | 44.96 | 24.88 | 0.553 | -44.7 |
| gim | 0.8 | 16 | 63.71 | 29.84 | 0.468 | -53.2 |
| gim | 1.0 | 1 | 14.55 | 11.64 | 0.8 | -20.0 |
| gim | 1.0 | 4 | 41.06 | 25.09 | 0.611 | -38.9 |
| gim | 1.0 | 8 | 63.4 | 36.41 | 0.574 | -42.6 |
| gim | 1.0 | 16 | 88.85 | 47.15 | 0.531 | -46.9 |
| gim | 1.4 | 1 | 24.12 | 20.54 | 0.852 | -14.8 |
| gim | 1.4 | 4 | 84.57 | 45.55 | 0.539 | -46.1 |
| gim | 1.4 | 8 | 118.94 | 51.72 | 0.435 | -56.5 |
| gim | 1.4 | 16 | 184.25 | 67.35 | 0.366 | -63.4 |
| read-heavy | 0.8 | 1 | 16.55 | 14.5 | 0.876 | -12.4 |
| read-heavy | 0.8 | 4 | 53.92 | 44.78 | 0.83 | -17.0 |
| read-heavy | 0.8 | 8 | 73.58 | 59.68 | 0.811 | -18.9 |
| read-heavy | 0.8 | 16 | 100.95 | 82.86 | 0.821 | -17.9 |
| read-heavy | 1.0 | 1 | 17.68 | 14.73 | 0.833 | -16.7 |
| read-heavy | 1.0 | 4 | 56.82 | 39.74 | 0.7 | -30.0 |
| read-heavy | 1.0 | 8 | 82.45 | 59.72 | 0.724 | -27.6 |
| read-heavy | 1.0 | 16 | 117.4 | 88.38 | 0.753 | -24.7 |
| read-heavy | 1.4 | 1 | 23.61 | 20.06 | 0.85 | -15.0 |
| read-heavy | 1.4 | 4 | 75.2 | 46.2 | 0.614 | -38.6 |
| read-heavy | 1.4 | 8 | 102.52 | 59.31 | 0.579 | -42.1 |
| read-heavy | 1.4 | 16 | 142.01 | 71.29 | 0.502 | -49.8 |


## Aggregate

- worst-cell delta: -63.4%
- best-cell delta : -12.4%
- median delta    : -34.3%
- mean delta      : -33.4%

## By op_mix

- **gim**: median -43.7%, worst -63.4%
- **read-heavy**: median -21.8%, worst -49.8%

## By skew

- **skew=0.8**: median -22.4%, worst -53.2%
- **skew=1.0**: median -28.8%, worst -46.9%
- **skew=1.4**: median -44.1%, worst -63.4%

## By threads

- **T=1**: median -15.8%, worst -25.8%
- **T=4**: median -38.8%, worst -46.1%
- **T=8**: median -42.4%, worst -56.5%
- **T=16**: median -48.3%, worst -63.4%
