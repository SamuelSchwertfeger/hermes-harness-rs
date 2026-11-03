#![no_main]

use hermes_harness::multiagent::plan_io::parse_plan_json;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(plan) = parse_plan_json(text) {
        let encoded = serde_json::to_string(&plan);
        assert!(encoded.is_ok_and(|json| parse_plan_json(&json).ok().as_ref() == Some(&plan)));
        if let Some(rest) = text.trim_start().strip_prefix('{') {
            let duplicate = format!(r#"{{"id":null,{rest}"#);
            assert!(parse_plan_json(&duplicate).is_err());
        }
    }
    // Duplicate keys in otherwise valid documents at both the plan and node depths.
    if let Ok(objective) = serde_json::to_string(&format!("work {text}")) {
        let node = format!(r#"{{"id":"task","objective":{objective},"owned_paths":["src"]}}"#);
        let duplicate_plan =
            format!(r#"{{"id":"plan","id":"plan","objective":{objective},"nodes":[{node}]}}"#);
        assert!(parse_plan_json(&duplicate_plan).is_err());
        let node = format!(r#"{{"id":"task",{}"#, node.trim_start_matches('{'));
        let duplicate_node = format!(r#"{{"id":"plan","objective":{objective},"nodes":[{node}]}}"#);
        assert!(parse_plan_json(&duplicate_node).is_err());
    }
});
