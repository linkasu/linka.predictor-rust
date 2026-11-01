use predictor_cli::prepare::{family, prepare};

#[test]
fn preparation_excludes_holdout_families_and_preserves_duplicate_attribution() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = tmp.path().join("source.tsv");
    let holdout = tmp.path().join("holdout.txt");
    std::fs::write(&raw, "1\trus\tЯ хочу чай!\tanna\t2020\t2020\n2\trus\tя хочу чай.\tboris\t2020\t2020\n3\trus\tНЕ хочу суп.\tanna\t2020\t2020\n4\trus\tнужна вода\t\\N\t2020\t2020\n5\trus\t<unk> слово\tanna\t2020\t2020\n6\trus\tЕщё чай.\tanna\t2020\t2020\n7\trus\tещё чай!\tboris\t2020\t2020\n").unwrap();
    std::fs::write(&holdout, "Я ХОЧУ ЧАЙ?\n").unwrap();
    let out = tmp.path().join("prepared");
    let report = prepare(&raw, &out, &holdout).unwrap();
    assert_eq!(family("Е\u{308}ЩЕ!"), "ёще");
    assert_eq!(report.unique_families, 2);
    assert_eq!(report.holdout_excluded, 2);
    assert_eq!(report.missing_author, 1);
    assert_eq!(report.reserved_markers, 1);
    assert_eq!(report.duplicate_records, 1);
    assert_eq!(report.max_token_bytes, 8);
    assert_eq!(report.oversize_tokens, 0);
    let attribution = std::fs::read_to_string(out.join("attribution.jsonl")).unwrap();
    assert!(attribution.contains("boris"));
    assert!(!attribution.contains("я хочу чай"));
    let all = ["train.txt", "dev.txt", "test.txt"]
        .map(|name| std::fs::read_to_string(out.join(name)).unwrap())
        .join("");
    assert!(all.contains("не хочу суп"));
    assert!(all.contains("ещё чай"));
    assert!(!all.contains("нужна вода"));
    let again = tmp.path().join("again");
    prepare(&raw, &again, &holdout).unwrap();
    for name in [
        "train.txt",
        "dev.txt",
        "test.txt",
        "frequencies.tsv",
        "attribution.jsonl",
    ] {
        assert_eq!(
            std::fs::read(out.join(name)).unwrap(),
            std::fs::read(again.join(name)).unwrap()
        );
    }
}

#[test]
fn every_segment_of_one_original_stays_in_one_split() {
    let temp = tempfile::tempdir().unwrap();
    let raw = temp.path().join("raw.tsv");
    let hold = temp.path().join("hold");
    std::fs::write(
        &raw,
        "4242\trus\tя словоаа. я словогв. я словобё.\tfixture\t0\t0\n",
    )
    .unwrap();
    std::fs::write(&hold, "").unwrap();
    let out = temp.path().join("prepared");
    prepare(&raw, &out, &hold).unwrap();
    let records = std::fs::read_to_string(out.join("attribution.jsonl")).unwrap();
    let splits: std::collections::BTreeSet<String> = records
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["split"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    assert_eq!(
        splits.len(),
        1,
        "one original document is not three independent split groups"
    );
}

#[test]
fn transitive_original_family_components_are_stable_and_keep_all_credits() {
    let temp = tempfile::tempdir().unwrap();
    let hold = temp.path().join("hold");
    std::fs::write(&hold, "").unwrap();
    let lines = [
        "1\trus\tя словоаа. я словогв.\tanna\t0\t0\n",
        "2\trus\tя словогв. я словобё.\tboris\t0\t0\n",
        "3\trus\tЯ СЛОВОАА!\tcarol\t0\t0\n",
    ];
    let raw = temp.path().join("raw.tsv");
    std::fs::write(&raw, lines.concat()).unwrap();
    let out = temp.path().join("prepared");
    let report = prepare(&raw, &out, &hold).unwrap();
    assert_eq!(report.grouping_version, "duplicate-original-components-v1");
    assert_eq!(report.unique_families, 3);
    assert_eq!(report.duplicate_records, 2);
    assert_eq!(report.train, 3);
    assert_eq!(report.dev + report.test, 0);
    assert_eq!(report.independent_groups["train"], 1);
    assert_eq!(
        report.original_overlap + report.component_overlap + report.split_overlap,
        0
    );
    assert!(report.dsu_reserved_bytes <= 64 * 1024 * 1024);
    let records: Vec<serde_json::Value> = std::fs::read_to_string(out.join("attribution.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 5);
    let expected = "4d861ecc5a86ccdc72ca0166e89a04ccf3f58d4b5c43e12e4e8d506e54412d7e";
    assert!(records
        .iter()
        .all(|row| row["component"] == expected && row["split"] == "train"));
    assert!(records.iter().any(|row| row["author"] == "carol"));
    let frequencies = std::fs::read_to_string(out.join("frequencies.tsv")).unwrap();
    assert!(frequencies.lines().any(|line| line == "я\t3"));
    use sha2::{Digest, Sha256};
    for split in ["train", "dev", "test"] {
        let text = std::fs::read_to_string(out.join(format!("{split}.txt"))).unwrap();
        let groups = std::fs::read_to_string(out.join(format!("{split}.groups.tsv"))).unwrap();
        assert_eq!(text.lines().count(), groups.lines().count());
        for (phrase, group) in text.lines().zip(groups.lines()) {
            assert_eq!(
                group,
                format!("{:x}\t{expected}", Sha256::digest(phrase.as_bytes()))
            );
        }
        assert!(report
            .split_sha256
            .contains_key(&format!("{split}.groups.tsv")));
    }
    std::fs::write(&raw, lines.into_iter().rev().collect::<String>()).unwrap();
    let shuffled = temp.path().join("shuffled");
    prepare(&raw, &shuffled, &hold).unwrap();
    for name in [
        "train.txt",
        "dev.txt",
        "test.txt",
        "train.groups.tsv",
        "dev.groups.tsv",
        "test.groups.tsv",
        "attribution.jsonl",
        "frequencies.tsv",
    ] {
        assert_eq!(
            std::fs::read(out.join(name)).unwrap(),
            std::fs::read(shuffled.join(name)).unwrap(),
            "{name}"
        );
    }
    std::fs::write(&hold, "Я СЛОВОГВ!\n").unwrap();
    let excluded = temp.path().join("excluded");
    let filtered = prepare(&raw, &excluded, &hold).unwrap();
    assert_eq!(filtered.holdout_matches, 2);
    assert_eq!(filtered.holdout_related_excluded, 2);
    assert_eq!(filtered.holdout_excluded, 4);
    assert_eq!(filtered.holdout_original_records, 2);
    let admitted: Vec<serde_json::Value> =
        std::fs::read_to_string(excluded.join("attribution.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0]["sentence_id"], "3");
}

#[test]
fn two_hundred_segments_are_one_independent_group_and_yo_is_not_e() {
    let temp = tempfile::tempdir().unwrap();
    let raw = temp.path().join("raw.tsv");
    let hold = temp.path().join("hold");
    std::fs::write(&hold, "").unwrap();
    let segments = (0..200)
        .map(|i| {
            format!(
                "я слово{}{}.",
                char::from_u32(0x430 + i / 20).unwrap(),
                char::from_u32(0x430 + i % 20).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(&raw, format!("4242\trus\t{segments}\tfixture\t0\t0\n")).unwrap();
    let report = prepare(&raw, &temp.path().join("many"), &hold).unwrap();
    assert_eq!(report.unique_families, 200);
    assert_eq!(report.independent_groups.values().sum::<u64>(), 1);
    assert_eq!(report.original_overlap, 0);
    std::fs::write(&raw, "1\trus\tёж\tanna\t0\t0\n2\trus\tеж\tboris\t0\t0\n").unwrap();
    std::fs::write(&hold, "еж\n").unwrap();
    let out = temp.path().join("yo");
    let report = prepare(&raw, &out, &hold).unwrap();
    assert_eq!(report.unique_families, 1);
    assert!(std::fs::read_to_string(out.join("attribution.jsonl"))
        .unwrap()
        .contains("ёж"));
}
