#![allow(dead_code, unreachable_pub, clippy::pedantic, clippy::nursery)]
//! In-memory admission ledger and scripted receipt transport for offline tests.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io;

use hermes_harness::gateway::gateway_identity::Receipt;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::gateway_terminal_reconciliation::{
    AdmissionLedger, AdmissionRecordView,
};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub submission_id: String,
    pub session_id: String,
    pub admission_id: Option<String>,
    pub local_state: String,
    pub gateway_status: Option<String>,
    pub execution_generation: Option<u64>,
}

impl AdmissionRecordView for Record {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn admission_id(&self) -> Option<&str> {
        self.admission_id.as_deref()
    }
}

#[derive(Default)]
pub struct FakeLedger {
    records: RefCell<BTreeMap<String, Record>>,
}

impl FakeLedger {
    pub fn prepare(&self, session_id: &str, submission_id: &str) {
        self.records.borrow_mut().insert(
            submission_id.to_owned(),
            Record {
                submission_id: submission_id.to_owned(),
                session_id: session_id.to_owned(),
                admission_id: None,
                local_state: "prepared".to_owned(),
                gateway_status: None,
                execution_generation: None,
            },
        );
    }

    pub fn record(&self, submission_id: &str, receipt: &Receipt) -> Record {
        let mut records = self.records.borrow_mut();
        let record = records.get_mut(submission_id).unwrap();
        record.admission_id = Some(receipt.admission_id.clone());
        record.local_state = "receipt_observed".to_owned();
        record.gateway_status = Some(receipt.status.clone());
        record.execution_generation = receipt.execution_generation;
        record.clone()
    }

    pub fn current(&self, submission_id: &str) -> Option<Record> {
        self.records.borrow().get(submission_id).cloned()
    }
}

impl AdmissionLedger for FakeLedger {
    type Record = Record;
    type Error = Infallible;

    fn get(&self, submission_id: &str) -> Result<Option<Record>, Infallible> {
        Ok(self.current(submission_id))
    }

    fn record_receipt(
        &self,
        submission_id: &str,
        receipt: &Receipt,
        _reconciled: bool,
    ) -> Result<Record, Infallible> {
        Ok(self.record(submission_id, receipt))
    }
}

/// Scripted `prompt.receipt` responses keyed by admission id; `None` is a disconnect.
#[derive(Default)]
pub struct ScriptedReceipts {
    pub responses: BTreeMap<String, Option<Value>>,
    pub calls: Vec<(String, Value)>,
}

impl GatewayRpc for ScriptedReceipts {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls
            .push((method.to_owned(), serde_json::to_value(params).unwrap()));
        let admission = params["admission_id"].as_str().unwrap();
        match self.responses[admission].clone() {
            Some(value) => Ok(value),
            None => Err(GatewayRpcError::Io(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "unavailable",
            ))),
        }
    }
}

impl ScriptedReceipts {
    pub fn methods(&self) -> Vec<&str> {
        self.calls.iter().map(|(name, _)| name.as_str()).collect()
    }
}
