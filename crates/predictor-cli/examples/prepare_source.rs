//! Explicit-source preparation, separate from the historical three-argument CLI.
use predictor_cli::prepare::{prepare_with_source, review_with_source, SourceEvidence};
use predictor_core::Result;
use std::{fs::File, path::Path};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 || !matches!(args[1].as_str(), "review" | "train") {
        return Err(
            "usage: prepare_source review|train SOURCE_MANIFEST RAW_TSV_OR_BZ2 NEW_OUTPUT HASH_EXCLUSIONS_JSON".into(),
        );
    }
    let manifest: serde_json::Value = serde_json::from_reader(File::open(&args[2])?)?;
    let source: SourceEvidence = serde_json::from_value(manifest["source"].clone())?;
    let prepare = if args[1] == "review" {
        review_with_source
    } else {
        prepare_with_source
    };
    let report = prepare(
        Path::new(&args[3]),
        Path::new(&args[4]),
        Path::new(&args[5]),
        &source,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
