//! Properties for untrusted text and byte parsers; each runs 256 cases by default.

use hermes_harness::broker::live_capability_broker::{
    constant_time_eq, decode_request_for_fuzzing,
};
use hermes_harness::git::worktrees::split_z_paths;
use hermes_harness::multiagent::ollama_discovery::parse_ollama_tags;
use hermes_harness::multiagent::ollama_native::decode_chat_completion_for_fuzzing;
use hermes_harness::multiagent::plan_io::parse_plan_json;
use hermes_harness::multiagent::replacement_proposals::parse_replacement_proposal;
use hermes_harness::util::{
    is_py_space, is_slug_id_for_testing, py_strip, splitlines_keepends_for_testing,
};
use proptest::prelude::*;

/// Independent reference for the documented path policy: relative, no backslash or NUL,
/// no empty, `.` or `..` component, and the first component is not `.git`.
fn reference_path_ok(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
        && path.split('/').next() != Some(".git")
}

/// Independent reference for ownership: equal to a scope or strictly below it.
fn reference_owned(path: &str, scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|s| path == s || path.starts_with(&format!("{s}/")))
}
use serde_json::json;

const fn python_separator(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

proptest! {
    #[test]
    fn splitlines_preserves_input(text in any::<String>()) {
        let lines = splitlines_keepends_for_testing(&text);
        prop_assert_eq!(lines.concat(), text.as_str());
        for line in lines.iter().take(lines.len().saturating_sub(1)) {
            prop_assert!(line.chars().next_back().is_some_and(python_separator));
        }
    }

    #[test]
    fn strip_matches_python_and_is_idempotent(text in any::<String>()) {
        let stripped = py_strip(&text);
        prop_assert_eq!(stripped, text.trim_matches(is_py_space));
        prop_assert_eq!(py_strip(stripped), stripped);
    }

    #[test]
    fn slug_matches_reference(text in prop_oneof![any::<String>(), "[a-z0-9-]{0,66}"]) {
        let bytes = text.as_bytes();
        let reference = (1..=64).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        prop_assert_eq!(is_slug_id_for_testing(&text), reference);
    }

    #[test]
    fn constant_time_comparison_matches_equality(
        left in proptest::collection::vec(any::<u8>(), 0..512),
        right in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        prop_assert_eq!(constant_time_eq(&left, &right), left == right);
        prop_assert!(constant_time_eq(&left, &left));
        let mut changed = left.clone();
        if let Some(byte) = changed.last_mut() {
            *byte ^= 1;
            prop_assert!(!constant_time_eq(&left, &changed));
        }
    }

    #[test]
    fn git_z_paths_round_trip(
        names in proptest::collection::vec("[^\x00]{1,64}", 0..16),
        terminated in any::<bool>(),
    ) {
        let paths: Vec<_> = names.iter().map(|name| format!("{name} \t\n")).collect();
        let mut raw = paths.join("\0");
        if terminated && !paths.is_empty() {
            raw.push('\0');
        }
        prop_assert_eq!(split_z_paths(&raw).collect::<Vec<_>>(), paths);
    }

    #[test]
    fn plan_parser_never_panics_and_round_trips(text in any::<String>()) {
        if let Ok(plan) = parse_plan_json(&text) {
            let encoded = serde_json::to_string(&plan)?;
            prop_assert_eq!(parse_plan_json(&encoded).ok(), Some(plan));
        }
    }

    #[test]
    fn generated_plans_round_trip_and_reject_duplicate_keys(suffix in any::<String>()) {
        let objective = format!("work {suffix}");
        let document = json!({
            "id": "plan", "objective": objective, "primary_identity": "primary",
            "nodes": [{"id": "task", "objective": "work", "owned_paths": ["src"]}],
        });
        let payload = document.to_string();
        let plan = parse_plan_json(&payload);
        prop_assert!(plan.is_ok());
        if let Ok(plan) = plan {
            let encoded = serde_json::to_string(&plan)?;
            prop_assert_eq!(parse_plan_json(&encoded).ok(), Some(plan));
        }
        // Inject identical duplicate keys into otherwise admissible documents at both depths.
        if let Some(object) = document.as_object() {
            for (key, value) in object {
                let key = serde_json::to_string(key)?;
                let duplicate = format!("{{{key}:{value},{}", payload.trim_start_matches('{'));
                prop_assert!(parse_plan_json(&duplicate).is_err());
            }
        }
        if let Some(node) = document.get("nodes").and_then(|nodes| nodes.get(0))
            && let Some(object) = node.as_object()
        {
            for (key, value) in object {
                let key = serde_json::to_string(key)?;
                let duplicate = format!("{{{key}:{value},{}", node.to_string().trim_start_matches('{'));
                let objective = serde_json::to_string(&objective)?;
                let payload = format!(r#"{{"id":"plan","objective":{objective},"nodes":[{duplicate}]}}"#);
                prop_assert!(parse_plan_json(&payload).is_err());
            }
        }
    }

    #[test]
    fn proposal_parser_never_panics_and_paths_are_safe(text in any::<String>()) {
        if let Ok(rows) = parse_replacement_proposal(&text, "task", &["src".to_owned()]) {
            let scopes = ["src".to_owned()];
            prop_assert!(rows.iter().all(|row| reference_path_ok(&row.path) && reference_owned(&row.path, &scopes)));
        }
    }

    #[test]
    fn proposal_paths_obey_lexical_predicate(path in any::<String>(), content in any::<String>()) {
        let payload = json!({
            "task_id": "task",
            "replacements": [{"path": path, "expected_sha256": "0".repeat(64), "content": content}],
        }).to_string();
        let result = parse_replacement_proposal(&payload, "task", std::slice::from_ref(&path));
        prop_assert_eq!(result.is_ok(), reference_path_ok(&path));
        if let Ok(rows) = result {
            prop_assert!(rows.iter().all(|row| reference_path_ok(&row.path)));
        }
    }

    #[test]
    fn byte_decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        if let Ok(fields) = decode_request_for_fuzzing(&bytes) {
            let keys = ["capability", "task_id", "role", "session_id", "submission_id", "method", "text"];
            prop_assert_eq!(fields.len(), keys.len());
            prop_assert!(keys.iter().all(|key| fields.contains_key(*key)));
        }
        let _ = parse_ollama_tags(&bytes);
        let _ = decode_chat_completion_for_fuzzing(&bytes);
    }
}
