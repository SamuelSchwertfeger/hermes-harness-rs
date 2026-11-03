//! Deterministic, network-free gateway for disposable supervised qualification.

use std::collections::BTreeMap;
use std::io;

use serde_json::{Value, json};

use super::gateway_readonly::{GatewayRpc, GatewayRpcError};

/// In-memory gateway double. Never creates a transport or provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedSupervisedGateway {
    /// Outcome reported for worker sessions.
    pub worker_outcome: String,
    /// Outcome reported for reviewer sessions.
    pub reviewer_outcome: String,
    /// Role whose submission fails ambiguously (empty for none).
    pub fail_submission_role: String,
    /// Every method called, in order.
    pub calls: Vec<String>,
    /// Admission identity to session identity.
    pub sessions: BTreeMap<String, String>,
}

impl Default for ScriptedSupervisedGateway {
    fn default() -> Self {
        Self {
            worker_outcome: "completed".to_owned(),
            reviewer_outcome: "completed".to_owned(),
            fail_submission_role: String::new(),
            calls: Vec::new(),
            sessions: BTreeMap::new(),
        }
    }
}

fn value_error(msg: &str) -> GatewayRpcError {
    GatewayRpcError::Value(msg.to_owned())
}

impl GatewayRpc for ScriptedSupervisedGateway {
    /// Return scripted admissions, queued receipts and terminal evidence.
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.push(method.to_owned());
        let session = match params.get("session_id").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => return Err(value_error("Scripted gateway requires session identity")),
        };
        let role = if session.starts_with("worker-") {
            "worker"
        } else {
            "reviewer"
        };
        if method == "prompt.submit" {
            if role == self.fail_submission_role {
                return Err(GatewayRpcError::Io(io::Error::other(
                    "Scripted ambiguous submission failure",
                )));
            }
            let admission = format!("scripted-{session}");
            self.sessions.insert(admission.clone(), session);
            return Ok(json!({"admission_id": admission, "status": "queued"}));
        }
        let receipt_admission = params
            .get("admission_id")
            .and_then(Value::as_str)
            .filter(|admission| self.sessions.get(*admission) == Some(&session))
            .ok_or_else(|| value_error("Unknown scripted admission identity"))?
            .to_owned();
        if method != "prompt.receipt" {
            return Err(value_error(
                "Only submit and read-only receipts are supported",
            ));
        }
        if params.get("include_result") != Some(&Value::Bool(true)) {
            return Ok(json!({"admission_id": receipt_admission, "status": "queued"}));
        }
        let outcome = if role == "worker" {
            &self.worker_outcome
        } else {
            &self.reviewer_outcome
        };
        Ok(json!({
            "admission_id": receipt_admission,
            "status": "terminal",
            "outcome": outcome,
            "ref": {"session_id": session},
            "result": {
                "completed": outcome == "completed",
                "final_response": "scripted evidence",
            },
        }))
    }
}
