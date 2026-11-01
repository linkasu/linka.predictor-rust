#[test]
fn dictionary_forms_are_deduplicated_words_not_fake_frequencies() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("dict.xml");
    let output = temp.path().join("words.txt");
    std::fs::write(&input,r#"<?xml version="1.0"?><dictionary><lemmata><lemma id="1"><l t="Ёж"><g v="NOUN"/></l><f t="ёж"/><f t="ежа"/><f t="ежу"/></lemma></lemmata></dictionary>"#).unwrap();
    let count = predictor_cli::lexicon::extract(&input, &output).unwrap();
    assert_eq!(count, 3);
    assert_eq!(std::fs::read_to_string(output).unwrap(), "ежа\nежу\nёж\n");
}

#[test]
fn dictionary_requires_a_complete_document_before_publishing_words() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("broken.xml");
    let output = temp.path().join("words.txt");
    std::fs::write(&input, "<dictionary><lemma><f t=\"чай\"/>").unwrap();
    assert!(predictor_cli::lexicon::extract(&input, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn unsupported_forms_are_reported_and_supported_output_compiles() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("forms.xml");
    let output = temp.path().join("forms.txt");
    std::fs::write(&input,r#"<dictionary><l t="Ёж"/><f t="ёж"/><f t="кто-то"/><f t="два слова"/><f t="123"/></dictionary>"#).unwrap();
    let stats = predictor_cli::lexicon::extract_report(&input, &output).unwrap();
    assert_eq!(stats.wordforms, 1);
    assert_eq!(stats.unsupported_forms, 3);
    assert_eq!(stats.duplicate_forms, 1);
    let arpa = temp.path().join("tiny.arpa");
    std::fs::write(
        &arpa,
        "\\data\\\nngram 1=3\n\\1-grams:\n-3 <unk>\n0 <s>\n-1 </s>\n\\end\\\n",
    )
    .unwrap();
    let words = std::fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let package = temp.path().join("model.lpk");
    predictor_core::Model::compile_arpa(
        arpa,
        &package,
        &predictor_core::PackageMetadata {
            external_words: words,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(predictor_core::Model::load(package)
        .unwrap()
        .word_id("ёж")
        .is_some());
}
