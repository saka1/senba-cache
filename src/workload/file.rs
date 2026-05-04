use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use super::Key;

/// 1 行 1 整数の ASCII テキストを `Iterator<Item = Key>` として返す。
/// NSDI 同梱 `mydata/zipf/zipf_1.0` 形式。
pub fn from_path(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = Key>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file).lines().map(|line| {
        line.expect("io error reading trace line")
            .trim()
            .parse::<Key>()
            .expect("trace line is not a u64 key")
    }))
}

/// Twitter cache trace (OSDI'20, Yang et al.) の CSV 1 本を
/// `Iterator<Item = Key>` として返す。
///
/// フォーマット: `timestamp,anonymized_key,ksize,vsize,client,op,ttl`
/// (Twitter 公式配布のテキスト形式。libCacheSim の oracleGeneral binary とは別物)
///
/// 設計判断:
/// - **CSV crate を入れない**: anonymized_key は base64 風で `,` を含まないため
///   `split(',')` で十分。依存追加のコストに見合わない。
/// - **String key を u64 に hash で潰す** (intern しない): bench harness が
///   `Cache<u64, _>` 前提なので変換は必須。1M unique key 規模で
///   `DefaultHasher` (SipHash-1-3) の衝突確率は ~3e-8 で hit ratio 比較には
///   実害なし。HashMap intern も検討したが (a) trace 2 pass か全 in-memory が
///   要る、(b) 衝突ゼロ保証は本実験で不要、で却下。
/// - **op 列を無視して全行を access として流す**: 現 harness の
///   get-then-insert モデルに合わせる。op を尊重すると harness 側拡張が要り
///   scope が膨らむ。Twitter cluster006/018/019 はいずれも get-dominant
///   (98/96/75%) で、hit ratio の order を見る本目的では十分。
/// - **ストリーミング (`.lines().map`)**: `Vec` に集めず Iterator を返す。
///   bench 側の `take(n).collect()` (`--len N`) がそのまま効くように
///   既存 `from_path` と API を揃える。
pub fn twitter_csv_from_path(
    path: impl AsRef<Path>,
) -> io::Result<impl Iterator<Item = Key>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file).lines().map(|line| {
        let line = line.expect("io error reading trace line");
        let key_str = line
            .split(',')
            .nth(1)
            .expect("malformed Twitter CSV row: no key column");
        let mut h = DefaultHasher::new();
        key_str.hash(&mut h);
        h.finish()
    }))
}
