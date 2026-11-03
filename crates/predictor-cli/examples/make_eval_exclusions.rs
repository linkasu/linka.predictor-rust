//! Curator-only: stdin JSONL -> hash-only exclusion manifest. No target text on stdout/stderr.
use predictor_cli::prepare::exclusions::{make_index, CuratorRecord, INDEX_LIMIT};
use predictor_core::Result;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err(
            "usage: make_eval_exclusions SOURCE_SEAL_SHA256 NEW_OUTPUT_JSON < CURATOR_JSONL".into(),
        );
    }
    let mut input = String::new();
    std::io::stdin()
        .take(INDEX_LIMIT + 1)
        .read_to_string(&mut input)
        .map_err(|_| "invalid curator UTF-8 input")?;
    if input.len() as u64 > INDEX_LIMIT {
        return Err("curator input exceeds 16 MiB".into());
    }
    let records: Vec<CuratorRecord> = input
        .lines()
        .map(|line| serde_json::from_str(line).map_err(|_| "invalid curator JSONL record"))
        .collect::<std::result::Result<_, _>>()?;
    let index = make_index(&records, &args[1])?;
    let bytes = serde_json::to_vec_pretty(&index)?;
    if bytes.len() as u64 > INDEX_LIMIT {
        return Err("hash-only index exceeds 16 MiB".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[2])?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
