use super::*;
use std::collections::HashMap;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (format!("ANYTOPDF_SENTIMENT_{k}"), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn segment(text: &str, start: f64) -> Segment {
    Segment {
        text: text.into(),
        from: Origin::Transcript,
        time_range: Some((start, start + 2.0)),
        region: None,
        paragraph: None,
    }
}

#[test]
fn defaults_use_the_lexicon_on_every_text_source() {
    let config = Config::from_env(env(&[])).unwrap();
    assert!(matches!(config.backend, Backend::Vader));
    assert_eq!(config.threshold, DEFAULT_THRESHOLD);
    assert!(!config.neutral && config.tones);
    assert_eq!(config.from.len(), 4);
}

#[test]
fn an_llm_url_selects_the_llm_backend_and_needs_a_model() {
    let config = Config::from_env(env(&[
        ("LLM_URL", "http://127.0.0.1:11434/v1"),
        ("LLM_MODEL", "qwen2.5:3b"),
        ("LLM_TIMEOUT", "90"),
    ]))
    .unwrap();
    let Backend::Llm(llm) = config.backend else {
        panic!("expected the llm backend");
    };
    assert_eq!(llm.model, "qwen2.5:3b");
    assert_eq!(llm.timeout, Duration::from_secs(90));

    let err = Config::from_env(env(&[("LLM_URL", "http://h/v1")])).unwrap_err();
    assert!(format!("{err:#}").contains("ANYTOPDF_SENTIMENT_LLM_MODEL"));
    // Forcing the lexicon ignores a configured endpoint.
    let config =
        Config::from_env(env(&[("BACKEND", "vader"), ("LLM_URL", "http://h/v1")])).unwrap();
    assert!(matches!(config.backend, Backend::Vader));
}

#[test]
fn invalid_settings_name_the_variable() {
    for (key, value, needle) in [
        ("BACKEND", "bert", "ANYTOPDF_SENTIMENT_BACKEND"),
        ("THRESHOLD", "2", "ANYTOPDF_SENTIMENT_THRESHOLD"),
        ("NEUTRAL", "maybe", "ANYTOPDF_SENTIMENT_NEUTRAL"),
        ("FROM", "ocr,faces", "ANYTOPDF_SENTIMENT_FROM"),
        ("BACKEND", "llm", "ANYTOPDF_SENTIMENT_LLM_URL"),
    ] {
        let err = Config::from_env(env(&[(key, value)])).unwrap_err();
        assert!(
            format!("{err:#}").contains(needle),
            "{key}={value}: {err:#}"
        );
    }
    let config = Config::from_env(env(&[("FROM", "ocr, caption"), ("TONES", "off")])).unwrap();
    assert_eq!(config.from, [Origin::Ocr, Origin::Caption]);
    assert!(!config.tones);
}

#[test]
fn segments_get_sentiment_tone_and_an_overall_line() {
    let config = Config::from_env(env(&[])).unwrap();
    let segments = [
        segment("Thanks, the demo was great!", 0.0),
        segment(
            "The upload is broken again and this is the worst service, I want a refund.",
            4.0,
        ),
        segment("When does the meeting start?", 9.0),
    ];
    let mut warnings = Vec::new();
    let (analyses, provider) = analyze(&segments, &config, &mut warnings);
    assert!(warnings.is_empty());
    assert_eq!(provider, "sentiment-vader");
    let labels: Vec<&str> = analyses.iter().map(|a| a.label).collect();
    assert_eq!(labels, ["positive", "negative", "neutral"]);

    let annotations = annotate(&segments, &analyses, &provider, &config, None);
    let summary: Vec<(&str, &str)> = annotations
        .iter()
        .map(|a| {
            (
                a["attributes"]["entity"].as_str().unwrap(),
                a["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("sentiment", "positive"),
            ("sentiment", "negative"),
            ("tone", "complaint"),
            ("tone", "question"),
            ("sentiment-overall", "overall negative"),
        ]
    );
    let negative = &annotations[1];
    assert_eq!(negative["time_range"]["start_seconds"], 4.0);
    assert_eq!(negative["attributes"]["from"], "transcript");
    assert!(
        negative["attributes"]["score"]
            .as_str()
            .unwrap()
            .starts_with('-')
    );
    let overall = annotations.last().unwrap();
    assert_eq!(overall["time_range"]["start_seconds"], 0.0);
    assert_eq!(overall["time_range"]["end_seconds"], 11.0);
    assert_eq!(overall["attributes"]["segments"], "3");
    assert_eq!(overall["attributes"]["neutral"], "1");
    // Every annotation fits the host's model (string attributes, valid confidence).
    for a in &annotations {
        let parsed: anytopdf_core::Annotation = serde_json::from_value(a.clone()).unwrap();
        assert_eq!(parsed.kind, anytopdf_core::AnnotationKind::Custom);
        assert!(parsed.confidence.is_none_or(|c| (0.0..=1.0).contains(&c)));
    }
}

#[test]
fn neutral_segments_are_only_written_when_asked_for() {
    let quiet = Config::from_env(env(&[])).unwrap();
    let verbose = Config::from_env(env(&[("NEUTRAL", "true")])).unwrap();
    let segments = [segment("The meeting is at noon.", 0.0)];
    let mut warnings = Vec::new();
    let (analyses, provider) = analyze(&segments, &quiet, &mut warnings);
    assert!(annotate(&segments, &analyses, &provider, &quiet, None).is_empty());
    let annotations = annotate(&segments, &analyses, &provider, &verbose, None);
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0]["text"], "neutral");
}

#[test]
fn an_unreachable_llm_falls_back_to_the_lexicon_with_a_warning() {
    // Bind and drop a listener so the port is very likely closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Config::from_env(env(&[
        ("LLM_URL", &format!("http://127.0.0.1:{port}/v1")),
        ("LLM_MODEL", "m"),
        ("LLM_TIMEOUT", "5"),
    ]))
    .unwrap();
    let segments = [segment("I love this", 0.0)];
    let mut warnings = Vec::new();
    let (analyses, provider) = analyze(&segments, &config, &mut warnings);
    assert_eq!(provider, "sentiment-vader");
    assert_eq!(analyses[0].label, "positive");
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("used the VADER lexicon instead"),
        "{warnings:?}"
    );
}
