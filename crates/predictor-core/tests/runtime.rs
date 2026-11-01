use predictor_core::*;
use std::fs;

// Hand-authored arithmetic fixture, not locally trained or a KenLM reference result.
const ARPA: &str = "\\data\\\nngram 1=10\nngram 2=4\nngram 3=1\n\n\\1-grams:\n-3\t<unk>\t0\n-1\t<s>\t-0.2\n-1\t</s>\n-0.2\tя\t-0.3\n-0.4\tхочу\t-0.4\n-0.6\tесть\t-0.1\n-0.8\tпривет\t0\n-1.1\tветер\t0\n-1.2\tпри\t0\n-1.4\tеда\t0\n\n\\2-grams:\n-0.1\t<s> я\t-0.1\n-0.2\tя хочу\t-0.2\n-0.1\tхочу есть\t-0.1\n-0.3\tесть </s>\n\n\\3-grams:\n-0.05\tя хочу есть\n\n\\end\\\n";

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let arpa = dir.path().join("tiny.arpa");
    let package = dir.path().join("model.lpk");
    fs::write(&arpa, ARPA).unwrap();
    Model::compile_arpa(arpa, &package, &PackageMetadata::default()).unwrap();
    (dir, package)
}

#[test]
fn test_package_backoff_and_prefix_oracle() {
    let (_dir, path) = fixture();
    let model = Model::load(&path).unwrap();
    assert!((model.score(&["я", "хочу"], "есть") + 0.05).abs() < 1e-6);
    assert!((model.score(&["я", "хочу"], "привет") + 1.4).abs() < 1e-6);
    assert!((model.score(&["я"], "несуществующее") + 3.3).abs() < 1e-6);
    // -0.1 + (-0.1 -0.2) -0.05 + (-0.1 -0.3), including missing trigram backoffs.
    assert!((model.score_sentence(&["я", "хочу", "есть"]) + 0.85).abs() < 1e-6);
    for p in ["", "е", "пр", "я", "zz"] {
        assert_eq!(model.prefix_top(p, 100), model.prefix_top_full_scan(p, 100));
    }
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = 0;
    fs::write(&path, bytes).unwrap();
    assert!(Model::load(&path).is_err());
}

#[test]
fn test_query_corrections_and_personal_causality() {
    let (_dir, path) = fixture();
    let predictor = Predictor::load(&path).unwrap();
    assert!(!predictor
        .explain(Request::new("зин"), None)
        .unwrap()
        .pool
        .iter()
        .any(|c| c.edit.apply("зин").unwrap() == "зинаида"));
    assert!(predictor
        .complete(Request::new(""), None)
        .unwrap()
        .text
        .is_empty());
    for (q, expected) in [
        ("пр", "привет"),
        ("првиет", "привет"),
        ("при вет", "привет"),
        ("хочуес", "хочу есть"),
        ("хочеус", "хочу есть"),
    ] {
        let completion = predictor
            .explain(
                Request {
                    q,
                    lang: "ru",
                    limit: 100,
                },
                None,
            )
            .unwrap();
        assert!(
            completion
                .candidates
                .iter()
                .any(|c| c.edit.apply(q).unwrap() == expected),
            "{q}: {:?}",
            completion.response
        );
    }
    assert!(predictor
        .complete(
            Request {
                q: "я",
                lang: "en",
                limit: 5
            },
            None
        )
        .is_err());
    let mut personal = PersonalModel::default();
    let event = ConfirmedEvent {
        event_id: 1,
        utterance_id: 9,
        origin: EventOrigin::Selection,
        text: "Зинаида".into(),
    };
    assert!(personal.observe(event.clone()).unwrap());
    assert!(!personal.observe(event.clone()).unwrap());
    let mut duplicate = event.clone();
    duplicate.event_id = 2;
    personal.observe(duplicate).unwrap();
    let mut confirmation = event;
    confirmation.event_id = 3;
    confirmation.origin = EventOrigin::Confirmation;
    personal.observe(confirmation).unwrap();
    let (_, counts) = personal.words().next().unwrap();
    assert_eq!((counts.confirmed_count, counts.selection_count), (1, 1));
    let before = serde_json::to_vec(&personal).unwrap();
    let personalized = predictor
        .explain(Request::new("зин"), Some(&personal))
        .unwrap();
    assert!(personalized
        .pool
        .iter()
        .any(|c| c.edit.apply("зин").unwrap() == "Зинаида"));
    assert_eq!(before, serde_json::to_vec(&personal).unwrap());
    let personal_path = path.with_extension("personal");
    personal.save(&personal_path).unwrap();
    let loaded = PersonalModel::load(personal_path).unwrap();
    assert_eq!(before, serde_json::to_vec(&loaded).unwrap());
}

#[test]
fn test_confirmed_personal_surface_survives_restart() {
    let (_dir, path) = fixture();
    let predictor = Predictor::load(&path).unwrap();
    let mut personal = PersonalModel::default();
    let event = ConfirmedEvent {
        event_id: 101,
        utterance_id: 101,
        origin: EventOrigin::Confirmation,
        text: "позови Зюмарика".into(),
    };
    personal.observe(event.clone()).unwrap();
    let store = path.with_extension("personal");
    personal.save(&store).unwrap();
    let mut loaded = PersonalModel::load(&store).unwrap();
    assert!(!loaded.observe(event).unwrap());
    for q in ["позови Зюм", "позови зюм"] {
        let completion = predictor.explain(Request::new(q), Some(&loaded)).unwrap();
        assert!(completion
            .candidates
            .iter()
            .any(|c| c.edit.apply(q).unwrap() == "позови Зюмарика"));
    }
    // Old profiles lack surface forms and remain readable with normalized fallback.
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&store).unwrap()).unwrap();
    for counts in old["words"].as_object_mut().unwrap().values_mut() {
        counts.as_object_mut().unwrap().remove("surface");
    }
    fs::write(&store, serde_json::to_vec(&old).unwrap()).unwrap();
    let legacy = PersonalModel::load(&store).unwrap();
    assert!(predictor
        .explain(Request::new("позови зюм"), Some(&legacy))
        .unwrap()
        .candidates
        .iter()
        .any(|c| c.edit.apply("позови зюм").unwrap() == "позови зюмарика"));
    old["words"]["зюмарика"]["surface"] = serde_json::json!("другое");
    fs::write(&store, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(PersonalModel::load(&store).is_err());
}

#[test]
fn test_count_prefix_and_external_ablation() {
    let dir = tempfile::tempdir().unwrap();
    let arpa = dir.path().join("model.arpa");
    let package = dir.path().join("model.lpk");
    fs::write(&arpa, ARPA).unwrap();
    let mut metadata = PackageMetadata {
        external_words: vec!["ежевика".into()],
        ..Default::default()
    };
    metadata.word_counts.insert("еда".into(), 1000);
    metadata.word_counts.insert("есть".into(), 2);
    Model::compile_arpa(arpa, &package, &metadata).unwrap();
    let model = Model::load(&package).unwrap();
    assert_eq!(model.prefix_top("е", 1), vec!["еда"]);
    assert!(model.frequency_score("еда") > model.frequency_score("есть"));
    assert!(
        (model.frequency_score("еда") - ((1000.0f64 + 1.0) / (1002.0 + 11.0)).log10()).abs()
            < 1e-10
    );
    for prefix in ["", "е", "еж", "<", "я", "пр", "яа"] {
        for limit in 0..20 {
            assert_eq!(
                model.prefix_top(prefix, limit),
                model.prefix_top_full_scan(prefix, limit)
            );
        }
    }
    let predictor = Predictor::load(&package)
        .unwrap()
        .with_config(SearchConfig {
            external_lexicon: false,
            letters: false,
            spaces: false,
            ..Default::default()
        })
        .unwrap();
    assert!(predictor
        .complete(Request::new("еж"), None)
        .unwrap()
        .text
        .is_empty());
    let predictor = Predictor::load(package)
        .unwrap()
        .with_config(SearchConfig {
            letters: false,
            spaces: false,
            ..Default::default()
        })
        .unwrap();
    assert!(predictor
        .complete(Request::new("еж"), None)
        .unwrap()
        .text
        .contains(&"ежевика".into()));
}

#[test]
fn test_package_mutations_never_panic() {
    let (_dir, path) = fixture();
    let bytes = fs::read(&path).unwrap();
    for cut in [0, 1, 31, 32, 100, bytes.len() - 1] {
        let corrupt = path.with_extension("bad");
        fs::write(&corrupt, &bytes[..cut]).unwrap();
        assert!(Model::load(corrupt).is_err());
    }
    for position in [8, 12, 16, 20, 24, 28, 40, 48, 60, bytes.len() - 1] {
        let mut bad = bytes.clone();
        bad[position] ^= 255;
        let corrupt = path.with_extension("bad");
        fs::write(&corrupt, bad).unwrap();
        assert!(Model::load(corrupt).is_err());
    }
}

#[test]
fn test_search_modes_limits_and_all_letter_edits() {
    let (_dir, path) = fixture();
    let predictor = Predictor::load(&path).unwrap();
    for query in [
        "првет",
        "приивет",
        "превет",
        "првиет",
        "хочуесть",
        "хочуес",
        "при вет",
        "пре вет",
    ] {
        let response = predictor
            .explain(
                Request {
                    q: query,
                    lang: "ru",
                    limit: 100,
                },
                None,
            )
            .unwrap();
        let target = if query.starts_with("хочу") {
            "хочу есть"
        } else {
            "привет"
        };
        assert!(
            response
                .pool
                .iter()
                .any(|c| c.edit.apply(query).unwrap() == target),
            "{query}"
        );
    }
    for request in [
        Request {
            q: "я",
            lang: "ru",
            limit: 0,
        },
        Request {
            q: "я",
            lang: "ru",
            limit: 101,
        },
    ] {
        assert!(predictor.complete(request, None).is_err());
    }
    assert!(predictor
        .complete(Request::new(&"я".repeat(8193)), None)
        .is_err());
    let exact = Predictor::load(&path)
        .unwrap()
        .with_config(SearchConfig {
            letters: false,
            spaces: false,
            ..Default::default()
        })
        .unwrap();
    assert!(exact
        .complete(Request::new("првиет"), None)
        .unwrap()
        .text
        .is_empty());
    let bounded = Predictor::load(&path)
        .unwrap()
        .with_config(SearchConfig {
            max_operations: 2,
            ..Default::default()
        })
        .unwrap();
    let result = bounded.explain(Request::new("пр"), None).unwrap();
    assert!(result.truncated);
    assert!(!result.response.text.is_empty());
    let oracle = Predictor::load(&path)
        .unwrap()
        .with_config(SearchConfig {
            full_scan_oracle: true,
            ..Default::default()
        })
        .unwrap();
    let q = "я хочу ";
    let bounded = predictor.explain(Request::new(q), None).unwrap();
    let full = oracle.explain(Request::new(q), None).unwrap();
    assert_eq!(bounded.response, full.response);
}

#[test]
fn test_parent_independent_projection_goldens() {
    // Arithmetic from docs/independent-golden-cases.md; reference does not call apply/project.
    let cases = vec![
        (
            "при вет",
            vec![(0, "привет"), (7, "ветер")],
            -7,
            vec!["привет", "при ветер"],
            vec!["привет", "при ветер"],
        ),
        (
            "🙂 при вет",
            vec![(5, "привет"), (12, "ветер")],
            -7,
            vec!["привет", "при ветер"],
            vec!["🙂 привет", "🙂 при ветер"],
        ),
        (
            "е\u{0308}",
            vec![(0, "ёлка")],
            -2,
            vec!["ёлка"],
            vec!["ёлка"],
        ),
        (
            "е\u{0308}ж при",
            vec![(7, "привет")],
            -3,
            vec!["привет"],
            vec!["е\u{0308}ж привет"],
        ),
        (
            "я хочу",
            vec![(11, " пить")],
            0,
            vec![" пить"],
            vec!["я хочу пить"],
        ),
        (
            "я хочу ",
            vec![(12, "пить")],
            0,
            vec!["пить"],
            vec!["я хочу пить"],
        ),
        (
            "я  при",
            vec![(4, "привет"), (10, "мер")],
            -3,
            vec!["привет", "пример"],
            vec!["я  привет", "я  пример"],
        ),
    ];
    for (q, edits, pos, text, expected) in cases {
        let edits: Vec<_> = edits
            .into_iter()
            .map(|(start_byte, replacement)| TailEdit {
                start_byte,
                replacement: replacement.into(),
            })
            .collect();
        let response = project(q, &edits).unwrap();
        assert_eq!(response.pos, pos);
        assert_eq!(response.text, text);
        let units: Vec<u16> = q.encode_utf16().collect();
        for (insertion, expected) in response.text.iter().zip(expected) {
            let keep = if pos < 0 {
                (units.len() as i32 + pos) as usize
            } else {
                units.len()
            };
            let mut output = units[..keep].to_vec();
            output.extend(insertion.encode_utf16());
            assert_eq!(String::from_utf16(&output).unwrap(), expected);
        }
    }
}

#[test]
#[ignore = "requires verified book.local artifacts/tiny fixture"]
fn test_book_reference_parity() {
    use sha2::{Digest, Sha256};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny");
    let reference: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("reference.json")).unwrap()).unwrap();
    assert_eq!(reference["host"], "book.local");
    for (file, field) in [
        ("model.arpa", "arpa_sha256"),
        ("query.stdout", "query_stdout_sha256"),
    ] {
        let hash = format!("{:x}", Sha256::digest(fs::read(root.join(file)).unwrap()));
        assert_eq!(hash, reference[field].as_str().unwrap());
    }
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("reference.lpk");
    Model::compile_arpa(
        root.join("model.arpa"),
        &package,
        &PackageMetadata::default(),
    )
    .unwrap();
    let model = Model::load(package).unwrap();
    let mut maximum_error = 0.0f64;
    let mut tokens = 0;
    for case in reference["cases"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let words: Vec<_> = text.split_whitespace().collect();
        let expected = case["total_log10"].as_f64().unwrap();
        let actual = model.score_sentence(&words);
        let error = (actual - expected).abs();
        maximum_error = maximum_error.max(error);
        assert!(error < 1e-5, "{text}: {actual} != {expected}");
        let mut context = vec!["<s>"];
        for (word, expected) in words
            .iter()
            .copied()
            .chain(std::iter::once("</s>"))
            .zip(case["token_log10_including_eos"].as_array().unwrap())
        {
            let error = (model.score(&context, word) - expected.as_f64().unwrap()).abs();
            maximum_error = maximum_error.max(error);
            assert!(error < 1e-5, "{text}: token {word}");
            context.push(word);
            tokens += 1;
        }
    }
    println!("book.local stored KenLM reference parity: {} sentences, {tokens} tokens including EOS, max_abs_error={maximum_error:.10}, tolerance=0.00001; no local training or KenLM execution",reference["cases"].as_array().unwrap().len());
}

#[test]
fn test_common_origin_and_punctuation_boundaries() {
    let (_dir, path) = fixture();
    let predictor = Predictor::load(path).unwrap();
    let result = predictor.explain(Request::new("при вет"), None).unwrap();
    let joined = result
        .pool
        .iter()
        .find(|c| c.edit.apply("при вет").unwrap() == "привет")
        .unwrap();
    let completed = result
        .pool
        .iter()
        .find(|c| c.edit.apply("при вет").unwrap() == "при ветер")
        .unwrap();
    assert!((joined.score + 2.0).abs() < 1e-6);
    assert!((completed.score + 2.5).abs() < 1e-6);
    let split = predictor.explain(Request::new("я хочуес"), None).unwrap();
    let candidate = split
        .pool
        .iter()
        .find(|c| c.edit.apply("я хочуес").unwrap() == "я хочу есть")
        .unwrap();
    assert!((candidate.score + 1.45).abs() < 1e-6);
    for q in ["при, вет", "при🙂вет", "при 123 вет"] {
        let result = predictor.explain(Request::new(q), None).unwrap();
        assert!(result
            .pool
            .iter()
            .all(|c| c.edit.start_byte >= q.rfind("вет").unwrap()));
    }
}

#[test]
fn test_rehashed_malformed_records_and_atomic_package_replacement() {
    use sha2::{Digest, Sha256};
    let (_dir, path) = fixture();
    let bytes = fs::read(&path).unwrap();
    let old = Model::load(&path).unwrap();
    let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    for kind in [1u32, 4, 11] {
        let mut bad = bytes.clone();
        let entry = (0..count)
            .map(|i| 32 + i * 56)
            .find(|i| u32::from_le_bytes(bad[*i..*i + 4].try_into().unwrap()) == kind)
            .unwrap();
        let offset = u64::from_le_bytes(bad[entry + 8..entry + 16].try_into().unwrap()) as usize;
        let length = u64::from_le_bytes(bad[entry + 16..entry + 24].try_into().unwrap()) as usize;
        match kind {
            1 => bad[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes()),
            4 => bad[offset + 4..offset + 8].copy_from_slice(&u32::MAX.to_le_bytes()),
            11 => bad[offset + 16..offset + 20].copy_from_slice(&f32::NAN.to_bits().to_le_bytes()),
            _ => unreachable!(),
        }
        let hash = Sha256::digest(&bad[offset..offset + length]);
        bad[entry + 24..entry + 56].copy_from_slice(&hash);
        let corrupt = path.with_extension("bad");
        fs::write(&corrupt, bad).unwrap();
        assert!(Model::load(corrupt).is_err());
    }
    let arpa = path.with_extension("arpa");
    fs::write(&arpa, ARPA).unwrap();
    let long_word = "я".repeat(40);
    Model::compile_arpa(
        &arpa,
        &path,
        &PackageMetadata {
            external_words: vec![long_word.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(old.word_id(&long_word).is_none());
    assert!(Model::load(&path).unwrap().word_id(&long_word).is_some());
    let predictor = Predictor::load(&path).unwrap();
    let prefix = "я".repeat(33);
    assert!(predictor
        .explain(Request::new(&prefix), None)
        .unwrap()
        .pool
        .iter()
        .any(|c| c.edit.apply(&prefix).unwrap() == long_word));
}
