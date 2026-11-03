#![no_main]

use hermes_harness::multiagent::replacement_proposals::parse_replacement_proposal;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Derive scopes and identity from the document to exercise accepted paths anywhere.
    // Neither operation grants filesystem access; the parser remains entirely in memory.
    let document = serde_json::from_str::<serde_json::Value>(text).ok();
    let task_id = document
        .as_ref()
        .and_then(|value| value.get("task_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("task");
    let scopes: Vec<String> = document
        .as_ref()
        .and_then(|value| value.get("replacements"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("path").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect();
    if let Ok(rows) = parse_replacement_proposal(text, task_id, &scopes) {
        assert!(rows.iter().all(|row| {
            let p = row.path.as_str();
            reference_path_ok(p)
                && scopes
                    .iter()
                    .any(|s| p == s || p.starts_with(&format!("{s}/")))
        }));
    }
});

/// Independent reference for the documented path policy (not the parser's predicate).
fn reference_path_ok(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
        && path.split('/').next() != Some(".git")
}
