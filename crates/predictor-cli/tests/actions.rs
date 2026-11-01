use predictor_cli::evaluate::{paired_interval, simulate};
use predictor_core::Response;

#[test]
fn paid_selection_and_spaces_have_hand_counted_costs() {
    for (cost, paid) in [(1, 2), (2, 3), (3, 4)] {
        let measured = simulate("я хочу чай", cost, |q| {
            Ok(Response {
                text: if q == "я" || q.is_empty() {
                    vec![" хочу чай".into()]
                } else {
                    vec![]
                },
                pos: 0,
                end_of_word: true,
            })
        })
        .unwrap();
        assert_eq!(measured.typed, 10);
        assert_eq!(measured.paid, paid);
        assert_eq!(measured.selections, 1);
    }
    let no_help = simulate("😀 ё", 1, |_| {
        Ok(Response {
            text: vec![],
            pos: 0,
            end_of_word: false,
        })
    })
    .unwrap();
    assert_eq!(no_help.typed, 3);
    assert_eq!(no_help.paid, 3);
    assert_eq!(paired_interval(&[0.2; 5], 10000).unwrap(), [0.2, 0.2]);
}

#[test]
fn bounded_evaluation_sample_is_not_biased_by_file_order() {
    let temporary = tempfile::tempdir().unwrap();
    let a = temporary.path().join("a.txt");
    let b = temporary.path().join("b.txt");
    std::fs::write(&a, "я хочу чай\nмне нужна помощь\nспасибо большое\n").unwrap();
    std::fs::write(&b, "спасибо большое\nмне нужна помощь\nя хочу чай\n").unwrap();
    let selected = predictor_cli::evaluate::read_phrases(&a, 2).unwrap();
    assert_eq!(selected.len(), 2);
    assert_eq!(
        selected,
        predictor_cli::evaluate::read_phrases(&b, 2).unwrap()
    );
}
