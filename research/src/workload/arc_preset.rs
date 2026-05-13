//! mokabench の `TraceFile::default_capacities` 由来の ARC trace preset。
//!
//! ARC paper trace ごとに「mokabench が canonical な評価点として採用している capacity」
//! と「`external/mokabench/cache-trace/arc/` 下の zstd 圧縮 trace path」のペアを返す。
//! 値は `external/mokabench/src/trace_file.rs` の対応行を転記。
//!
//! `spc1likeread` は split zst (`.zst.00`/`.zst.01`/...) なので連結 reader が要り、
//! 現状の `arc_from_path` (file.rs) で読めないため preset から外す。

pub struct ArcPreset {
    pub trace_path: &'static str,
    pub default_capacities: &'static [usize],
}

/// `name` (case-insensitive) を mokabench preset に解決する。
/// 未知の name には `None`。
pub fn lookup(name: &str) -> Option<ArcPreset> {
    let (trace_path, default_capacities): (&'static str, &'static [usize]) =
        match name.trim().to_ascii_lowercase().as_str() {
            "concat" => (
                "external/mokabench/cache-trace/arc/ConCat.lis.zst",
                &[200_000, 400_000, 3_200_000],
            ),
            "ds1" => (
                "external/mokabench/cache-trace/arc/DS1.lis.zst",
                &[1_000_000, 4_000_000, 8_000_000],
            ),
            "merge-p" | "mergep" => (
                "external/mokabench/cache-trace/arc/MergeP.lis.zst",
                &[400_000, 1_000_000, 3_200_000],
            ),
            "merge-s" | "merges" => (
                "external/mokabench/cache-trace/arc/MergeS.lis.zst",
                &[400_000, 1_000_000, 3_200_000],
            ),
            "oltp" => (
                "external/mokabench/cache-trace/arc/OLTP.lis.zst",
                &[256, 512, 1_000, 2_000],
            ),
            "p1" => (
                "external/mokabench/cache-trace/arc/P1.lis.zst",
                &[20_000, 160_000],
            ),
            "p2" => (
                "external/mokabench/cache-trace/arc/P2.lis.zst",
                &[20_000, 160_000],
            ),
            "p3" => (
                "external/mokabench/cache-trace/arc/P3.lis.zst",
                &[20_000, 160_000],
            ),
            "p4" => (
                "external/mokabench/cache-trace/arc/P4.lis.zst",
                &[20_000, 160_000],
            ),
            "p5" => (
                "external/mokabench/cache-trace/arc/P5.lis.zst",
                &[20_000, 160_000],
            ),
            "p6" => (
                "external/mokabench/cache-trace/arc/P6.lis.zst",
                &[20_000, 160_000],
            ),
            "p7" => (
                "external/mokabench/cache-trace/arc/P7.lis.zst",
                &[20_000, 160_000],
            ),
            "p8" => (
                "external/mokabench/cache-trace/arc/P8.lis.zst",
                &[20_000, 160_000],
            ),
            "p9" => (
                "external/mokabench/cache-trace/arc/P9.lis.zst",
                &[20_000, 160_000],
            ),
            "p10" => (
                "external/mokabench/cache-trace/arc/P10.lis.zst",
                &[20_000, 160_000],
            ),
            "p11" => (
                "external/mokabench/cache-trace/arc/P11.lis.zst",
                &[20_000, 160_000],
            ),
            "p12" => (
                "external/mokabench/cache-trace/arc/P12.lis.zst",
                &[20_000, 160_000],
            ),
            "p13" => (
                "external/mokabench/cache-trace/arc/P13.lis.zst",
                &[20_000, 160_000],
            ),
            "p14" => (
                "external/mokabench/cache-trace/arc/P14.lis.zst",
                &[80_000, 640_000],
            ),
            "s1" => (
                "external/mokabench/cache-trace/arc/S1.lis.zst",
                &[100_000, 800_000],
            ),
            "s2" => (
                "external/mokabench/cache-trace/arc/S2.lis.zst",
                &[100_000, 800_000],
            ),
            "s3" => (
                "external/mokabench/cache-trace/arc/S3.lis.zst",
                &[100_000, 400_000, 800_000],
            ),
            _ => return None,
        };
    Some(ArcPreset {
        trace_path,
        default_capacities,
    })
}
