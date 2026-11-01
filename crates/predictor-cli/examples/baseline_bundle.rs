//! Assemble a diagnostic Tatoeba bundle using the existing package/credit writers.
use predictor_cli::pipeline;
use predictor_core::{Model, PackageMetadata, Result};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("baseline_bundle RUN_ROOT NEW_BUNDLE_DIRECTORY FINAL_RUNTIME_BINARY".into());
    }
    let root = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    fs::create_dir(&out)?;
    let config: Value = serde_json::from_slice(&fs::read(root.join("config.json"))?)?;
    let report: Value = serde_json::from_slice(&fs::read(root.join("prepared/report.json"))?)?;
    let provenance = root.join("checkpoint/provenance");
    let metadata = PackageMetadata {
        provenance: json!({
            "source":config["source_sha256"],
            "train_sha256":pipeline::hash_file(&root.join("prepared/train.txt"))?,
            "split":report["split_sha256"],
            "config_sha256":pipeline::hash_file(&root.join("config.json"))?,
            "holdout_sha256":pipeline::hash_file(&root.join("holdout.txt"))?,
            "arpa_sha256":pipeline::hash_file(&root.join("training/model.arpa"))?,
            "kenlm_sha256":config["kenlm_sha256"],
            "training_runtime_binary_sha256":pipeline::hash_file(&root.join("lp"))?,
            "runtime_binary_sha256":pipeline::hash_file(&PathBuf::from(&args[3]))?,
            "builder_binary_sha256":pipeline::hash_file(&std::env::current_exe()?)?,
            "training_source_tree_sha256":pipeline::source_tree_hash(&root.join("checkpoint"))?,
            "role":"diagnostic baseline; no final-test tuning; near-duplicate isolation incomplete"
        }),
        attribution: json!({"source":"Tatoeba contributors","license":"CC-BY-2.0-FR","attribution_sidecar":"attribution.jsonl.bz2","attribution_format":"linka-compact-attribution-jsonl-v1","license_file":"CC-BY-2.0-FR.html"}),
        word_counts: pipeline::word_counts(&root.join("prepared/frequencies.tsv"))?,
        ..Default::default()
    };
    Model::compile_arpa(
        root.join("training/model.arpa"),
        out.join("model.lpk"),
        &metadata,
    )?;
    Model::load(out.join("model.lpk"))?;
    pipeline::compact_attribution(&root.join("prepared"), &out.join("attribution.jsonl.bz2"))?;
    for (source, name) in [
        (
            provenance.join("licenses/CC-BY-2.0-FR.html"),
            "CC-BY-2.0-FR.html",
        ),
        (
            provenance.join("licenses/tatoeba-usage.html"),
            "tatoeba-usage.html",
        ),
        (root.join("config.json"), "config.json"),
    ] {
        fs::copy(source, out.join(name))?;
    }
    fs::write(out.join("NOTICES.txt"), "Tatoeba Russian sentences, contributors and original sentence IDs in attribution.jsonl.bz2; https://tatoeba.org/ ; CC BY 2.0 France, https://creativecommons.org/licenses/by/2.0/fr/ . Modified: Unicode normalization, segmentation, exact-family/original component deduplication, train-only MKN trigram estimation. Raw source and audit hashes are bound in model provenance and credit header; original audit retained in the run's prepared/attribution.jsonl. No OpenCorpora or SOVA data included. Diagnostic local bundle, not human acceptance or product release. Runtime executable/dependency notices and delivery are outside this data-only bundle.\n")?;
    let sizes = pipeline::bundle_sizes(&out, pipeline::COMMON_PACK_LIMIT)?;
    assert!(sizes.unpacked_bytes <= 500_000_000);
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"model_sha256":pipeline::hash_file(&out.join("model.lpk"))?,"sizes":sizes,"quality_pass":false})
        )?
    );
    Ok(())
}
