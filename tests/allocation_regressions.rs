#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hermes_harness::gateway::gateway_contract_probe::all_method_hints;
use hermes_harness::gateway::gateway_event_replay::{EventReplayRpc, read_pending_snapshot};
use hermes_harness::gateway::gateway_readonly::GatewayRpcError;
use hermes_harness::util::ordered_map::OrderedMap;
use hermes_harness::util::{
    canonical_json_value, ordered_json_compact, ordered_json_spaced, pretty_json_ordered,
    pretty_json_value, sorted_json_spaced_value,
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

#[test]
fn indexed_map_preserves_large_insert_order_and_overwrites() {
    let mut map = OrderedMap::new();
    for index in (0..20_000).rev() {
        assert_eq!(map.insert(index.to_string(), index), None);
    }
    for index in 0..20_000 {
        assert_eq!(map.insert(index.to_string(), index + 1), Some(index));
    }
    assert_eq!(map.len(), 20_000);
    for ((key, value), index) in map.iter().zip((0..20_000).rev()) {
        assert_eq!(key, &index.to_string());
        assert_eq!(*value, index + 1);
        assert_eq!(map.get(key), Some(value));
        assert!(map.contains_key(key));
    }
    let copy = map.clone();
    assert_eq!(copy, map);
    assert_eq!(copy.get("missing"), None);
    assert!(!copy.contains_key("missing"));
    let map: OrderedMap<i32> = serde_json::from_str(r#"{"z":1,"a":2,"z":3}"#).unwrap();
    assert_eq!(serde_json::to_string(&map).unwrap(), r#"{"z":3,"a":2}"#);
    assert_eq!(
        format!("{map:?}"),
        "OrderedMap { entries: [(\"z\", 3), (\"a\", 2)] }"
    );
}

#[test]
fn borrowed_json_and_ordered_fast_path_match_python_fixture() {
    let fixture: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/util_parity.json")).unwrap();
    for case in fixture {
        let input: Value = serde_json::from_str(case["input"].as_str().unwrap()).unwrap();
        assert_eq!(canonical_json_value(&input).unwrap(), case["compact"]);
        assert_eq!(sorted_json_spaced_value(&input).unwrap(), case["spaced"]);
        assert_eq!(pretty_json_value(&input).unwrap(), case["indent2"]);
        assert_eq!(ordered_json_compact(&input).unwrap(), case["compact"]);
        assert_eq!(ordered_json_spaced(&input).unwrap(), case["spaced"]);
        assert_eq!(pretty_json_ordered(&input).unwrap(), case["indent2"]);
    }
}

#[test]
fn nested_function_ranges_include_decorators_and_end_at_dedent() {
    let source = "\
@outer({'outer_decorator': 1})
def outer(first):
    @inner({'inner_decorator': 2})
    def inner(second):
        return {'nested': {'leaf': 3}, 'duplicate': 1, 'duplicate': 2}
    return {'outer': 4}
@sibling({'sibling_decorator': 5})
def sibling(third):
    return {'sibling': 6}
";
    let hints = all_method_hints(source).unwrap();
    assert_eq!(hints.len(), 3);
    assert_eq!(hints[0].parameters, ["first"]);
    assert_eq!(hints[1].parameters, ["second"]);
    assert_eq!(hints[2].parameters, ["third"]);
    assert_eq!(hints[0].returns, 2);
    assert_eq!(hints[1].returns, 1);
    assert_eq!(hints[2].returns, 1);
    assert_eq!(
        hints[0].literal_dict_keys,
        [
            "duplicate",
            "inner_decorator",
            "leaf",
            "nested",
            "outer",
            "outer_decorator"
        ]
    );
    assert_eq!(
        hints[1].literal_dict_keys,
        ["duplicate", "inner_decorator", "leaf", "nested"]
    );
    assert_eq!(hints[2].literal_dict_keys, ["sibling", "sibling_decorator"]);
}

struct ReplayResponses(VecDeque<Value>);

impl EventReplayRpc for ReplayResponses {
    fn rpc(
        &mut self,
        _method: &str,
        _params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        Ok(self.0.pop_front().unwrap())
    }
}

#[test]
fn replay_selects_last_snapshot_but_validates_every_event() {
    for invalid_sequence in [None, Some(1), Some(3)] {
        let events: Vec<_> = (1..=3)
            .map(|sequence| {
                serde_json::json!({
                    "seq": sequence,
                    "session_id": "session-1",
                    "replay_epoch": "epoch-1",
                    "type": "session.info",
                    "payload": {"pending": if invalid_sequence == Some(sequence) {
                        Value::Null
                    } else {
                        serde_json::json!([{"input_id": sequence}])
                    }},
                })
            })
            .collect();
        let envelope = |events: Vec<Value>| {
            serde_json::json!({
                "epoch": "epoch-1",
                "replay_epoch": "epoch-1",
                "latest_seq": 3,
                "truncated": false,
                "snapshot_required": false,
                "events": events,
            })
        };
        let mut rpc = ReplayResponses(VecDeque::from([envelope(Vec::new()), envelope(events)]));
        let result = read_pending_snapshot(&mut rpc, "session-1").unwrap();
        if invalid_sequence.is_some() {
            assert!(result.is_none());
        } else {
            assert_eq!(
                result.unwrap().data["pending"],
                serde_json::json!([{"input_id": 3}])
            );
        }
        assert!(rpc.0.is_empty());
    }
}

#[test]
fn bracket_pairs_keep_nested_keys_and_skip_sets_and_comprehensions() {
    let hints = all_method_hints(
        "\
def sample(first=..., /, *, second=2**3, **kwargs):
    x = {'outer': [{'inner': 1}], 'dupe': 2, 'dupe': 3}
    y = {'ignored' for item in []}
    z = {'key': {'value': 1} for item in []}
    return {'last': x, 'joined' 'key': y}
",
    )
    .unwrap();
    assert_eq!(hints[0].parameters, ["first", "second"]);
    assert_eq!(
        hints[0].literal_dict_keys,
        ["dupe", "inner", "joinedkey", "last", "outer", "value"]
    );
}
