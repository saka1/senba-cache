use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read};
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
pub fn twitter_csv_from_path(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = Key>> {
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

/// libCacheSim 同梱 CSV (`# time, object, size, next_access_vtime` 形式) から
/// object id を `Iterator<Item = u64>` で返す。
///
/// 想定: `external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv` 等。
/// 各行は `0, 13053225291711363978, 737, 13` のように `, ` 区切り。
///
/// `twitter_csv_from_path` (OSDI'20 Yang 形式) との差:
/// - col 1 が **数値** object id なので String hash を経由せず u64 で直接読む
///   → libCacheSim 側 (cachesim) のキー扱いと対称になる
/// - 先頭の `# ...` コメント行を skip する
/// - `, ` の空白に対応するため `trim()` してからパースする
pub fn libcachesim_csv_from_path(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = Key>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file).lines().filter_map(|line| {
        let line = line.expect("io error reading trace line");
        if line.starts_with('#') {
            return None;
        }
        let key_str = line
            .split(',')
            .nth(1)
            .expect("malformed libCacheSim CSV row: no object column");
        Some(
            key_str
                .trim()
                .parse::<Key>()
                .expect("libCacheSim CSV object column is not a u64"),
        )
    }))
}

/// Twitter cache trace の CSV から **生 String キー**を `Iterator<Item = String>` で返す。
///
/// `twitter_csv_from_path` の pre-hash 版に対する peer。`senba::Cache<K, V>` のように
/// 任意 `K: Hash + Eq` を取れる実装が出てきたため、String キーをそのままキャッシュに
/// 流して実 workload のキー比較・ハッシュコストを実測するために用意した。
/// op 列を無視する点・ストリーミングで返す点は `twitter_csv_from_path` と同一。
pub fn twitter_csv_from_path_string(
    path: impl AsRef<Path>,
) -> io::Result<impl Iterator<Item = String>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file).lines().map(|line| {
        let line = line.expect("io error reading trace line");
        line.split(',')
            .nth(1)
            .expect("malformed Twitter CSV row: no key column")
            .to_string()
    }))
}

/// ARC paper trace (mokabench 同梱 `external/mokabench/cache-trace/arc/*.lis[.zst]`)
/// を `Iterator<Item = Key>` で返す。
///
/// **出典 / リファレンス実装**: mokabench
/// (<https://github.com/moka-rs/mokabench>) — trace dataset は
/// `cache-trace` submodule (<https://github.com/moka-rs/cache-trace>)、
/// パーサ意味論は mokabench の `src/parser.rs::GenericTraceParser` に準拠。
/// 我々は load generator 本体は流用せず trace 形式だけを拝借し、
/// 既存の `bench` driver にそのまま乗せる方針 (理由は
/// `docs/reports/2026-05-08-mokabench-arc-traces.md` 参照予定)。
///
/// フォーマット: 1 行 `start len ...` または `start` (len 省略時は 1)。
/// `start..start+len` の各整数を 1 アクセスとして展開する。
///
/// 拡張子 `.zst` の場合は zstd で on-the-fly 展開する。`spc1likeread` のような
/// 分割 zst (`.zst.00`, `.zst.01`, ...) はサポートしない (連結が要るため)。
/// LIRS の `*` (NOOP 行) は今のところスキップしない — ARC trace には現れない。
pub fn arc_from_path(path: impl AsRef<Path>) -> io::Result<Box<dyn Iterator<Item = Key>>> {
    let p = path.as_ref();
    let file = File::open(p)?;
    let reader: Box<dyn Read> = if p.extension() == Some(OsStr::new("zst")) {
        Box::new(zstd::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let buf = BufReader::new(reader);
    Ok(Box::new(buf.lines().flat_map(|line| {
        let line = line.expect("io error reading ARC trace line");
        let mut t = line.split_whitespace();
        let start: u64 = t
            .next()
            .expect("ARC trace: empty line")
            .parse()
            .expect("ARC trace: start is not u64");
        let len: u64 = t
            .next()
            .map(|s| s.parse().expect("ARC trace: len is not u64"))
            .unwrap_or(1);
        start..(start + len)
    })))
}
