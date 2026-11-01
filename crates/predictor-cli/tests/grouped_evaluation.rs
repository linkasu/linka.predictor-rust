use predictor_cli::evaluate;
use predictor_core::{Model, PackageMetadata};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn row(phrase: &str, group: &str) -> String {
    format!(
        "{}\t{}\n",
        hash(&predictor_cli::prepare::family(phrase)),
        hash(group)
    )
}
fn model(directory: &Path, counts: bool) -> std::path::PathBuf {
    let path = directory.join("model.lpk");
    let arpa = directory.join("fixture.arpa");
    fs::write(&arpa,"\\data\\\nngram 1=6\nngram 2=2\n\\1-grams:\n-3 <unk>\n0 <s> 0\n-1 </s>\n-0.5 я 0\n-0.7 хочу 0\n-0.9 чай\n\\2-grams:\n-0.1 <s> я\n-0.1 я хочу\n\\end\\\n").unwrap();
    let mut metadata = PackageMetadata::default();
    if counts {
        metadata.word_counts = std::collections::BTreeMap::from([
            ("я".into(), 7),
            ("хочу".into(), 3),
            ("чай".into(), 2),
        ]);
    }
    Model::compile_arpa(arpa, &path, &metadata).unwrap();
    path
}

#[test]
fn component_selection_retains_all_segments_and_validates_every_map_row() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("dev.txt");
    let mut phrases = String::new();
    let mut map = String::new();
    for _ in 0..200 {
        phrases.push_str("я\n");
        map.push_str(&row("я", "one-original"));
    }
    fs::write(&input, &phrases).unwrap();
    fs::write(input.with_extension("groups.tsv"), &map).unwrap();
    let selected = evaluate::read_phrases(&input, 1).unwrap();
    assert_eq!(
        selected.len(),
        200,
        "one component retains all 200 segments"
    );
    let grouped = evaluate::read_grouped_phrases(&input, 1).unwrap();
    assert_eq!(grouped.group_count(), 1);
    assert_eq!(grouped.grouping_version, "duplicate-original-components-v1");
    let report = evaluate::evaluate_grouped(&model(dir.path(), false), &grouped).unwrap();
    assert_eq!(report["phrase_groups"], 1);
    assert_eq!(report["phrase_count"], 200);
    assert_eq!(report["quality_gate"]["minimum_groups"], false);
    map.push_str(&row("я", "extra"));
    fs::write(input.with_extension("groups.tsv"), map).unwrap();
    assert!(
        evaluate::read_grouped_phrases(&input, 1).is_err(),
        "unused trailing rows must fail"
    );
}

#[test]
fn paired_cluster_resample_keeps_unweighted_phrase_mean() {
    let groups = [(20.0, 200usize), (-0.5, 1usize)];
    assert!(
        (evaluate::resampled_phrase_mean(&groups, &[0, 1]).unwrap() - 19.5 / 201.0).abs() < 1e-12
    );
    assert!((evaluate::resampled_phrase_mean(&groups, &[0, 0]).unwrap() - 0.1).abs() < 1e-12);
    assert_eq!(
        evaluate::resampled_phrase_mean(&groups, &[1, 1]).unwrap(),
        -0.5
    );
    assert_eq!(
        evaluate::paired_cluster_interval(&groups, 10000).unwrap(),
        [-0.5, 0.1]
    );
    let four = [(20.0, 200usize), (-0.5, 1), (-0.5, 1), (-0.5, 1)];
    let ci = evaluate::paired_cluster_interval(&four, 10000).unwrap();
    assert_eq!(ci[0], -0.5);
    assert!(
        (ci[1] - 59.5 / 601.0).abs() < 1e-12,
        "upper tail must weight phrases, not component means: {ci:?}"
    );
}

#[test]
fn whole_file_lowest_component_selection_and_frozen_protocol_binding() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("dev.txt");
    let raw = "я\nхочу\nчай\n";
    let map = format!("{}{}{}", row("я", "a"), row("хочу", "b"), row("чай", "b"));
    fs::write(&input, raw).unwrap();
    fs::write(input.with_extension("groups.tsv"), &map).unwrap();
    let grouped = evaluate::read_grouped_phrases(&input, 1).unwrap();
    assert_eq!(grouped.phrases, ["хочу", "чай"]);
    assert_eq!(grouped.group_count(), 1);
    assert_eq!(grouped.group_map_sha256, Some(hash(&map)));
    let frozen = serde_json::json!({"maximum_groups":1,"selection_protocol":evaluate::SELECTION_PROTOCOL,"grouping_version":evaluate::GROUPING_VERSION,"grouping_mode":"component_sidecar","group_map_sha256":hash(&map)});
    evaluate::validate_frozen_selection(&frozen, &grouped).unwrap();
    for (field, value) in [
        ("maximum_groups", serde_json::json!(2)),
        ("selection_protocol", serde_json::json!("old-lowest-phrase")),
        ("grouping_version", serde_json::json!("old")),
        ("grouping_mode", serde_json::json!("fixture")),
        ("group_map_sha256", serde_json::Value::Null),
    ] {
        let mut bad = frozen.clone();
        bad[field] = value;
        assert!(
            evaluate::validate_frozen_selection(&bad, &grouped).is_err(),
            "{field}"
        );
    }
    fs::write(&input, "чай\nя\nхочу\n").unwrap();
    fs::write(
        input.with_extension("groups.tsv"),
        format!("{}{}{}", row("чай", "b"), row("я", "a"), row("хочу", "b")),
    )
    .unwrap();
    let reordered = evaluate::read_grouped_phrases(&input, 1).unwrap();
    assert_eq!(reordered.phrases, grouped.phrases);
    assert_ne!(reordered.group_map_sha256, grouped.group_map_sha256);
    assert!(evaluate::validate_frozen_selection(&frozen, &reordered).is_err());
    fs::write(
        input.with_extension("groups.tsv"),
        format!(
            "{}{}{}",
            row("чай", "b"),
            row("wrong unselected phrase", "a"),
            row("хочу", "b")
        ),
    )
    .unwrap();
    assert!(
        evaluate::read_grouped_phrases(&input, 1).is_err(),
        "must validate unselected rows too"
    );
}

#[test]
fn frequency_qualification_requires_complete_embedded_count_binding_and_all_clean_modes() {
    let dir = tempfile::tempdir().unwrap();
    let path = model(dir.path(), false);
    fs::write(dir.path().join("frequencies.tsv"), "").unwrap();
    let report = evaluate::evaluate(&path, &["я хочу чай".into()]).unwrap();
    assert_eq!(report["frequency_counts_binding"]["qualified"], false);
    assert_eq!(
        report["frequency_counts_binding"]["positive_embedded_records"],
        0
    );
    assert_eq!(report["quality_gate"]["contextual_gain"], false);
    for costs in report["per_phrase"][0]["costs"].as_array().unwrap() {
        assert!(costs["letters_only"]["ksr"].is_number());
        assert!(costs["spaces_only"]["ksr"].is_number());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = model(dir.path(), true);
    fs::write(
        dir.path().join("frequencies.tsv"),
        "хочу\t3\nчай\t2\nя\t7\n",
    )
    .unwrap();
    let report = evaluate::evaluate(&path, &["я хочу чай".into()]).unwrap();
    assert_eq!(report["frequency_counts_binding"]["qualified"], true);
    assert_eq!(
        report["frequency_counts_binding"]["sidecar_sha256"],
        report["frequency_counts_binding"]["embedded_canonical_counts_sha256"]
    );
    assert_eq!(
        report["frequency_counts_binding"]["model_sha256"],
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))
    );
    for invalid in ["хочу\t4\nчай\t2\nя\t7\n", "я\t7\nхочу\t3\nчай\t2\n"] {
        fs::write(dir.path().join("frequencies.tsv"), invalid).unwrap();
        let report = evaluate::evaluate(&path, &["я хочу чай".into()]).unwrap();
        assert_eq!(
            report["frequency_counts_binding"]["qualified"], false,
            "{invalid}"
        );
    }
    fs::write(dir.path().join("frequencies.tsv"), "хочу\t3\nя\t7\n").unwrap();
    let report = evaluate::evaluate(&path, &["я хочу чай".into()]).unwrap();
    assert_eq!(report["frequency_counts_binding"]["qualified"], false);
}

#[test]
fn partial_oracle_never_certifies_candidate_recall() {
    let dir = tempfile::tempdir().unwrap();
    let model = Model::load(model(dir.path(), true)).unwrap();
    let predicted = Vec::new();
    let snapshot = evaluate::exact_candidate_oracle(&model, "я", &predicted, 1, 100).unwrap();
    assert!(snapshot.candidate_cap_truncated);
    assert!(snapshot.certified_space_coverage().is_none());
    let snapshot = evaluate::exact_candidate_oracle(&model, "я", &predicted, 100, 1).unwrap();
    assert!(snapshot.operation_truncated);
    assert!(snapshot.certified_space_coverage().is_none());
    let snapshot = evaluate::exact_candidate_oracle(&model, "я", &predicted, 100, 100).unwrap();
    assert!(!snapshot.candidate_cap_truncated && !snapshot.operation_truncated);
    assert!(snapshot.denominator > 0);
    assert_eq!(snapshot.certified_space_coverage(), Some(0.0));
    let predictor = predictor_core::Predictor::load(dir.path().join("model.lpk"))
        .unwrap()
        .with_config(predictor_core::SearchConfig {
            letters: false,
            spaces: false,
            external_lexicon: false,
            ..Default::default()
        })
        .unwrap();
    for query in ["я", "Я", "хо", "я х", "я хочу", "я\nх"] {
        let predicted = predictor
            .explain(predictor_core::Request::new(query), None)
            .unwrap();
        let snapshot =
            evaluate::exact_candidate_oracle(&model, query, &predicted.pool, 100, 100).unwrap();
        assert_eq!(snapshot.certified_space_coverage(), Some(1.0), "{query}");
    }
}

#[test]
fn count_qualification_requires_every_ordinary_nonexternal_lm_word() {
    for complete in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = model(dir.path(), false);
        let mut counts = std::collections::BTreeMap::from([("я".to_owned(), 7)]);
        let sidecar = if complete {
            counts.insert("хочу".into(), 3);
            counts.insert("чай".into(), 2);
            "хочу\t3\nчай\t2\nя\t7\n"
        } else {
            "я\t7\n"
        };
        Model::compile_arpa(
            dir.path().join("fixture.arpa"),
            &path,
            &PackageMetadata {
                word_counts: counts,
                external_words: vec!["зинаида".into()],
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(dir.path().join("frequencies.tsv"), sidecar).unwrap();
        let report = evaluate::evaluate(&path, &["я хочу чай".into()]).unwrap();
        let binding = &report["frequency_counts_binding"];
        assert_eq!(
            binding["qualified"], complete,
            "complete={complete}: {binding}"
        );
        assert_eq!(binding["ordinary_lm_records"], 3);
        assert_eq!(
            binding["zero_count_ordinary_lm_records"],
            if complete { 0 } else { 2 }
        );
        let loaded = Model::load(path).unwrap();
        for exempt in ["<s>", "</s>", "<unk>", "зинаида"] {
            let (_, count, external) = loaded
                .vocabulary()
                .find(|(word, _, _)| *word == exempt)
                .unwrap();
            assert_eq!(count, 0);
            assert_eq!(external, exempt == "зинаида");
        }
        println!("synthetic count coverage complete={complete}: {binding}");
    }
}

#[test]
fn all_five_clean_modes_hold_external_dictionary_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = model(dir.path(), true);
    Model::compile_arpa(
        dir.path().join("fixture.arpa"),
        &path,
        &PackageMetadata {
            external_words: vec!["зинаида".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let target = "я зинаида";
    let dictionary_on = predictor_core::Predictor::load(&path).unwrap();
    let synthetic_on = evaluate::simulate(target, 1, |q| {
        dictionary_on.complete(predictor_core::Request::new(q), None)
    })
    .unwrap();
    assert_eq!(
        synthetic_on.paid, 2,
        "synthetic fixture must expose the external dictionary confound"
    );
    let report = evaluate::evaluate(&path, &[target.into()]).unwrap();
    for cost in report["per_phrase"][0]["costs"].as_array().unwrap() {
        let expected = cost["contextual"]["paid"].as_u64().unwrap();
        assert_eq!(expected, 9);
        for mode in [
            "frequency",
            "contextual",
            "letters_only",
            "spaces_only",
            "corrections",
        ] {
            assert_eq!(
                cost[mode]["paid"], expected,
                "selection_cost={} mode={mode}: {cost}",
                cost["selection_cost"]
            );
        }
    }
    println!("synthetic external-name positive control dictionary_on_paid=2; all five clean modes dictionary_off_paid=9 at costs1/2/3; not a real OpenCorpora ablation");
}

#[test]
fn only_the_three_exact_reserved_markers_are_count_exempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = model(dir.path(), false);
    let arpa = dir.path().join("fixture.arpa");
    let text = fs::read_to_string(&arpa)
        .unwrap()
        .replace("ngram 1=6", "ngram 1=7")
        .replace("-0.9 чай\n", "-0.9 чай\n-2 <other>\n");
    fs::write(&arpa, text).unwrap();
    Model::compile_arpa(
        arpa,
        &path,
        &PackageMetadata {
            word_counts: std::collections::BTreeMap::from([
                ("я".into(), 7),
                ("хочу".into(), 3),
                ("чай".into(), 2),
            ]),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(
        dir.path().join("frequencies.tsv"),
        "хочу\t3\nчай\t2\nя\t7\n",
    )
    .unwrap();
    let report = evaluate::evaluate(&path, &["я".into()]).unwrap();
    assert_eq!(report["frequency_counts_binding"]["qualified"], false);
    assert_eq!(report["frequency_counts_binding"]["ordinary_lm_records"], 4);
    assert_eq!(
        report["frequency_counts_binding"]["zero_count_ordinary_lm_records"],
        1
    );
}

fn gold_metric_model(directory: &Path) -> std::path::PathBuf {
    let arpa = directory.join("gold.arpa");
    let package = directory.join("gold.lpk");
    let mut text =
        "\\data\\\nngram 1=12\n\\1-grams:\n-3 <unk>\n0 <s>\n-1 </s>\n-0.05 я\n".to_owned();
    for (index, word) in [
        "абаа", "абаб", "абав", "абаг", "абад", "абае", "абаж", "абаз",
    ]
    .iter()
    .enumerate()
    {
        text.push_str(&format!("-0.{} {word}\n", index + 1));
    }
    text.push_str("\\end\\\n");
    fs::write(&arpa, text).unwrap();
    Model::compile_arpa(arpa, &package, &PackageMetadata::default()).unwrap();
    package
}

#[test]
fn clean_gold_before_rank_is_distinct_from_space_coverage_and_top5() {
    let dir = tempfile::tempdir().unwrap();
    let path = gold_metric_model(dir.path());
    let report = evaluate::evaluate(&path, &["я абаз".into()]).unwrap();
    assert_eq!(report["prefix_completion"]["denominator"], 1);
    assert_eq!(report["next_word"]["denominator"], 1);
    assert_eq!(report["prefix_completion"]["hits"], 0);
    assert_eq!(report["next_word"]["top5_hits"], 0);
    for slice in ["prefix_completion", "next_word"] {
        let gold = &report["clean_gold_pre_rank"][slice];
        assert_eq!(gold["total_queries"], 1, "{slice}: {report}");
        assert_eq!(
            gold["pool_hits"], 1,
            "gold must be found before ranking for {slice}"
        );
        assert_eq!(gold["fullsearch_gold_available"], 1);
        assert_eq!(gold["pool_hits_given_oracle_available"], 1);
        assert_eq!(
            gold["retrieval_recall_given_fullsearch_gold_available"],
            1.0
        );
        assert_eq!(gold["oracle_inconclusive"], 0);
    }
    assert!(
        report.get("candidate_pool_oracle").is_none(),
        "old ambiguous metric key must be replaced"
    );
    assert_eq!(report["candidate_space_coverage"]["coverage"], 1.0);
    assert!(report["candidate_space_coverage"].get("recall").is_none());
}

#[test]
fn partial_gold_oracle_cannot_certify_conditional_retrieval() {
    let dir = tempfile::tempdir().unwrap();
    let path = gold_metric_model(dir.path());
    let predictor = predictor_core::Predictor::load(path)
        .unwrap()
        .with_config(predictor_core::SearchConfig {
            letters: false,
            spaces: false,
            external_lexicon: false,
            ..Default::default()
        })
        .unwrap();
    let q = "я ";
    let intended = "я абаз";
    let predicted = predictor
        .explain(predictor_core::Request::new(q), None)
        .unwrap();
    for (candidate_cap, operation_cap) in [(1, 100), (100, 1)] {
        let oracle = evaluate::exact_candidate_oracle_with_gold(
            predictor.model(),
            q,
            &predicted.pool,
            candidate_cap,
            operation_cap,
            intended,
        )
        .unwrap();
        assert!(oracle.certified_gold_available().is_none());
        let mut metrics = evaluate::CleanGoldMetrics::default();
        metrics
            .observe(q, intended, &predicted.pool, &oracle)
            .unwrap();
        let report = metrics.report();
        assert_eq!(report["total_queries"], 1);
        assert_eq!(report["pool_hits"], 1);
        assert_eq!(report["oracle_inconclusive"], 1);
        for field in [
            "fullsearch_gold_available",
            "fullsearch_gold_unavailable",
            "pool_hits_given_oracle_available",
            "retrieval_recall_given_fullsearch_gold_available",
        ] {
            assert!(report[field].is_null(), "{field}: {report}");
        }
    }
}

#[test]
fn absent_lexical_gold_is_unavailable_not_retrieval_success_and_rank_is_bounded_by_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = gold_metric_model(dir.path());
    let absent = evaluate::evaluate(&path, &["я озеро".into()]).unwrap();
    for slice in ["prefix_completion", "next_word"] {
        let gold = &absent["clean_gold_pre_rank"][slice];
        assert_eq!(gold["total_queries"], 1);
        assert_eq!(gold["pool_hits"], 0);
        assert_eq!(gold["fullsearch_gold_available"], 0);
        assert_eq!(gold["fullsearch_gold_unavailable"], 1);
        assert_eq!(gold["pool_hits_given_oracle_available"], 0);
        assert_eq!(gold["oracle_inconclusive"], 0);
        assert!(
            gold["retrieval_recall_given_fullsearch_gold_available"].is_null(),
            "OOV must not become perfect retrieval: {gold}"
        );
    }
    let report =
        evaluate::evaluate(&path, &["я абаз".into(), "я абаа".into(), "я озеро".into()]).unwrap();
    let prefix_pool = report["clean_gold_pre_rank"]["prefix_completion"]["pool_hits"]
        .as_u64()
        .unwrap();
    let next_pool = report["clean_gold_pre_rank"]["next_word"]["pool_hits"]
        .as_u64()
        .unwrap();
    assert!(report["prefix_completion"]["hits"].as_u64().unwrap() <= prefix_pool);
    assert!(report["next_word"]["top5_hits"].as_u64().unwrap() <= next_pool);
    assert!(report["next_word"]["top1_hits"].as_u64().unwrap() <= next_pool);
    for slice in ["prefix_completion", "next_word"] {
        assert_eq!(
            report[slice]["denominator"],
            report["clean_gold_pre_rank"][slice]["total_queries"]
        );
    }
}

#[test]
fn high_candidate_space_coverage_does_not_hide_a_retrieval_miss() {
    let dir = tempfile::tempdir().unwrap();
    let arpa = dir.path().join("retrieval.arpa");
    let package = dir.path().join("retrieval.lpk");
    let alphabet: Vec<_> = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя".chars().collect();
    let mut text = "\\data\\\nngram 1=132\n\\1-grams:\n-4 <unk>\n0 <s>\n-1 </s>\n".to_owned();
    let mut gold = String::new();
    for i in 0..129 {
        let word = format!(
            "а{}{}{}",
            alphabet[i / 1089],
            alphabet[i / 33 % 33],
            alphabet[i % 33]
        );
        text.push_str(&format!("{} {word}\n", -(i as f64 + 1.0) / 100.0));
        gold = word;
    }
    text.push_str("\\end\\\n");
    fs::write(&arpa, text).unwrap();
    Model::compile_arpa(arpa, &package, &PackageMetadata::default()).unwrap();
    let report = evaluate::evaluate(&package, &[gold]).unwrap();
    let wanted = &report["clean_gold_pre_rank"]["prefix_completion"];
    assert_eq!(wanted["total_queries"], 1);
    assert_eq!(wanted["pool_hits"], 0);
    assert_eq!(wanted["fullsearch_gold_available"], 1);
    assert_eq!(
        wanted["retrieval_recall_given_fullsearch_gold_available"],
        0.0
    );
    assert_eq!(report["candidate_space_coverage"]["denominator"], 129);
    assert_eq!(report["candidate_space_coverage"]["hits"], 128);
    assert!(
        (report["candidate_space_coverage"]["coverage"]
            .as_f64()
            .unwrap()
            - 128.0 / 129.0)
            .abs()
            < 1e-12
    );
}
