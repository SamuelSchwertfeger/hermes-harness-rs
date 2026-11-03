#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Provider-free broker admission authorization and socket lifecycle tests.

use hermes_harness::broker::live_broker_admission::{OperatorGrant, OperatorGrantStore};

fn grant(role: &str, sub: &str, unit: &str, digest: char) -> OperatorGrant {
    OperatorGrant {
        task_id: "task".into(),
        role: match role {
            "worker" => hermes_harness::sandbox::live_sandbox_plan::Role::Worker,
            "reviewer" => hermes_harness::sandbox::live_sandbox_plan::Role::Reviewer,
            _ => panic!("unexpected test role"),
        },
        session_id: "session".into(),
        submission_id: sub.into(),
        unit: unit.into(),
        text_sha256: digest.to_string().repeat(64),
    }
}

#[test]
fn authorization_is_exactly_once_and_restart_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("grants.db");
    let store = OperatorGrantStore::new(&path).unwrap();
    let grant = grant("worker", "sub", "hermes-job-1", 'a');
    let err = store.authorize(&grant, false).unwrap_err();
    assert!(err.to_string().contains("confirmation"));
    store.authorize(&grant, true).unwrap();
    store.consume(&grant).unwrap();
    let err = OperatorGrantStore::new(&path)
        .unwrap()
        .consume(&grant)
        .unwrap_err();
    assert!(err.to_string().contains("unconsumed"));
    let err = store.authorize(&grant, true).unwrap_err();
    assert!(err.to_string().contains("UNIQUE"));
}

#[test]
fn grant_mismatch_never_consumes_valid_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let store = OperatorGrantStore::new(&tmp.path().join("grants.db")).unwrap();
    let valid = grant("reviewer", "sub", "hermes-job-2", 'b');
    store.authorize(&valid, true).unwrap();
    assert!(
        store
            .consume(&grant("worker", "sub", "hermes-job-2", 'b'))
            .is_err()
    );
    store.consume(&valid).unwrap();
}

#[cfg(unix)]
#[test]
fn listener_framing_and_replay_after_restart() {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use hermes_harness::broker::live_broker_listener::{ListenerSettings, serve_until};
    use hermes_harness::broker::live_capability_broker::{
        BoundRequest, CapabilityBroker, CapabilityLedger,
    };
    use hermes_harness::broker::live_capability_client::submit_once;
    use hermes_harness::util::ordered_map::OrderedMap;
    use hermes_harness::util::sha256_hex;
    use serde_json::{Map, Value};

    let tmp = tempfile::tempdir().unwrap();
    let text = "bounded task";
    let request = BoundRequest {
        task_id: "task".into(),
        role: hermes_harness::sandbox::live_sandbox_plan::Role::Worker,
        session_id: "session".into(),
        submission_id: "sub".into(),
        method: "prompt.submit".into(),
        text_sha256: sha256_hex(text.as_bytes()),
    };
    let ledger = CapabilityLedger::new(&tmp.path().join("capability.db")).unwrap();
    let token = ledger.issue(&request).unwrap();
    let ledger_path = ledger.path.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (recorded, expected) = (Arc::clone(&calls), request.clone());
    let mut broker = CapabilityBroker::with_bound_adapter(
        ledger,
        Box::new(move |bound, prompt| {
            assert_eq!(bound, &expected);
            recorded.lock().unwrap().push(prompt.to_owned());
            let mut out = Map::new();
            out.insert("admission_id".to_owned(), Value::String("receipt".into()));
            Ok(out)
        }),
    );
    let data: BTreeMap<String, String> = BTreeMap::from([
        ("capability".to_owned(), token.clone()),
        ("task_id".to_owned(), request.task_id.clone()),
        ("role".to_owned(), request.role.as_str().to_owned()),
        ("session_id".to_owned(), request.session_id.clone()),
        ("submission_id".to_owned(), request.submission_id.clone()),
        ("method".to_owned(), request.method.clone()),
        ("text".to_owned(), text.to_owned()),
    ]);
    let wire: OrderedMap<String> = data.into_iter().collect();
    let path = tmp.path().join("broker.sock");
    let stop = AtomicBool::new(false);
    let ready = Arc::new(AtomicBool::new(false));
    let mut settings = ListenerSettings::new(path.clone(), rustix::process::geteuid().as_raw());
    settings.max_connections = 2;
    settings.ready = Some(Arc::clone(&ready));
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| serve_until(&settings, &mut broker, &stop));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "Broker listener did not start accepting connections"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let socket = path.to_str().unwrap();
        assert_eq!(submit_once(socket, &wire).unwrap(), "receipt");
        let err = submit_once(socket, &wire).unwrap_err();
        assert!(err.to_string().contains("uncertain"));
        stop.store(true, Ordering::SeqCst);
        handle.join().unwrap().unwrap();
    });
    assert!(!path.exists());
    assert_eq!(*calls.lock().unwrap(), vec![text.to_owned()]);
    assert_eq!(
        CapabilityLedger::new(&ledger_path)
            .unwrap()
            .status(&token)
            .unwrap()
            .as_deref(),
        Some("response_recorded")
    );
}

#[cfg(unix)]
#[test]
fn listener_refuses_existing_paths() {
    use std::sync::atomic::AtomicBool;

    use hermes_harness::broker::live_broker_listener::{ListenerSettings, serve_until};
    use hermes_harness::broker::live_capability_broker::{CapabilityBroker, CapabilityLedger};

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("broker.sock");
    std::fs::write(&path, "not a socket").unwrap();
    let settings = ListenerSettings::new(path, rustix::process::geteuid().as_raw());
    let mut broker =
        CapabilityBroker::new(CapabilityLedger::new(&tmp.path().join("db")).unwrap(), None);
    let err = serve_until(&settings, &mut broker, &AtomicBool::new(false)).unwrap_err();
    assert!(err.to_string().contains("existing"));
}
