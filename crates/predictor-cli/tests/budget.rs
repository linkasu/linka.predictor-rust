use std::{fs, io::Write};

fn compressed(path: &std::path::Path, payload: &[u8]) {
    let mut encoder =
        bzip2::write::BzEncoder::new(fs::File::create(path).unwrap(), bzip2::Compression::best());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap();
}

#[test]
fn common_budget_counts_expanded_attribution_not_compression_savings() {
    let temp = tempfile::tempdir().unwrap();
    let expanded = b"synthetic original-author id=123 family=example\n".repeat(4000);
    assert_eq!(expanded.len(), 192000);
    compressed(&temp.path().join("attribution.jsonl.bz2"), &expanded);
    fs::write(temp.path().join("model.lpk"), b"dummy model size only").unwrap();
    let unpacked = predictor_cli::pipeline::directory_bytes(temp.path()).unwrap();
    assert_eq!(unpacked, 192021);
    let sizes = predictor_cli::pipeline::bundle_sizes(temp.path(), 500_000_000).unwrap();
    assert_eq!(sizes.unpacked_bytes, 192021);
    assert_eq!(sizes.physical_bytes, 208);
    println!(
        "probe physical={} unpacked={}",
        sizes.physical_bytes, sizes.unpacked_bytes
    );
    assert!(predictor_cli::pipeline::bundle_sizes(temp.path(), 192020)
        .unwrap_err()
        .to_string()
        .contains("limit"));
}

#[test]
fn size_gate_handles_concatenated_members_corruption_and_hard_limit() {
    let temp = tempfile::tempdir().unwrap();
    let bundle = temp.path().join("bundle");
    fs::create_dir(&bundle).unwrap();
    let path = bundle.join("credits.bz2");
    compressed(&path, b"first member");
    let mut append = bzip2::write::BzEncoder::new(
        fs::OpenOptions::new().append(true).open(&path).unwrap(),
        bzip2::Compression::best(),
    );
    append.write_all(b"second member").unwrap();
    append.finish().unwrap();
    assert_eq!(
        predictor_cli::pipeline::directory_bytes(&bundle).unwrap(),
        25
    );
    fs::write(&path, b"BZh9truncated").unwrap();
    assert!(predictor_cli::pipeline::directory_bytes(&bundle).is_err());
    let large = temp.path().join("large");
    fs::create_dir(&large).unwrap();
    fs::File::create(large.join("sparse.lpk"))
        .unwrap()
        .set_len(500_000_001)
        .unwrap();
    assert!(predictor_cli::pipeline::directory_bytes(&large)
        .unwrap_err()
        .to_string()
        .contains("limit"));
    let links = temp.path().join("links");
    fs::create_dir(&links).unwrap();
    std::os::unix::fs::symlink(&path, links.join("credits.bz2")).unwrap();
    assert!(predictor_cli::pipeline::directory_bytes(&links).is_err());
}

#[test]
fn compact_credits_preserve_every_record_author_segment_and_audit_binding() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let raw = temp.path().join("raw.tsv");
    let hold = temp.path().join("hold");
    fs::write(&raw,"11\trus\tя словоаа. я словогв.\tanna\t0\t0\n12\trus\tя словогв. я словобё.\tboris\t0\t0\n13\trus\tЯ СЛОВОАА!\tcarol\t0\t0\n14\trus\tещё чай.\tЁж \"author\"\t0\t0\n").unwrap();
    fs::write(&hold, "").unwrap();
    let prepared = temp.path().join("prepared");
    predictor_cli::prepare::prepare(&raw, &prepared, &hold).unwrap();
    let audit = fs::read(prepared.join("attribution.jsonl")).unwrap();
    let database_hash =
        predictor_cli::pipeline::hash_file(&prepared.join("families.sqlite")).unwrap();
    let output = temp.path().join("compact.jsonl.bz2");
    predictor_cli::pipeline::compact_attribution(&prepared, &output).unwrap();
    assert_eq!(fs::read(prepared.join("attribution.jsonl")).unwrap(), audit);
    assert_eq!(
        predictor_cli::pipeline::hash_file(&prepared.join("families.sqlite")).unwrap(),
        database_hash
    );
    let mut expanded = String::new();
    bzip2::read::MultiBzDecoder::new(fs::File::open(output).unwrap())
        .read_to_string(&mut expanded)
        .unwrap();
    let mut lines = expanded.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(
        header["audit_retained"],
        prepared.join("attribution.jsonl").to_str().unwrap()
    );
    assert_eq!(
        header["audit_sha256"],
        predictor_cli::pipeline::hash_file(&prepared.join("attribution.jsonl")).unwrap()
    );
    assert_eq!(header["sources"][0]["license"], "CC-BY-2.0-FR");
    assert_eq!(header["source_ref"], 0);
    assert_eq!(
        header["grouping_version"],
        "duplicate-original-components-v1"
    );
    assert_eq!(header["records"], 6);
    let compact: Vec<serde_json::Value> = lines
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(compact.len(), 6);
    let original: Vec<serde_json::Value> = std::str::from_utf8(&audit)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let component_ids: std::collections::BTreeSet<&str> = original
        .iter()
        .map(|row| row["component"].as_str().unwrap())
        .collect();
    assert_eq!(component_ids.len(), 2);
    for (row, old) in compact.iter().zip(&original) {
        assert_eq!(row[0], old["sentence_id"]);
        assert_eq!(row[1], old["author"]);
        assert_eq!(row[2], old["segment"]);
        assert!(row[3].as_i64().unwrap() > 0);
        assert_eq!(
            row[4],
            component_ids
                .iter()
                .position(|id| *id == old["component"].as_str().unwrap())
                .unwrap()
                + 1
        );
    }
    assert!(
        expanded.len() < audit.len(),
        "compact expanded representation, not bzip2 savings"
    );
}

#[test]
fn legacy_prepare_cannot_be_published_as_component_isolated_data() {
    let temp = tempfile::tempdir().unwrap();
    let prepared = temp.path().join("legacy");
    fs::create_dir(&prepared).unwrap();
    fs::write(prepared.join("report.json"), b"{}").unwrap();
    let destination = temp.path().join("credits.bz2");
    assert!(
        predictor_cli::pipeline::compact_attribution(&prepared, &destination)
            .unwrap_err()
            .to_string()
            .contains("source_old")
    );
    assert!(!destination.exists());
}
