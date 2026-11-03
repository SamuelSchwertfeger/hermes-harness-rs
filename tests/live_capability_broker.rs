#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Capability broker tests: real sqlite, bounded Unix sockets, no provider calls.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hermes_harness::broker::live_capability_broker::{
    BoundRequest, BrokerDeniedError, BrokerResponse, CapabilityBroker, CapabilityLedger,
    GatewayAdapter,
};
use hermes_harness::util::sha256_hex;
use serde_json::{Map, Value};

type Calls = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

fn fixture(
    dir: &Path,
) -> (
    CapabilityLedger,
    BoundRequest,
    String,
    BTreeMap<String, String>,
) {
    let ledger = CapabilityLedger::new(&dir.join("ledger.sqlite")).unwrap();
    let prompt = "Implement the bounded task";
    let request = BoundRequest {
        task_id: "task-1".into(),
        role: hermes_harness::sandbox::live_sandbox_plan::Role::Worker,
        session_id: "session-1".into(),
        submission_id: "sub-1".into(),
        method: "prompt.submit".into(),
        text_sha256: sha256_hex(prompt.as_bytes()),
    };
    let token = ledger.issue(&request).unwrap();
    let data = BTreeMap::from([
        ("capability".to_owned(), token.clone()),
        ("task_id".to_owned(), request.task_id.clone()),
        ("role".to_owned(), request.role.as_str().to_owned()),
        ("session_id".to_owned(), request.session_id.clone()),
        ("submission_id".to_owned(), request.submission_id.clone()),
        ("method".to_owned(), request.method.clone()),
        ("text".to_owned(), prompt.to_owned()),
    ]);
    (ledger, request, token, data)
}

fn payload(data: &BTreeMap<String, String>) -> Vec<u8> {
    serde_json::to_vec(data).unwrap()
}

fn admission(id: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("admission_id".to_owned(), Value::String(id.to_owned()));
    map
}

fn message<T: std::fmt::Debug>(result: Result<T, BrokerDeniedError>) -> String {
    result.unwrap_err().to_string()
}

fn status(path: &Path, token: &str) -> Option<String> {
    CapabilityLedger::new(path).unwrap().status(token).unwrap()
}

#[test]
fn one_shot_success_and_durable_no_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, data) = fixture(tmp.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let make = |calls: &Calls| -> GatewayAdapter {
        let recorded = Arc::clone(calls);
        Box::new(move |method, params| {
            recorded
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            Ok(admission("admission-1"))
        })
    };
    let path = ledger.path.clone();
    let mut broker = CapabilityBroker::new(ledger, Some(make(&calls)));
    assert_eq!(
        broker.handle(&payload(&data)).unwrap(),
        BrokerResponse {
            admission_id: "admission-1".into()
        }
    );
    assert_eq!(status(&path, &token).as_deref(), Some("response_recorded"));
    let mut second =
        CapabilityBroker::new(CapabilityLedger::new(&path).unwrap(), Some(make(&calls)));
    assert!(message(second.handle(&payload(&data))).contains("already consumed"));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn uncertain_failure_never_replays() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, data) = fixture(tmp.path());
    let failed = || -> GatewayAdapter {
        Box::new(|_, _| Err(Box::new(std::io::Error::other("transport outcome unknown")) as _))
    };
    let path = ledger.path.clone();
    let mut broker = CapabilityBroker::new(ledger, Some(failed()));
    assert!(message(broker.handle(&payload(&data))).contains("outcome unknown"));
    assert_eq!(
        status(&path, &token).as_deref(),
        Some("submitted_uncertain")
    );
    assert!(message(broker.handle(&payload(&data))).contains("already consumed"));
}

#[test]
fn binding_refuses_tamper() {
    for changed in [
        "role",
        "session_id",
        "submission_id",
        "text",
        "method",
        "task_id",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (ledger, _, token, mut data) = fixture(tmp.path());
        data.insert(changed.to_owned(), "tampered".to_owned());
        let calls = Arc::new(Mutex::new(0_u32));
        let counter = Arc::clone(&calls);
        let adapter: GatewayAdapter = Box::new(move |_, _| {
            *counter.lock().unwrap() += 1;
            Ok(admission("a"))
        });
        let path = ledger.path.clone();
        let mut broker = CapabilityBroker::new(ledger, Some(adapter));
        assert!(
            message(broker.handle(&payload(&data))).contains("mismatch"),
            "{changed}"
        );
        assert_eq!(status(&path, &token).as_deref(), Some("issued"));
        assert_eq!(*calls.lock().unwrap(), 0);
    }
}

#[test]
fn unknown_and_duplicate_issue() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, request, _, mut data) = fixture(tmp.path());
    let path = ledger.path.clone();
    let mut broker = CapabilityBroker::new(ledger, Some(Box::new(|_, _| Ok(Map::new()))));
    data.insert("capability".to_owned(), "00".repeat(32));
    assert!(message(broker.handle(&payload(&data))).contains("unknown"));
    let ledger = CapabilityLedger::new(&path).unwrap();
    assert!(message(ledger.issue(&request)).contains("UNIQUE"));
}

#[test]
fn no_adapter_does_not_consume() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, data) = fixture(tmp.path());
    let path = ledger.path.clone();
    let mut broker = CapabilityBroker::new(ledger, None);
    assert!(message(broker.handle(&payload(&data))).contains("No qualified"));
    assert_eq!(status(&path, &token).as_deref(), Some("issued"));
}

#[cfg(unix)]
#[test]
fn unix_socket_framing() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use hermes_harness::broker::live_capability_broker::serve_connection;

    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, data) = fixture(tmp.path());
    let path = ledger.path.clone();
    let (mut left, mut right) = UnixStream::pair().unwrap();
    let mut broker = CapabilityBroker::new(ledger, Some(Box::new(|_, _| Ok(admission("safe")))));
    let uid = rustix::process::geteuid().as_raw();
    let outcome = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let outcome = serve_connection(&mut left, &mut broker, uid);
            drop(left);
            outcome
        });
        let frame = payload(&data);
        right
            .write_all(&u32::try_from(frame.len()).unwrap().to_be_bytes())
            .unwrap();
        right.write_all(&frame).unwrap();
        let mut header = [0_u8; 4];
        right.read_exact(&mut header).unwrap();
        let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
        right.read_exact(&mut body).unwrap();
        let reply: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply, serde_json::json!({"admission_id": "safe"}));
        drop(right);
        handle.join().unwrap()
    });
    assert!(outcome.is_ok());
    assert_eq!(status(&path, &token).as_deref(), Some("response_recorded"));
}

#[cfg(target_os = "linux")]
#[test]
fn unix_socket_rejects_other_peer_uid() {
    use std::os::unix::net::UnixStream;

    use hermes_harness::broker::live_capability_broker::serve_connection;

    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, _) = fixture(tmp.path());
    let path = ledger.path.clone();
    let (mut left, right) = UnixStream::pair().unwrap();
    let mut broker = CapabilityBroker::new(ledger, Some(Box::new(|_, _| Ok(admission("safe")))));
    let other = rustix::process::geteuid().as_raw().wrapping_add(1);
    let err = serve_connection(&mut left, &mut broker, other).unwrap_err();
    drop(right);
    assert!(err.to_string().contains("Peer UID not authorized"));
    assert_eq!(status(&path, &token).as_deref(), Some("issued"));
}

#[test]
fn reject_oversized_and_unexpected_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, _, mut data) = fixture(tmp.path());
    let mut broker = CapabilityBroker::new(ledger, Some(Box::new(|_, _| Ok(admission("x")))));
    assert!(message(broker.handle(&vec![0x78_u8; 20000])).contains("too large"));
    data.insert("arbitrary".to_owned(), "unsafe".to_owned());
    assert!(message(broker.handle(&payload(&data))).contains("Unexpected"));
}

#[cfg(unix)]
#[test]
fn client_over_real_unix_listener() {
    use std::os::unix::net::UnixListener;

    use hermes_harness::broker::live_capability_broker::serve_connection;
    use hermes_harness::broker::live_capability_client::submit_once;
    use hermes_harness::util::ordered_map::OrderedMap;

    let tmp = tempfile::tempdir().unwrap();
    let (ledger, _, token, data) = fixture(tmp.path());
    let path = ledger.path.clone();
    let address = tmp.path().join("broker.sock");
    let listener = UnixListener::bind(&address).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let mut broker =
        CapabilityBroker::new(ledger, Some(Box::new(|_, _| Ok(admission("verified")))));
    let request: OrderedMap<String> = data.into_iter().collect();
    let outcome = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let (mut connection, _) = listener.accept().unwrap();
            serve_connection(&mut connection, &mut broker, uid)
        });
        let admitted = submit_once(address.to_str().unwrap(), &request).unwrap();
        (admitted, handle.join().unwrap())
    });
    assert_eq!(outcome.0, "verified");
    assert!(outcome.1.is_ok());
    assert_eq!(status(&path, &token).as_deref(), Some("response_recorded"));
}
