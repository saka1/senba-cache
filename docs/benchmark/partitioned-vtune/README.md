# partitioned / r1 VTune diagnostic data

`docs/reports/2026-05-13-partitioned-vtune.md` と `2026-05-13-partitioned-cap1024-sweep.md` の VTune セルの生サマリ出力 (Windows host, `vtune -report summary` 相当) を保管する。VTune の `-result-dir` 本体 (数百 MB の binary) は Windows host 側に残し、本リポジトリには報告書で参照される数値の出所として **CLI summary text のみ** を置く。

## ファイル命名

`{variant}_[wN_]cap{C}_T{T}_{collect}.txt`

- `variant`: `partitioned` | `r1` 等
- `w{N}`: r1 の `--ways=N` (variant に必要なときのみ)
- `cap{C}`: `--cap` 値
- `T{T}`: `--threads` 値
- `collect`: `memory-access` | `uarch-exploration`

## 共通実行パラメータ

特記がなければ全 cell で `--shards=64 --keys=100000 --skew=1.4 --warmup=400000 --ops=100000000 --seed=42` を使う (= `partitioned-vtune.md` 設計 gate に揃える)。ファイル先頭の `# vtune -collect ... -- bench_vtune_concurrent.exe ...` コメントが実行コマンドの完全形。

## 現状ファイル

| ファイル | variant | cap | T | collect | 出所 |
|---|---|---|---|---|---|
| `r1_w16_cap4096_T8_memory-access.txt` | r1 ways=16 | 4096 | 8 | memory-access | `partitioned-cap1024-sweep.md` §結果 |
| `r1_w16_cap4096_T16_memory-access.txt` | r1 ways=16 | 4096 | 16 | memory-access | 同上 (天井: 178.6 Mops) |
| `r1_w16_cap1024_T8_memory-access.txt` | r1 ways=16 | 1024 | 8 | memory-access | 同上 |
| `r1_w16_cap1024_T16_memory-access.txt` | r1 ways=16 | 1024 | 16 | memory-access | 同上 |

`partitioned-vtune.md` の 9 cell (cap ∈ {4096, 1024} × T ∈ {1,8,16}) は同形式での保存が未完。次に Windows 側で取り直すか、既存 `-result-dir` から `vtune -report summary -result-dir ...` で復元してここに保存するかは保留。

## binary

`cargo xwin build --release -p senba-research --bin bench_vtune_concurrent --target x86_64-pc-windows-msvc --features "senba/concurrent"` で `target/x86_64-pc-windows-msvc/release/bench_vtune_concurrent.{exe,pdb}` を生成。Windows host に転送して使う。
