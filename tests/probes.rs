#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::probe::{self, Connector, ProbeError};
use serde_json::{Value, json};

type Calls = Rc<RefCell<Vec<(String, BTreeMap<String, Value>)>>>;
type Ledgers = Rc<RefCell<Vec<(PathBuf, String, String, Option<String>)>>>;

#[derive(Default)]
struct Fake {
    endpoints: Vec<PathBuf>,
    calls: Calls,
    fail: bool,
    ledgers: Ledgers,
    inspect_ledger: bool,
    terminal: Option<Value>,
}

struct Rpc {
    calls: Calls,
    fail: bool,
    ledgers: Ledgers,
    inspect_ledger: bool,
    terminal: Option<Value>,
}

impl Connector for Fake {
    type Rpc = Rpc;
    fn connect(&mut self, endpoint: &Path) -> Result<Rpc, ProbeError> {
        self.endpoints.push(endpoint.to_owned());
        Ok(Rpc {
            calls: self.calls.clone(),
            fail: self.fail,
            ledgers: self.ledgers.clone(),
            inspect_ledger: self.inspect_ledger,
            terminal: self.terminal.clone(),
        })
    }
}

impl GatewayRpc for Rpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls
            .borrow_mut()
            .push((method.into(), params.clone()));
        if self.fail {
            return Err(std::io::Error::other("private gateway details").into());
        }
        if method == "session.events.since" && self.inspect_ledger {
            let session = params["session_id"].as_str().unwrap();
            for entry in std::fs::read_dir(std::env::temp_dir()).unwrap().flatten() {
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("hermes-recovery-probe-")
                {
                    continue;
                }
                let path = entry.path().join("admissions.sqlite");
                let Ok(db) = rusqlite::Connection::open_with_flags(
                    &path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                ) else {
                    continue;
                };
                if let Ok((id, state, admission)) = db.query_row(
                    "SELECT submission_id, local_state, admission_id FROM admissions WHERE session_id = ?",
                    [session], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
                ) { self.ledgers.borrow_mut().push((path, id, state, admission)); }
            }
        }
        if method == "prompt.receipt"
            && let Some(response) = &self.terminal
        {
            return Ok(response.clone());
        }
        match method {
            "session.create" => {
                Ok(json!({"session_id":"local-aaaaaaaaaaaaaaaa","subscription_id":"sub-1"}))
            }
            "prompt.submit" | "prompt.receipt" => {
                Ok(json!({"admission_id":"adm-1","status":"queued"}))
            }
            "session.events.since" => Ok(
                json!({"epoch":"epoch-1","replay_epoch":"epoch-1","latest_seq":0,"truncated":false,"snapshot_required":false,"events":[]}),
            ),
            "ping" => Ok(json!({"pong":true})),
            "session.list" => Ok(json!({"scope":params.get("scope"),"sessions":[]})),
            _ => panic!("Unauthorized RPC {method}"),
        }
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn disposable(dir: &Path) -> Vec<OsString> {
    let mut a = args(&[
        "disposable-admission",
        "--profile-home",
        "nonexistent-profile",
        "--create-request-id",
        "create-test",
        "--submission-id",
        "submit-test",
        "--creation-record",
    ]);
    a.push(dir.join("creation.json").into());
    a.push("--ledger".into());
    a.push(dir.join("ledger.sqlite").into());
    a
}

fn live(dir: &Path) -> Vec<OsString> {
    let mut a = args(&[
        "live-admission",
        "--profile-home",
        "nonexistent-profile",
        "--session-id",
        "local-aaaaaaaaaaaaaaaa",
        "--submission-id",
        "submit-test",
        "--ledger",
    ]);
    a.push(dir.join("ledger.sqlite").into());
    a
}

fn invoke(argv: &[OsString], fake: &mut Fake) -> (i32, Value, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = probe::run_with(argv, &mut out, &mut err, fake);
    (
        status,
        serde_json::from_slice(&out).unwrap_or(Value::Null),
        String::from_utf8(err).unwrap(),
    )
}

fn help(command: &str, flags: &[&str]) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hermes-probe"))
        .args([command, "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let out = String::from_utf8(output.stdout).unwrap();
    for flag in flags {
        assert!(out.contains(flag), "Missing help flag {flag}");
    }
}

fn empty(dir: &Path, fake: &Fake) {
    assert_eq!(fake.endpoints.len(), 0);
    assert!(fake.calls.borrow().is_empty());
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
}

#[test]
fn preflight_is_offline_and_creates_no_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (code, value, _) = invoke(&disposable(dir.path()), &mut fake);
    assert_eq!(code, 0);
    assert_eq!(
        value,
        json!({"result":"preflight_only","gateway_contacted":false,"submission_enabled":false})
    );
    empty(dir.path(), &fake);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hermes-probe"))
        .args(disposable(dir.path()))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        value
    );
    empty(dir.path(), &fake);
}

#[test]
fn bad_confirmation_rejected_before_gateway() {
    for confirmation in ["incorrect", ""] {
        let dir = tempfile::tempdir().unwrap();
        let mut a = disposable(dir.path());
        a.extend(args(&[
            "--execute",
            "--confirm-disposable",
            "--confirm-create-request-id",
            confirmation,
            "--endpoint",
            "explicit.sock",
        ]));
        let mut fake = Fake::default();
        let (code, value, _) = invoke(&a, &mut fake);
        assert_eq!(code, 2);
        assert_eq!(
            value,
            json!({"result":"unavailable","error_type":"ValueError"})
        );
        let parsed = probe::parse("disposable-admission", &a[1..]).unwrap();
        assert_eq!(
            probe::disposable_admission::run(&parsed, &mut fake)
                .unwrap_err()
                .message,
            "Confirmation must exactly match the creation request ID"
        );
        empty(dir.path(), &fake);
    }
}

#[test]
fn missing_disposable_ack_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = disposable(dir.path());
    a.extend(args(&[
        "--execute",
        "--confirm-create-request-id",
        "create-test",
        "--endpoint",
        "explicit.sock",
    ]));
    let mut fake = Fake::default();
    let (code, value, _) = invoke(&a, &mut fake);
    assert_eq!(code, 2);
    assert_eq!(value["error_type"], "ValueError");
    let parsed = probe::parse("disposable-admission", &a[1..]).unwrap();
    assert_eq!(
        probe::disposable_admission::run(&parsed, &mut fake)
            .unwrap_err()
            .message,
        "Explicit disposable-session confirmation is required"
    );
    empty(dir.path(), &fake);
}

#[test]
fn reused_durable_paths_rejected() {
    for occupied in ["creation.json", "ledger.sqlite"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(occupied), "keep").unwrap();
        let mut a = disposable(dir.path());
        a.extend(args(&[
            "--execute",
            "--confirm-create-request-id",
            "create-test",
            "--confirm-disposable",
            "--endpoint",
            "explicit.sock",
        ]));
        let mut fake = Fake::default();
        let (code, value, _) = invoke(&a, &mut fake);
        assert_eq!(code, 2);
        assert_eq!(value["error_type"], "ValueError");
        assert_eq!(fake.endpoints.len(), 0);
        assert!(fake.calls.borrow().is_empty());
        let parsed = probe::parse("disposable-admission", &a[1..]).unwrap();
        let error = probe::disposable_admission::run(&parsed, &mut Fake::default()).unwrap_err();
        assert!(error.message.contains("Durable path exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(occupied)).unwrap(),
            "keep"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn no_gateway_autostart_or_hidden_submission() {
    help(
        "disposable-admission",
        &[
            "--execute",
            "--confirm-create-request-id",
            "--confirm-disposable",
            "--endpoint",
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let mut a = disposable(dir.path());
    a.extend(args(&[
        "--execute",
        "--confirm-create-request-id",
        "create-test",
        "--confirm-disposable",
        "--endpoint",
        "explicit.sock",
    ]));
    let mut fake = Fake::default();
    assert_eq!(invoke(&a, &mut fake).0, 0);
    assert_eq!(fake.endpoints, [PathBuf::from("explicit.sock")]);
    let calls = fake.calls.borrow();
    assert_eq!(
        calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        ["session.create", "prompt.submit", "prompt.receipt"]
    );
    assert_eq!(calls[0].1["request_id"], "create-test");
    assert_eq!(calls[0].1["toolsets"], json!([]));
    assert_eq!(calls[1].1["submission_id"], "submit-test");
    let store = AdmissionStore::new(&dir.path().join("ledger.sqlite")).unwrap();
    assert_eq!(
        store.get("submit-test").unwrap().unwrap().local_state,
        LocalState::Admitted
    );
}

#[test]
fn live_probe_requires_explicit_submit_flag() {
    help(
        "live-admission",
        &[
            "--submit",
            "--confirm-session-id",
            "--confirm-disposable-session",
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let (code, value, _) = invoke(&live(dir.path()), &mut fake);
    assert_eq!(code, 0);
    assert_eq!(
        value,
        json!({"result":"preflight_only","submission_enabled":false,"gateway_contacted":false})
    );
    empty(dir.path(), &fake);
}

#[test]
fn live_probe_attaches_to_discovered_endpoint_not_ensure_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = live(dir.path());
    a.extend(args(&[
        "--submit",
        "--confirm-session-id",
        "local-aaaaaaaaaaaaaaaa",
        "--confirm-disposable-session",
        "--endpoint",
        "operator-selected.sock",
    ]));
    let mut fake = Fake::default();
    let (code, value, _) = invoke(&a, &mut fake);
    assert_eq!(code, 0);
    assert_eq!(value["receipt_identity_verified"], true);
    assert_eq!(fake.endpoints, [PathBuf::from("operator-selected.sock")]);
    let calls = fake.calls.borrow();
    assert_eq!(
        calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        ["prompt.submit", "prompt.receipt"]
    );
    assert_eq!(calls[0].1["session_id"], "local-aaaaaaaaaaaaaaaa");
}

#[test]
fn live_probe_requires_exact_session_confirmation_and_disposable_ack() {
    for extras in [
        vec!["--submit", "--confirm-disposable-session"],
        vec![
            "--submit",
            "--confirm-session-id",
            "wrong",
            "--confirm-disposable-session",
        ],
        vec!["--submit", "--confirm-session-id", "local-aaaaaaaaaaaaaaaa"],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut a = live(dir.path());
        a.extend(args(&extras));
        a.extend(args(&["--endpoint", "explicit.sock"]));
        let mut fake = Fake::default();
        let (code, value, _) = invoke(&a, &mut fake);
        assert_eq!(code, 2);
        assert_eq!(value["error_type"], "ValueError");
        let parsed = probe::parse("live-admission", &a[1..]).unwrap();
        let error = probe::live_admission::run(&parsed, &mut fake).unwrap_err();
        assert_eq!(
            error.message,
            if extras.contains(&"--confirm-disposable-session") {
                "--confirm-session-id must exactly match --session-id"
            } else {
                "--confirm-disposable-session is required for live submission"
            }
        );
        empty(dir.path(), &fake);
    }
}

#[test]
fn host_probe_has_no_submission_or_cancel_rpc() {
    let mut fake = Fake::default();
    let (code, value, _) = invoke(
        &args(&[
            "unknown-recovery",
            "--profile-home",
            "unused",
            "--session-id",
            "authorized-session",
            "--endpoint",
            "recovery.sock",
        ]),
        &mut fake,
    );
    assert_eq!(code, 0);
    assert_eq!(
        value,
        json!({"result":"verified","scenario":"synthetic_never_submitted","recovery_status":"still_unknown","ledger_unchanged":true})
    );
    assert_eq!(fake.endpoints, [PathBuf::from("recovery.sock")]);
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "session.events.since");
    assert_eq!(calls[0].1["session_id"], "authorized-session");
}

#[test]
fn host_probe_requires_explicit_session_and_temporary_ledger() {
    for a in [
        args(&["unknown-recovery", "--profile-home", "unused"]),
        args(&["unknown-recovery", "--session-id", "session"]),
        args(&[
            "unknown-recovery",
            "--profile-home",
            "unused",
            "--session-id",
            " ",
            "--endpoint",
            "explicit.sock",
        ]),
        args(&[
            "unknown-recovery",
            "--profile-home",
            "unused",
            "--session-id",
            "session",
            "--ledger",
            "persistent.sqlite",
            "--endpoint",
            "explicit.sock",
        ]),
    ] {
        let mut fake = Fake::default();
        assert_eq!(invoke(&a, &mut fake).0, 2);
        assert_eq!(fake.endpoints.len(), 0);
        assert!(fake.calls.borrow().is_empty());
    }
    // The only ledger permitted by this command is disposable; successful recovery
    // verifies both OUTCOME_UNKNOWN and absence of any authoritative admission ID.
    let mut fake = Fake {
        inspect_ledger: true,
        ..Fake::default()
    };
    // Unique ID so stale recovery dirs in the shared temp dir never match.
    let session = format!("ledger-test-{}", std::process::id());
    let (code, value, _) = invoke(
        &args(&[
            "unknown-recovery",
            "--profile-home",
            "unused",
            "--session-id",
            &session,
            "--endpoint",
            "explicit.sock",
        ]),
        &mut fake,
    );
    assert_eq!(code, 0);
    assert_eq!(value["ledger_unchanged"], true);
    assert_eq!(value["recovery_status"], "still_unknown");
    let ledgers = fake.ledgers.borrow();
    assert_eq!(ledgers.len(), 1);
    let (path, identity, state, admission) = &ledgers[0];
    assert!(identity.starts_with("never-submitted-"));
    assert_eq!(state, "outcome_unknown");
    assert_eq!(admission, &None);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn terminal_preflight_and_receipt_only_reconciliation() {
    use hermes_harness::gateway::gateway_identity::{Receipt, Submission};
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(&[
        "reconcile-terminal",
        "--profile-home",
        "unused",
        "--submission-id",
        "submit-test",
        "--ledger",
    ]);
    a.push(dir.path().join("ledger.sqlite").into());
    let mut fake = Fake::default();
    assert_eq!(invoke(&a, &mut fake).0, 0);
    empty(dir.path(), &fake);
    a.extend(args(&["--execute", "--endpoint", "receipt.sock"]));
    assert_eq!(invoke(&a, &mut fake).0, 2);
    empty(dir.path(), &fake);
    let store = AdmissionStore::new(&dir.path().join("ledger.sqlite")).unwrap();
    store
        .prepare(&Submission::new("session", "submit-test"))
        .unwrap();
    store
        .record_receipt(
            "submit-test",
            &Receipt {
                admission_id: "adm-1".into(),
                status: "queued".into(),
                execution_generation: None,
            },
            false,
        )
        .unwrap();
    fake.terminal = Some(
        json!({"admission_id":"adm-1", "ref":{"session_id":"session"}, "status":"terminal", "outcome":"completed", "execution_generation":1, "result":{"completed":true,"final_response":"M3_OK"}}),
    );
    let (code, value, _) = invoke(&a, &mut fake);
    assert_eq!(code, 0);
    assert_eq!(value["expected_response_match"], true);
    assert_eq!(value["local_state"], "receipt_observed");
    assert_eq!(fake.endpoints, [PathBuf::from("receipt.sock")]);
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "prompt.receipt");
    assert_eq!(calls[0].1["include_result"], true);
}

#[test]
fn session_listing_defaults_and_parser_gates() {
    let mut fake = Fake::default();
    let base = args(&[
        "validate-existing",
        "--profile-home",
        "unused",
        "--endpoint",
        "list.sock",
        "--list-sessions",
        "stored",
    ]);
    let (code, value, _) = invoke(&base, &mut fake);
    assert_eq!(code, 0);
    assert_eq!(value["session_listing"]["limit"], 10);
    assert_eq!(value["session_listing"]["scope"], "stored");
    assert_eq!(fake.calls.borrow()[0].1["limit"], 10);
    for extra in [
        vec!["--session-id", "session"],
        vec!["--limit", "0"],
        vec!["--limit", "21"],
        vec!["--limit", "bad"],
    ] {
        let mut a = base.clone();
        a.extend(args(&extra));
        let mut refused = Fake::default();
        assert_eq!(invoke(&a, &mut refused).0, 2);
        assert_eq!(refused.endpoints.len(), 0);
        assert!(refused.calls.borrow().is_empty());
    }
}

#[test]
fn host_probe_is_parseable() {
    help(
        "unknown-recovery",
        &[
            "--profile-home",
            "--session-id",
            "--endpoint",
            "temporary ledger",
            "no submit or cancel",
        ],
    );
}

#[test]
fn host_script_syntax_and_explicit_endpoint() {
    help(
        "validate-existing",
        &[
            "--profile-home",
            "--endpoint",
            "--session-id",
            "--list-sessions",
            "--limit",
            "Read-only",
        ],
    );
    let mut fake = Fake::default();
    assert_eq!(invoke(&args(&["validate-existing"]), &mut fake).0, 2);
    assert_eq!(
        invoke(
            &args(&["validate-existing", "--profile-home", "unused"]),
            &mut fake
        )
        .0,
        2
    );
    assert_eq!(fake.endpoints.len(), 0);
    let (code, value, _) = invoke(
        &args(&[
            "validate-existing",
            "--profile-home",
            "unused",
            "--endpoint",
            "contract.sock",
        ]),
        &mut fake,
    );
    assert_eq!(code, 0);
    assert_eq!(value["contract"]["ping_verified"], true);
    assert_eq!(fake.endpoints, [PathBuf::from("contract.sock")]);
    assert_eq!(
        fake.calls
            .borrow()
            .iter()
            .map(|(m, _)| m.as_str())
            .collect::<Vec<_>>(),
        ["ping", "session.list"]
    );
}

#[test]
fn all_subcommands_help_and_fail_closed_connector() {
    help(
        "reconcile-terminal",
        &[
            "--ledger",
            "--submission-id",
            "--execute",
            "Never submits or cancels",
        ],
    );
    help("ollama-smoke", &["no files applied"]);
    help(
        "functional-fixture",
        &["Bubblewrap", "never executes generated code"],
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        probe::run(
            &args(&[
                "validate-existing",
                "--profile-home",
                "unused",
                "--endpoint",
                "explicit.sock"
            ]),
            &mut out,
            &mut err
        ),
        2
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains(probe::CLIENT_UNAVAILABLE)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["error_type"],
        "RuntimeError"
    );
}

#[test]
fn disconnected_submit_never_retries_or_leaks_details() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = live(dir.path());
    a.extend(args(&[
        "--submit",
        "--confirm-session-id",
        "local-aaaaaaaaaaaaaaaa",
        "--confirm-disposable-session",
        "--endpoint",
        "explicit.sock",
    ]));
    let mut fake = Fake {
        fail: true,
        ..Fake::default()
    };
    let (code, value, err) = invoke(&a, &mut fake);
    assert_eq!(code, 2);
    assert_eq!(value["result"], "unavailable");
    assert!(!value.to_string().contains("private gateway"));
    assert!(!err.contains("private gateway"));
    assert_eq!(fake.endpoints.len(), 1);
    assert_eq!(fake.calls.borrow().len(), 1);
    let store = AdmissionStore::new(&dir.path().join("ledger.sqlite")).unwrap();
    assert_eq!(
        store.get("submit-test").unwrap().unwrap().local_state,
        LocalState::OutcomeUnknown
    );
}
