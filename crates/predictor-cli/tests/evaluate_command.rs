#[test]
fn evaluator_serializes_real_runtime_responses_and_publishes_relative_output() {
    let temporary = tempfile::tempdir().unwrap();
    let arpa =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny/model.arpa");
    predictor_core::Model::compile_arpa(
        arpa,
        temporary.path().join("model.lpk"),
        &Default::default(),
    )
    .unwrap();
    std::fs::write(temporary.path().join("dev.txt"), "я хочу чай\n").unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_lp"))
        .current_dir(temporary.path())
        .args(["evaluate", "model.lpk", "dev.txt", "metrics.json", "1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(temporary.path().join("metrics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["phrase_groups"], 1);
    assert_eq!(
        report["per_phrase"][0]["costs"].as_array().unwrap().len(),
        3
    );
    assert_eq!(report["bootstrap"]["resamples"], 10000);
    assert_eq!(report["quality_gate"]["complete"], false);
    assert!(
        report["synthetic_corrections"]["slices"]["join"]["denominator"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn unknown_trailing_flags_are_rejected_before_any_pipeline_work() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_lp"))
        .args(["run", "missing-config.json", "must-not-start", "--dry-run"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unexpected argument count"));
}

#[test]
fn invalid_frozen_limits_do_not_claim_final_evaluation() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(temporary.path().join("test.txt"), "я хочу чай\n").unwrap();
    std::fs::write(
        temporary.path().join("frozen.json"),
        r#"{"maximum_groups":250}"#,
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_lp"))
        .current_dir(temporary.path())
        .args([
            "evaluate",
            "missing.lpk",
            "test.txt",
            "metrics.json",
            "1",
            "frozen.json",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("frozen manifest mismatch: maximum_groups"));
    assert!(
        !temporary
            .path()
            .join("frozen.evaluation-claim.json")
            .exists(),
        "invalid frozen protocol must be rejected BEFORE exclusive claim"
    );
}

#[test]
fn personal_replay_rejects_empty_events_and_learns_only_after_confirmation() {
    let temporary = tempfile::tempdir().unwrap();
    let arpa =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny/model.arpa");
    predictor_core::Model::compile_arpa(
        arpa,
        temporary.path().join("model.lpk"),
        &Default::default(),
    )
    .unwrap();
    std::fs::write(temporary.path().join("general.txt"), "я хочу чай\n").unwrap();
    std::fs::write(temporary.path().join("empty.jsonl"), "").unwrap();
    let empty = std::process::Command::new(env!("CARGO_BIN_EXE_lp"))
        .current_dir(temporary.path())
        .args([
            "personal-replay",
            "model.lpk",
            "empty.jsonl",
            "general.txt",
            "empty-replay",
        ])
        .output()
        .unwrap();
    assert!(
        !empty.status.success(),
        "empty replay must not look like completed evidence"
    );
    let row = serde_json::json!({"query":"чайнико","expected":"чайникотест","event":predictor_core::ConfirmedEvent{event_id:1,utterance_id:1,origin:predictor_core::EventOrigin::Confirmation,text:"чайникотест".into()}});
    std::fs::write(
        temporary.path().join("events.jsonl"),
        format!("{row}\n{row}\n"),
    )
    .unwrap();
    let replay = std::process::Command::new(env!("CARGO_BIN_EXE_lp"))
        .current_dir(temporary.path())
        .args([
            "personal-replay",
            "model.lpk",
            "events.jsonl",
            "general.txt",
            "causal-replay",
        ])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let report: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(temporary.path().join("causal-replay/replay.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["steps"][0]["before"], false);
    assert_eq!(report["steps"][0]["after"], true);
    assert_eq!(report["steps"][0]["learned"], true);
    assert_eq!(report["steps"][1]["learned"], false);
    assert_eq!(report["model_hash_unchanged"], true);
    assert_eq!(report["reopened_words"], 1);
}
