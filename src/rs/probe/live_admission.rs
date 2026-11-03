//! Opt-in single-submit probe for an explicitly confirmed disposable session.
use super::{Args, Connector, ProbeError, SessionAdapter};
use crate::gateway::{
    admission_store::AdmissionStore,
    gateway_identity::Submission,
    gateway_live_admission::{LiveAdmissionRequest, submit_once},
};
use serde_json::{Value, json};

/// Perform offline preflight, or submit exactly once after both confirmations.
pub fn run<C: Connector>(args: &Args, connector: &mut C) -> Result<Value, ProbeError> {
    if !args.has("submit") {
        return Ok(
            json!({"result":"preflight_only","submission_enabled":false,"gateway_contacted":false}),
        );
    }
    if !args.has("confirm-session-id") || args.text("confirm-session-id") != args.text("session-id")
    {
        return Err(ProbeError::value(
            "--confirm-session-id must exactly match --session-id",
        ));
    }
    if !args.has("confirm-disposable-session") {
        return Err(ProbeError::value(
            "--confirm-disposable-session is required for live submission",
        ));
    }
    if ["profile-home", "session-id", "submission-id"]
        .iter()
        .any(|key| args.text(key).trim().is_empty())
    {
        return Err(ProbeError::value(
            "profile-home, session-id, and submission-id must be nonblank",
        ));
    }
    let mut rpc = args.connect(connector)?;
    let store = AdmissionStore::new(&args.path("ledger")).map_err(ProbeError::runtime)?;
    let request = LiveAdmissionRequest {
        submission: Submission::new(args.text("session-id"), args.text("submission-id")),
        text: "M3 harness integration probe. Reply with exactly M3_OK; do not use tools or modify files.".into(),
    };
    let result = submit_once(&store, &request, &mut SessionAdapter(&mut rpc))
        .map_err(ProbeError::runtime)?;
    Ok(
        json!({"result":"verified","submit_calls":1,"receipt_calls":1,"receipt_identity_verified":true,"submit_status":result.submit_status,"observed_status":result.observed_status,"ledger_state":"admitted"}),
    )
}
