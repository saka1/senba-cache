//! 1 トレースを各 variant に流して CSV を stdout に吐く CLI。
//!
//! 例:
//!   cargo run --release --bin bench -- \
//!     --source zipf --skew 1.1 --keys 100000 --len 1000000 --seed 42 \
//!     --capacity 1024,4096,16384 --variant orig,v0
//!
//!   cargo run --release --bin bench -- \
//!     --source file --path external/NSDI24-SIEVE/mydata/zipf/zipf_1.0 \
//!     --capacity 4096 --variant orig,v0
//!
//!   cargo run --release --bin bench -- \
//!     --source twitter --path external/twitter-cache-trace/cluster018 \
//!     --capacity 4096 --variant orig,j5_n32

use std::time::Instant;

use senba_cache::Cache;
// 2026-05-05: ベースラインを orig vs j7 に絞り込み。過去 variant は
// 必要になれば use と matcher を復活させる (テスト・実装は残置)。
// use senba_cache::sieve_j3::SieveCache as J3;
// use senba_cache::sieve_j4::SieveCache as J4;
// use senba_cache::sieve_j5::SieveCache as J5;
// use senba_cache::sieve_j6::SieveCache as J6;
use senba_cache::sieve_j7::SieveCache as J7;
use senba_cache::sieve_orig::SieveCache as Orig;
// use senba_cache::sieve_v0::SieveCache as V0;
// use senba_cache::sieve_v3::SieveCache as V3;
use senba_cache::workload::file;
use senba_cache::workload::zipf::ZipfGen;

struct Args {
    source: String,
    skew: f64,
    keys: u64,
    seed: u64,
    len: Option<usize>,
    path: Option<String>,
    capacities: Vec<usize>,
    variants: Vec<String>,
}

struct Stats {
    elapsed_ns: u128,
    hits: u64,
    misses: u64,
    evictions: u64,
}

fn drive<C: Cache<u64, u64>>(trace: &[u64], cap: usize) -> Stats {
    let mut c = C::new(cap);
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut evictions = 0u64;
    let t0 = Instant::now();
    for &k in trace {
        if c.get(&k).is_some() {
            hits += 1;
        } else {
            misses += 1;
            if c.insert(k, k).is_some() {
                evictions += 1;
            }
        }
    }
    Stats {
        elapsed_ns: t0.elapsed().as_nanos(),
        hits,
        misses,
        evictions,
    }
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut source = String::from("zipf");
    let mut skew = f64::NAN;
    let mut keys = 0u64;
    let mut seed = 0u64;
    let mut len: Option<usize> = None;
    let mut path: Option<String> = None;
    let mut capacities: Vec<usize> = Vec::new();
    let mut variants: Vec<String> = Vec::new();

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--source" => source = val().clone(),
            "--skew" => skew = val().parse().expect("--skew is f64"),
            "--keys" => keys = val().parse().expect("--keys is u64"),
            "--seed" => seed = val().parse().expect("--seed is u64"),
            "--len" => len = Some(val().parse().expect("--len is usize")),
            "--path" => path = Some(val().clone()),
            "--capacity" => {
                capacities = val()
                    .split(',')
                    .map(|s| s.trim().parse::<usize>().expect("--capacity entry is usize"))
                    .collect();
            }
            "--variant" => {
                variants = val().split(',').map(|s| s.trim().to_string()).collect();
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench --source <zipf|file|twitter> [--skew F --keys N --seed N --len N | --path P] --capacity C1,C2,... --variant orig,v0"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    if capacities.is_empty() {
        panic!("--capacity is required");
    }
    if variants.is_empty() {
        panic!("--variant is required");
    }

    Args {
        source,
        skew,
        keys,
        seed,
        len,
        path,
        capacities,
        variants,
    }
}

fn build_trace(args: &Args) -> Vec<u64> {
    match args.source.as_str() {
        "zipf" => {
            let n = args.len.expect("--len required for --source zipf");
            assert!(args.keys > 0, "--keys required for --source zipf");
            assert!(args.skew.is_finite(), "--skew required for --source zipf");
            ZipfGen::new(args.skew, args.keys, args.seed)
                .take(n)
                .collect()
        }
        "file" => {
            let p = args.path.as_ref().expect("--path required for --source file");
            let it = file::from_path(p).expect("open trace");
            match args.len {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        }
        // `file` (NSDI24 zipf_1.0 = u64 1 列) と `twitter` (OSDI'20 CSV) は
        // 別 source として扱う。auto-detect は拡張子なし trace で誤判定しうる
        // し、将来 oracleGeneral binary を増やすときも同じパターンで分岐できる。
        "twitter" => {
            let p = args
                .path
                .as_ref()
                .expect("--path required for --source twitter");
            let it = file::twitter_csv_from_path(p).expect("open trace");
            match args.len {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        }
        other => panic!("unknown --source: {other}"),
    }
}

fn main() {
    let args = parse_args();
    let trace = build_trace(&args);

    println!("variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions");
    for v in &args.variants {
        for &cap in &args.capacities {
            let s = match v.as_str() {
                "orig" => drive::<Orig<u64, u64>>(&trace, cap),
                // 2026-05-05: ベースラインは orig vs j7 のみ。過去 variant は
                // モジュール自体は残置 (`src/sieve_*.rs` + `src/lib.rs`)、必要な
                // 比較が再発したらここを復活させる。
                // "v0" => drive::<V0<u64, u64>>(&trace, cap),
                // "v3" => drive::<V3<u64, u64>>(&trace, cap),
                // "j3" => drive::<J3<u64, u64>>(&trace, cap),
                // "j4" => drive::<J4<u64, u64>>(&trace, cap),
                // "j4_n1" => drive::<J4<u64, u64, 1>>(&trace, cap),
                // "j4_n2" => drive::<J4<u64, u64, 2>>(&trace, cap),
                // "j4_n4" => drive::<J4<u64, u64, 4>>(&trace, cap),
                // "j4_n8" => drive::<J4<u64, u64, 8>>(&trace, cap),
                // "j4_n16" => drive::<J4<u64, u64, 16>>(&trace, cap),
                // "j4_n32" => drive::<J4<u64, u64, 32>>(&trace, cap),
                // "j4_n64" => drive::<J4<u64, u64, 64>>(&trace, cap),
                // "j4_n128" => drive::<J4<u64, u64, 128>>(&trace, cap),
                // "j5" => drive::<J5<u64, u64>>(&trace, cap),
                // "j5_n1" => drive::<J5<u64, u64, 1>>(&trace, cap),
                // "j5_n2" => drive::<J5<u64, u64, 2>>(&trace, cap),
                // "j5_n4" => drive::<J5<u64, u64, 4>>(&trace, cap),
                // "j5_n8" => drive::<J5<u64, u64, 8>>(&trace, cap),
                // "j5_n16" => drive::<J5<u64, u64, 16>>(&trace, cap),
                // "j5_n32" => drive::<J5<u64, u64, 32>>(&trace, cap),
                // "j5_n64" => drive::<J5<u64, u64, 64>>(&trace, cap),
                // "j5_n128" => drive::<J5<u64, u64, 128>>(&trace, cap),
                // "j5_n256" => drive::<J5<u64, u64, 256>>(&trace, cap),
                // "j5_n512" => drive::<J5<u64, u64, 512>>(&trace, cap),
                // "j5_n1024" => drive::<J5<u64, u64, 1024>>(&trace, cap),
                // "j5_n2048" => drive::<J5<u64, u64, 2048>>(&trace, cap),
                // "j6" => drive::<J6<u64, u64>>(&trace, cap),
                // "j6_n1" => drive::<J6<u64, u64, 1>>(&trace, cap),
                // "j6_n2" => drive::<J6<u64, u64, 2>>(&trace, cap),
                // "j6_n4" => drive::<J6<u64, u64, 4>>(&trace, cap),
                // "j6_n8" => drive::<J6<u64, u64, 8>>(&trace, cap),
                // "j6_n16" => drive::<J6<u64, u64, 16>>(&trace, cap),
                // "j6_n32" => drive::<J6<u64, u64, 32>>(&trace, cap),
                // "j6_n64" => drive::<J6<u64, u64, 64>>(&trace, cap),
                // "j6_n128" => drive::<J6<u64, u64, 128>>(&trace, cap),
                // "j6_n256" => drive::<J6<u64, u64, 256>>(&trace, cap),
                // "j6_n512" => drive::<J6<u64, u64, 512>>(&trace, cap),
                // "j6_n1024" => drive::<J6<u64, u64, 1024>>(&trace, cap),
                // "j6_n2048" => drive::<J6<u64, u64, 2048>>(&trace, cap),
                "j7" => drive::<J7<u64, u64>>(&trace, cap),
                "j7_n1" => drive::<J7<u64, u64, 1>>(&trace, cap),
                "j7_n2" => drive::<J7<u64, u64, 2>>(&trace, cap),
                "j7_n4" => drive::<J7<u64, u64, 4>>(&trace, cap),
                "j7_n8" => drive::<J7<u64, u64, 8>>(&trace, cap),
                "j7_n16" => drive::<J7<u64, u64, 16>>(&trace, cap),
                "j7_n32" => drive::<J7<u64, u64, 32>>(&trace, cap),
                "j7_n64" => drive::<J7<u64, u64, 64>>(&trace, cap),
                "j7_n128" => drive::<J7<u64, u64, 128>>(&trace, cap),
                "j7_n256" => drive::<J7<u64, u64, 256>>(&trace, cap),
                "j7_n512" => drive::<J7<u64, u64, 512>>(&trace, cap),
                "j7_n1024" => drive::<J7<u64, u64, 1024>>(&trace, cap),
                "j7_n2048" => drive::<J7<u64, u64, 2048>>(&trace, cap),
                other => panic!("unknown variant: {other}"),
            };
            println!(
                "{v},{},{},{},{},{cap},{},{},{},{}",
                args.source,
                args.skew,
                args.keys,
                trace.len(),
                s.elapsed_ns,
                s.hits,
                s.misses,
                s.evictions
            );
        }
    }
}
