#[test]
fn synthetic_error_slices_and_percentiles_are_explicit() {
    let cases = predictor_cli::diagnostics::errors("я хочу чай");
    for name in [
        "insert",
        "delete",
        "substitute",
        "transpose",
        "split",
        "join",
        "mixed",
    ] {
        assert!(
            cases
                .iter()
                .any(|(kind, q)| kind == name && q != "я хочу чай"),
            "{name}"
        );
    }
    assert_eq!(
        predictor_cli::diagnostics::percentile(&[1, 2, 3, 4, 100], 0.95),
        100
    );
}

#[test]
fn personal_replay_simulates_each_selection_cost_on_fixed_before_after_profiles() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let general = temp.path().join("general.txt");
    let map = general.with_extension("groups.tsv");
    let phrases = ["чай", "чайникотест"];
    let hashes = phrases.map(|q| format!("{:x}", Sha256::digest(q.as_bytes())));
    let component = hashes.iter().min().unwrap();
    std::fs::write(&general, format!("{}\n{}\n", phrases[0], phrases[1])).unwrap();
    std::fs::write(
        &map,
        format!("{}\t{component}\n{}\t{component}\n", hashes[0], hashes[1]),
    )
    .unwrap();
    let event = serde_json::json!({"query":"чайнико","expected":"чайникотест","event":predictor_core::ConfirmedEvent{event_id:1,utterance_id:1,origin:predictor_core::EventOrigin::Confirmation,text:"чайникотест".into()}});
    let events = temp.path().join("events.jsonl");
    std::fs::write(&events, format!("{event}\n")).unwrap();
    let model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/tiny/runtime-v1.lpk");
    let report = predictor_cli::diagnostics::personal_replay(
        &model,
        &events,
        &general,
        &temp.path().join("replay"),
    )
    .unwrap();
    assert_eq!(report["steps"][0]["before"], false);
    assert_eq!(report["steps"][0]["after"], true);
    let costs = report["general_costs"]
        .as_array()
        .expect("explicit cost-1/2/3 sensitivity runs");
    assert_eq!(costs.len(), 3);
    for (index, expected_paid) in [2, 3, 3].into_iter().enumerate() {
        let cost = &costs[index];
        assert_eq!(cost["selection_cost"], index + 1);
        let rows = cost["per_phrase"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let tea = rows.iter().find(|row| row["target"] == "чай").unwrap();
        for phase in ["before", "after"] {
            assert_eq!(tea[phase]["typed"], 3);
            assert_eq!(tea[phase]["paid"], expected_paid);
            assert_eq!(
                tea[phase]["selections"],
                if index == 0 { 1 } else { 0 },
                "cost changes the real action path, not a cost-1 arithmetic rescale"
            );
        }
        let novel = rows
            .iter()
            .find(|row| row["target"] == "чайникотест")
            .unwrap();
        assert!(
            novel["after"]["paid"].as_u64().unwrap() < novel["before"]["paid"].as_u64().unwrap()
        );
    }
    assert_eq!(report["general_mean_ksr_delta"], costs[0]["mean_ksr_delta"]);
    assert_eq!(
        report["general_regression_gate"],
        costs[0]["regression_gate"]
    );
    assert_eq!(report["general_phrase_groups"], 1);
    assert_eq!(report["general_phrase_count"], 2);
    assert_eq!(report["model_hash_unchanged"], true);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(temp.path().join("replay/replay.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn pipeline_evaluation_keeps_components_instead_of_flattening_for_ci() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("dev.txt");
    let map = temp.path().join("dev.groups.tsv");
    let phrases = ["я хочу чай", "я хочу пить"];
    let hashes = phrases.map(|q| format!("{:x}", Sha256::digest(q.as_bytes())));
    let component = hashes.iter().min().unwrap();
    std::fs::write(&input, format!("{}\n{}\n", phrases[0], phrases[1])).unwrap();
    std::fs::write(
        &map,
        format!("{}\t{component}\n{}\t{component}\n", hashes[0], hashes[1]),
    )
    .unwrap();
    let model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/tiny/runtime-v1.lpk");
    let report = predictor_cli::pipeline::evaluate_stage(&model, &input, 1).unwrap();
    assert_eq!(report["phrase_groups"], 1);
    assert_eq!(report["phrase_count"], 2);
    assert_eq!(report["grouping_mode"], "component_sidecar");
    assert_eq!(
        report["grouping_version"],
        "duplicate-original-components-v1"
    );
    assert_eq!(
        report["selection_protocol"],
        predictor_cli::evaluate::SELECTION_PROTOCOL
    );
    assert_eq!(
        report["group_map_sha256"],
        predictor_cli::pipeline::hash_file(&map).unwrap()
    );
    assert_eq!(report["maximum_groups"], 1);
}
