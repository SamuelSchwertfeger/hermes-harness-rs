//! Opt-in one-session/one-admission integration probe.
use super::{Args, Connector, ProbeError, SessionAdapter};
use crate::gateway::{
    admission_store::AdmissionStore,
    gateway_disposable_admission::{DisposableAdmissionRequest, create_and_submit_once},
    gateway_session_creation::SessionCreationRequest,
};
use serde_json::{Value, json};

/// Validate authorization before connecting or creating any durable record.
pub fn run<C: Connector>(args: &Args, connector: &mut C) -> Result<Value, ProbeError> {
    if !args.has("execute") {
        return Ok(
            json!({"result":"preflight_only","gateway_contacted":false,"submission_enabled":false}),
        );
    }
    if !args.has("confirm-create-request-id")
        || args.text("confirm-create-request-id") != args.text("create-request-id")
    {
        return Err(ProbeError::value(
            "Confirmation must exactly match the creation request ID",
        ));
    }
    if !args.has("confirm-disposable") {
        return Err(ProbeError::value(
            "Explicit disposable-session confirmation is required",
        ));
    }
    if ["profile-home", "create-request-id", "submission-id"]
        .iter()
        .any(|key| args.text(key).trim().is_empty())
    {
        return Err(ProbeError::value(
            "Profile and identity arguments must be nonblank",
        ));
    }
    let record = args.path("creation-record");
    let ledger = args.path("ledger");
    if !record.is_absolute() || !ledger.is_absolute() {
        return Err(ProbeError::value(
            "Creation record and ledger paths must be absolute",
        ));
    }
    if record == ledger {
        return Err(ProbeError::value(
            "Creation record and admission ledger must have distinct paths",
        ));
    }
    if record.exists() || ledger.exists() {
        return Err(ProbeError::value(
            "Durable path exists; never reuse an ambiguous attempt",
        ));
    }
    if !record.parent().is_some_and(std::path::Path::is_dir)
        || !ledger.parent().is_some_and(std::path::Path::is_dir)
    {
        return Err(ProbeError::value(
            "Both durable parent directories must exist",
        ));
    }
    let mut rpc = args.connect(connector)?;
    let store = AdmissionStore::new(&ledger).map_err(ProbeError::runtime)?;
    let request = DisposableAdmissionRequest {
        creation: SessionCreationRequest {
            request_id: args.text("create-request-id").into(),
            record_path: record,
        },
        submission_id: args.text("submission-id").into(),
        text: "M3 isolated integration check. Reply M3_OK. Do not use tools or modify files."
            .into(),
    };
    let result = create_and_submit_once(&mut SessionAdapter(&mut rpc), &store, &request)
        .map_err(ProbeError::runtime)?;
    Ok(
        json!({"result":"verified","session_attached":true,"receipt_identity_verified":true,"submit_status":result.admission.submit_status,"observed_status":result.admission.observed_status}),
    )
}
