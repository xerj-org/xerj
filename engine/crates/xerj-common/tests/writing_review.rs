use std::collections::BTreeSet;
use xerj_common::writing_review::{review, Check, Review, ReviewConfig, Rule};
fn config() -> ReviewConfig {
    toml::from_str(include_str!("fixtures/writing_review/rules.toml")).unwrap()
}
fn sources() -> BTreeSet<String> {
    BTreeSet::from(["study.1".into()])
}
fn single(check: Check) -> ReviewConfig {
    ReviewConfig {
        rules: vec![Rule {
            id: "test".into(),
            check,
        }],
    }
}
#[test]
fn golden_good_and_bad_drafts() {
    for (draft, expected) in [
        (
            include_str!("fixtures/writing_review/good.txt"),
            include_str!("fixtures/writing_review/good.json"),
        ),
        (
            include_str!("fixtures/writing_review/bad.txt"),
            include_str!("fixtures/writing_review/bad.json"),
        ),
    ] {
        let expected: Review = serde_json::from_str(expected).unwrap();
        assert_eq!(review(draft, &config(), &sources()).unwrap(), expected);
    }
}
#[test]
fn deterministic_json_independent_of_rule_order() {
    let draft = include_str!("fixtures/writing_review/bad.txt");
    let expected = serde_json::to_string(&review(draft, &config(), &sources()).unwrap()).unwrap();
    let mut cfg = config();
    cfg.rules.reverse();
    for _ in 0..20 {
        assert_eq!(
            serde_json::to_string(&review(draft, &cfg, &sources()).unwrap()).unwrap(),
            expected
        );
    }
}
#[test]
fn whole_words_unicode_and_soft_wraps() {
    let cfg = single(Check::BannedPhrases {
        phrases: vec!["GAME changer".into(), "école".into()],
    });
    let result = review(
        "A game\nchanger!\nÉCOLE.\nGames changers; écoles.",
        &cfg,
        &sources(),
    )
    .unwrap();
    assert_eq!(
        result.notes.iter().map(|n| n.line).collect::<Vec<_>>(),
        [1, 3]
    );
    assert!(review("game\n\nchanger", &cfg, &sources()).unwrap().passed);
}
#[test]
fn thresholds_are_inclusive_and_configurable() {
    let cfg = single(Check::SentenceLength { max_words: 3 });
    assert!(review("one two three.", &cfg, &sources()).unwrap().passed);
    assert!(
        !review("one two three four.", &cfg, &sources())
            .unwrap()
            .passed
    );
    let cfg = single(Check::Hedging {
        phrases: vec!["perhaps".into()],
        max_per_100_words: 25,
    });
    assert!(
        review("Perhaps this will work.", &cfg, &sources())
            .unwrap()
            .passed
    );
    assert!(
        !review("Perhaps this works.", &cfg, &sources())
            .unwrap()
            .passed
    );
}
#[test]
fn repetitions_ignore_case_and_citations() {
    let cfg = single(Check::Repetition {
        min_words: 3,
        max_occurrences: 2,
    });
    let result = review(
        "It works well.\nIT WORKS WELL [source:study.1]!\nIt works well.",
        &cfg,
        &sources(),
    )
    .unwrap();
    assert_eq!(result.notes.len(), 1);
    assert_eq!(result.notes[0].line, 3);
}
#[test]
fn citations_need_known_exact_ids_in_same_sentence() {
    let cfg = single(Check::Citations {
        claim_phrases: vec!["research shows".into()],
        numeric_claims: true,
    });
    assert!(
        review("20 people [source:study.1].", &cfg, &sources())
            .unwrap()
            .passed
    );
    assert!(
        !review("20 people. A citation [source:study.1].", &cfg, &sources())
            .unwrap()
            .passed
    );
    assert_eq!(
        review("20 people [source:fake].", &cfg, &sources())
            .unwrap()
            .notes
            .len(),
        2
    );
    assert!(
        !review(
            "Research shows improvement [source:study.1",
            &cfg,
            &sources()
        )
        .unwrap()
        .passed
    );
    assert!(
        !review("20 people [source: study.1].", &cfg, &sources())
            .unwrap()
            .passed
    );
    assert!(
        review("Read this [source:study.1].", &cfg, &sources())
            .unwrap()
            .passed
    );
}
#[test]
fn empty_crlf_and_unicode_inputs() {
    assert!(review("", &config(), &sources()).unwrap().passed);
    assert_eq!(
        review("\r\n\r\nGAME CHANGER.\r\n", &config(), &sources())
            .unwrap()
            .notes[0]
            .line,
        3
    );
    assert!(
        review("你好世界。 مرحبا.", &config(), &sources())
            .unwrap()
            .passed
    );
}
#[test]
fn invalid_config_is_rejected_even_with_empty_input() {
    for check in [
        Check::SentenceLength { max_words: 0 },
        Check::Repetition {
            min_words: 0,
            max_occurrences: 1,
        },
        Check::Repetition {
            min_words: 1,
            max_occurrences: 0,
        },
        Check::BannedPhrases {
            phrases: vec!["!!!".into()],
        },
        Check::Hedging {
            phrases: vec!["Maybe".into(), "maybe".into()],
            max_per_100_words: 10,
        },
    ] {
        assert!(review("", &single(check), &sources()).is_err());
    }
    let mut cfg = config();
    cfg.rules.push(cfg.rules[0].clone());
    assert!(review("", &cfg, &sources()).is_err());
    cfg.rules.clear();
    assert!(review("anything", &cfg, &sources()).unwrap().passed);
}
#[test]
fn unknown_policy_fields_and_kinds_are_rejected() {
    assert!(toml::from_str::<ReviewConfig>(
        "[[rules]]\nid='x'\n[rules.check]\nkind='sentence_length'\nmax_words=12\nmax_word=2"
    )
    .is_err());
    assert!(
        toml::from_str::<ReviewConfig>("[[rules]]\nid='x'\n[rules.check]\nkind='imaginary'")
            .is_err()
    );
}

#[test]
fn citation_only_unknown_markers_are_reported() {
    let cfg = single(Check::Citations {
        claim_phrases: vec![],
        numeric_claims: false,
    });
    assert_eq!(
        review("[source:fake]", &cfg, &sources())
            .unwrap()
            .notes
            .len(),
        1
    );
}

#[test]
fn apostrophe_only_phrases_are_not_words() {
    let cfg = single(Check::BannedPhrases {
        phrases: vec!["'''".into()],
    });
    assert!(review("", &cfg, &sources()).is_err());
}
