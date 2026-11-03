//! Read a terminal receipt and reconcile an existing durable ledger.
use super::{Args, Connector, ProbeError};
use crate::gateway::{
    admission_store::AdmissionStore, gateway_terminal_reconciliation::reconcile_terminal_once,
};
use serde_json::{Value, json};

/// Offline unless explicitly enabled; only receipt reads are authorized.
pub fn run<C: Connector>(args: &Args, connector: &mut C) -> Result<Value, ProbeError> {
    if !args.has("execute") {
        return Ok(json!({"result":"preflight_only","gateway_contacted":false}));
    }
    let ledger = args.path("ledger");
    if !ledger.is_file() || ledger.is_symlink() {
        return Err(ProbeError::value(
            "Existing regular admission ledger required",
        ));
    }
    if args.text("submission-id").trim().is_empty() || args.text("profile-home").trim().is_empty() {
        return Err(ProbeError::value(
            "Profile and submission identities must be nonblank",
        ));
    }
    let mut rpc = args.connect(connector)?;
    let store = AdmissionStore::new(&ledger).map_err(ProbeError::runtime)?;
    let evidence = reconcile_terminal_once(&mut rpc, &store, args.text("submission-id"))
        .map_err(ProbeError::runtime)?;
    Ok(
        json!({"result":"verified_terminal","outcome":evidence.outcome,"generation":evidence.execution_generation,"gateway_status":evidence.recorded.gateway_status,"local_state":evidence.recorded.local_state.as_str(),"expected_response_match":evidence.final_response.as_deref() == Some("M3_OK")}),
    )
}
