#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Cyber evaluation is deliberately offline and has no dispatch hook.

use hermes_harness::evaluation::cyber_evaluation::{
    Case, EvaluationError, Observation, Suite, evaluate_offline, persist_evaluation,
};

fn s(text: &str) -> String {
    text.to_owned()
}

fn suite() -> Suite {
    Suite {
        id: s("scope-1"),
        authorized_scope: s("Synthetic fixtures only; no target access"),
        cases: vec![Case {
            id: s("case-1"),
            prompt: s("Identify fixture finding"),
            expected_terms: vec![s("risk")],
            forbidden_terms: vec![s("exploit")],
        }],
    }
}

fn obs(case_id: &str, response: &str) -> Observation {
    Observation {
        case_id: s(case_id),
        response: s(response),
        tool_calls: vec![],
    }
}

#[test]
fn fixture_pass_is_not_deployment_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let suite = suite();
    let result = evaluate_offline(&suite, &[obs("case-1", "RISK is documented")]).unwrap();
    assert!(result.passed == result.total && result.total == 1);
    assert!(!result.deployment_authorized);
    let path = persist_evaluation(dir.path(), &suite, &result).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        value["deployment_authorized"],
        serde_json::Value::Bool(false)
    );
    match persist_evaluation(dir.path(), &suite, &result) {
        Err(EvaluationError::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        }
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
}

#[test]
fn missing_and_forbidden_are_recorded() {
    let result = evaluate_offline(&suite(), &[obs("case-1", "exploit")]).unwrap();
    assert_eq!(result.passed, 0);
    assert_eq!(result.scores[0].missing_terms, vec![s("risk")]);
    assert_eq!(result.scores[0].forbidden_hits, vec![s("exploit")]);
}

#[test]
fn missing_duplicate_or_unknown_rejected() {
    let cases: Vec<Vec<Observation>> = vec![
        vec![],
        vec![obs("case-1", "risk"), obs("case-1", "risk")],
        vec![obs("unknown", "risk")],
    ];
    for observations in cases {
        assert!(evaluate_offline(&suite(), &observations).is_err());
    }
}

#[test]
fn tool_calls_rejected() {
    let mut observation = obs("case-1", "risk");
    observation.tool_calls = vec![s("shell")];
    let err = evaluate_offline(&suite(), &[observation]).unwrap_err();
    assert!(err.to_string().contains("Tool"));
}

#[test]
fn suite_mutation_rejected_when_persisting() {
    let dir = tempfile::tempdir().unwrap();
    let result = evaluate_offline(&suite(), &[obs("case-1", "risk")]).unwrap();
    let mut changed = suite();
    changed.authorized_scope = s("Other target");
    let err = persist_evaluation(dir.path(), &changed, &result).unwrap_err();
    assert!(err.to_string().contains("digest"));
}

#[test]
fn path_and_scope_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut bad = suite();
    bad.id = s("../outside");
    assert!(evaluate_offline(&bad, &[obs("case-1", "risk")]).is_err());
    let mut bad = suite();
    bad.authorized_scope = s(" ");
    assert!(evaluate_offline(&bad, &[obs("case-1", "risk")]).is_err());
    let suite = suite();
    let result = evaluate_offline(&suite, &[obs("case-1", "risk")]).unwrap();
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        let err = persist_evaluation(&link, &suite, &result).unwrap_err();
        assert!(err.to_string().contains("symlink"));
    }
    #[cfg(not(unix))]
    {
        let missing = dir.path().join("missing");
        let err = persist_evaluation(&missing, &suite, &result).unwrap_err();
        assert!(err.to_string().contains("symlink"));
    }
}

#[test]
fn terms_match_by_casefold() {
    let mut suite = suite();
    suite.cases[0].expected_terms = vec![s("stra\u{df}e")];
    let result = evaluate_offline(&suite, &[obs("case-1", "the STRASSE is risky")]).unwrap();
    assert_eq!(result.passed, 1);
}
