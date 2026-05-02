use std::fs::File;
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
