#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;

use hermes_harness::gateway::gateway_event_replay::{EventReplayRpc, read_pending_snapshot};
use hermes_harness::gateway::gateway_readonly::{GatewayReadError, GatewayRpcError};
use serde_json::{Value, json};

const EXPECTED_REPLAY_CALLS: usize = 2;

#[derive(Clone)]
struct ReplayFixture {
    events: Value,
    epoch: String,
    latest_seq: i64,
    truncated: bool,
    fail_on_call: Option<usize>,
    malformed_initial: bool,
}

impl ReplayFixture {
    fn new(events: Value) -> Self {
        Self {
            events,
            epoch: "epoch-1".to_owned(),
            latest_seq: 1,
            truncated: false,
            fail_on_call: None,
            malformed_initial: false,
        }
    }
}

struct FakeReplayRpc {
    fixture: ReplayFixture,
    calls: Vec<(String, BTreeMap<String, Value>)>,
    race_after_first: bool,
}

impl FakeReplayRpc {
    fn new(fixture: ReplayFixture) -> Self {
        Self {
            fixture,
            calls: Vec::new(),
            race_after_first: false,
        }
    }
}

impl EventReplayRpc for FakeReplayRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.push((method.to_owned(), params.clone()));
        if self.fixture.fail_on_call == Some(self.calls.len()) {
            return Err(std::io::Error::other("simulated disconnect").into());
        }
        assert_eq!(method, "session.events.since");
        let fixture = self.fixture.clone();
        let response = if params.contains_key("replay_epoch") {
            let count = fixture.events.as_array().map_or(0, Vec::len);
            json!({
                "events": fixture.events,
                "latest_seq": fixture.latest_seq,
                "last_sequence": fixture.latest_seq,
                "epoch": fixture.epoch,
                "replay_epoch": fixture.epoch,
                "truncated": fixture.truncated,
                "snapshot_required": fixture.truncated,
                "count": count,
            })
        } else if fixture.malformed_initial {
            json!({"replay_epoch": null})
        } else {
            json!({
                "events": [],
                "latest_seq": fixture.latest_seq,
                "last_sequence": fixture.latest_seq,
                "epoch": fixture.epoch,
                "replay_epoch": fixture.epoch,
                "truncated": true,
                "snapshot_required": true,
                "count": 0,
            })
        };
        if self.race_after_first && self.calls.len() == 1 {
            "epoch-2".clone_into(&mut self.fixture.epoch);
        }
        Ok(response)
    }
}

fn info_event(pending: &Value, seq: i64) -> Value {
    json!({
        "type": "session.info",
        "session_id": "session-1",
        "payload": {"pending": pending},
        "seq": seq,
        "replay_epoch": "epoch-1",
    })
}

#[test]
fn complete_replay_returns_latest_pending_snapshot() {
    let pending = json!([{"input_id": "sub-1", "admission_id": "adm-1"}]);
    let mut rpc = FakeReplayRpc::new(ReplayFixture::new(json!([info_event(&pending, 1)])));

    let snapshot = read_pending_snapshot(&mut rpc, "session-1")
        .unwrap()
        .unwrap();

    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.data["pending"], pending);
    assert_eq!(rpc.calls.len(), EXPECTED_REPLAY_CALLS);
    let first = BTreeMap::from([
        ("session_id".to_owned(), json!("session-1")),
        ("last_sequence".to_owned(), json!(0)),
    ]);
    let mut second = first.clone();
    second.insert("replay_epoch".to_owned(), json!("epoch-1"));
    assert_eq!(rpc.calls[0].1, first);
    assert_eq!(rpc.calls[1].1, second);
}

#[test]
fn truncated_replay_returns_no_candidate() {
    let mut fixture = ReplayFixture::new(json!([{
        "type": "session.info", "session_id": "session-1",
        "payload": {"pending": []}, "seq": 1,
    }]));
    fixture.truncated = true;
    let mut rpc = FakeReplayRpc::new(fixture);
    assert!(
        read_pending_snapshot(&mut rpc, "session-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn epoch_change_returns_no_candidate() {
    let fixture = ReplayFixture::new(json!([{
        "type": "session.info", "session_id": "session-1",
        "payload": {"pending": []}, "seq": 1,
    }]));
    let mut rpc = FakeReplayRpc::new(fixture);
    rpc.race_after_first = true;
    assert!(
        read_pending_snapshot(&mut rpc, "session-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn missing_pending_event_returns_no_candidate() {
    let fixture = ReplayFixture::new(json!([{
        "type": "message.complete", "session_id": "session-1",
        "payload": {}, "seq": 1, "replay_epoch": "epoch-1",
    }]));
    let mut rpc = FakeReplayRpc::new(fixture);
    assert!(
        read_pending_snapshot(&mut rpc, "session-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn malformed_replay_envelope_is_rejected() {
    let mut fixture = ReplayFixture::new(json!([]));
    fixture.malformed_initial = true;
    let mut rpc = FakeReplayRpc::new(fixture);
    let err = read_pending_snapshot(&mut rpc, "session-1").unwrap_err();
    assert!(matches!(err, GatewayReadError::Read(_)));
}

#[test]
fn disconnect_during_replay_is_not_retried() {
    let mut fixture = ReplayFixture::new(json!([]));
    fixture.fail_on_call = Some(2);
    let mut rpc = FakeReplayRpc::new(fixture);
    let err = read_pending_snapshot(&mut rpc, "session-1").unwrap_err();
    assert!(err.to_string().contains("no retry"));
    assert_eq!(rpc.calls.len(), EXPECTED_REPLAY_CALLS);
}

#[test]
fn malformed_event_is_rejected() {
    let fixture = ReplayFixture::new(json!([
        {"type": "session.info", "session_id": "session-1", "payload": {}, "seq": "bad"}
    ]));
    let mut rpc = FakeReplayRpc::new(fixture);
    let err = read_pending_snapshot(&mut rpc, "session-1").unwrap_err();
    assert!(matches!(err, GatewayReadError::Read(_)));
}

#[test]
fn blank_session_id_is_rejected_without_rpc() {
    let mut rpc = FakeReplayRpc::new(ReplayFixture::new(json!([])));
    let err = read_pending_snapshot(&mut rpc, "  ").unwrap_err();
    assert!(err.to_string().contains("session_id"));
    assert_eq!(rpc.calls.len(), 0);
}

#[test]
fn sequence_gap_returns_no_candidate() {
    let mut fixture = ReplayFixture::new(json!([{
        "type": "session.info", "session_id": "session-1",
        "payload": {"pending": []}, "seq": 2, "replay_epoch": "epoch-1",
    }]));
    fixture.latest_seq = 2;
    let mut rpc = FakeReplayRpc::new(fixture);
    assert!(
        read_pending_snapshot(&mut rpc, "session-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn empty_event_history_does_not_request_replay() {
    let mut fixture = ReplayFixture::new(json!([]));
    fixture.latest_seq = 0;
    let mut rpc = FakeReplayRpc::new(fixture);
    assert!(
        read_pending_snapshot(&mut rpc, "session-1")
            .unwrap()
            .is_none()
    );
    assert_eq!(rpc.calls.len(), 1);
}
