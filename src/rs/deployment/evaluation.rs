//! Offline evidence-based model comparison; never executes claimed tool calls.
use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::multiagent::runtime::WorkflowError;
use crate::util::ordered_map::OrderedMap;
use crate::util::sha256_hex;

/// Minimum tool attempts.
pub const MIN_TOOL_ATTEMPTS: u32 = 5;
/// Required verified success fraction.
pub const MIN_TOOL_SUCCESS_FRACTION: f64 = 0.95;
/// Maximum trace count.
pub const MAX_TRACES: usize = 512;
/// Maximum trace size.
pub const MAX_TRACE_BYTES: u64 = 128 * 1024;
/// An externally supplied sample and optional terminal receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalSample {
    /// Model identity.
    pub model: String,
    /// Task description.
    pub task: String,
    /// Session identity.
    pub session: String,
    /// Expected tool name.
    pub expected_tool: String,
    /// Untrusted response.
    pub response: Value,
    /// Bound receipt path.
    #[serde(default)]
    pub receipt_path: Option<PathBuf>,
    /// Receipt digest.
    #[serde(default)]
    pub receipt_sha256: String,
    /// Policy denied execution.
    #[serde(default)]
    pub policy_denied: bool,
    /// Duration in milliseconds.
    #[serde(default)]
    pub duration_ms: i64,
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: i64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: i64,
}
fn receipt(sample: &EvalSample) -> Result<&'static str, WorkflowError> {
    if sample.policy_denied {
        return Ok("policy_denial");
    }
    let Some(calls) = sample.response.get("tool_calls").and_then(Value::as_array) else {
        return Ok("missing_or_malformed_tool_call");
    };
    let Some(call) = calls.first().filter(|v| calls.len() == 1 && v.is_object()) else {
        return Ok("missing_or_malformed_tool_call");
    };
    let Some(function) = call.get("function").filter(|v| v.is_object()) else {
        return Ok("malformed_tool_schema");
    };
    if function.get("name").and_then(Value::as_str) != Some(&sample.expected_tool) {
        return Ok("wrong_tool_name");
    }
    let arguments = function.get("arguments").cloned().unwrap_or(Value::Null);
    let decoded = if let Some(text) = arguments.as_str() {
        match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => return Ok("malformed_tool_arguments"),
        }
    } else {
        arguments
    };
    let Some(command) = decoded.get("command").and_then(Value::as_str) else {
        return Ok("malformed_tool_arguments");
    };
    let Some(path) = sample
        .receipt_path
        .as_ref()
        .filter(|_| !sample.receipt_sha256.is_empty())
    else {
        return Ok("missing_terminal_receipt");
    };
    if path.is_symlink_lexical() || !path.is_file() || path.metadata()?.len() > MAX_TRACE_BYTES {
        return Ok("invalid_terminal_receipt");
    }
    let content = std::fs::read(path)?;
    if sha256_hex(&content) != sample.receipt_sha256 {
        return Ok("tampered_terminal_receipt");
    }
    let Ok(receipt) = serde_json::from_slice::<Value>(&content) else {
        return Ok("invalid_terminal_receipt");
    };
    if !receipt.is_object()
        || receipt.get("session_id").and_then(Value::as_str) != Some(&sample.session)
        || receipt.get("command").and_then(Value::as_str) != Some(command)
        || receipt.get("tool").and_then(Value::as_str) != Some(&sample.expected_tool)
    {
        return Ok("terminal_receipt_binding_mismatch");
    }
    if receipt.get("policy_denied") == Some(&Value::Bool(true)) {
        return Ok("policy_denial");
    }
    if receipt.get("executed") != Some(&Value::Bool(true)) {
        return Ok("unconfirmed_tool_execution");
    }
    // Python compares False equal to zero.
    if !receipt.get("exit_code").is_some_and(|v| {
        v == &Value::from(0) || v == &Value::Bool(false) || v.as_f64() == Some(0.0)
    }) {
        return Ok("tool_execution_error");
    }
    Ok("reported_tool_success_requires_origin_review")
}
/// Per-model counters, in Python insertion order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationRow {
    /// Number of samples.
    pub total: u32,
    /// Receipt categories in first-seen order.
    pub categories: OrderedMap<u32>,
    /// Origin-verified successes.
    pub verified_successes: u32,
    /// Total duration.
    pub duration_ms: i64,
    /// Total input tokens.
    pub input_tokens: i64,
    /// Total output tokens.
    pub output_tokens: i64,
    /// Origin-verified fraction.
    pub success_fraction: f64,
    /// Sample floor reached.
    pub meets_sample_floor: bool,
    /// Automatic worker eligibility.
    pub tool_worker_eligible: bool,
    /// Operator inspection is mandatory.
    pub operator_origin_review_required: bool,
}
impl Default for EvaluationRow {
    fn default() -> Self {
        Self {
            total: 0,
            categories: OrderedMap::new(),
            verified_successes: 0,
            duration_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            success_fraction: 0.0,
            meets_sample_floor: false,
            tool_worker_eligible: false,
            operator_origin_review_required: true,
        }
    }
}
/// Offline report in Python field order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationReport {
    /// Report mode.
    pub mode: &'static str,
    /// Sample count.
    pub samples: usize,
    /// Results in first-seen model order.
    pub results: OrderedMap<EvaluationRow>,
    /// Always false.
    pub generated_code_executed: bool,
    /// Always false.
    pub gateway_modified: bool,
    /// Always false.
    pub unattended_worker_admission: bool,
}
/// Evaluate evidence without granting worker admission.
/// # Errors
/// Rejects invalid counts or identities and unreadable receipt files.
pub fn evaluate_samples(samples: &[EvalSample]) -> Result<EvaluationReport, WorkflowError> {
    if samples.is_empty() || samples.len() > MAX_TRACES {
        return Err(WorkflowError::msg("Evaluation sample count invalid"));
    }
    let mut matrix = OrderedMap::<EvaluationRow>::new();
    for item in samples {
        if item.model.is_empty() || item.session.is_empty() || item.duration_ms < 0 {
            return Err(WorkflowError::msg("Invalid evaluation sample identity"));
        }
        let status = receipt(item)?;
        let mut row = matrix.get(&item.model).cloned().unwrap_or_default();
        row.total += 1;
        row.categories.insert(
            status.to_owned(),
            row.categories.get(status).copied().unwrap_or(0) + 1,
        );
        row.duration_ms += item.duration_ms;
        row.input_tokens += item.input_tokens;
        row.output_tokens += item.output_tokens;
        row.success_fraction = f64::from(row.verified_successes) / f64::from(row.total);
        row.meets_sample_floor = row.total >= MIN_TOOL_ATTEMPTS;
        row.tool_worker_eligible =
            row.meets_sample_floor && row.success_fraction >= MIN_TOOL_SUCCESS_FRACTION;
        matrix.insert(item.model.clone(), row);
    }
    Ok(EvaluationReport {
        mode: "offline-model-evaluation",
        samples: samples.len(),
        results: matrix,
        generated_code_executed: false,
        gateway_modified: false,
        unattended_worker_admission: false,
    })
}
/// Load bounded offline fixtures with filename-only receipt references.
/// # Errors
/// Rejects invalid paths, malformed fixtures, and receipt traversal.
pub fn load_samples(path: &Path, evidence_root: &Path) -> Result<Vec<EvalSample>, WorkflowError> {
    if evidence_root.is_symlink_lexical()
        || !evidence_root.is_dir()
        || path.is_symlink_lexical()
        || !path.is_file()
        || path.metadata()?.len() > MAX_TRACE_BYTES
    {
        return Err(WorkflowError::msg("Invalid evaluation fixture path"));
    }
    let raw: Value = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| WorkflowError::msg("Malformed evaluation fixture"))?;
    let items = raw
        .as_array()
        .ok_or_else(|| WorkflowError::msg("Evaluation fixtures must be a list"))?;
    items
        .iter()
        .map(|item| load_sample(item, evidence_root))
        .collect()
}
fn load_sample(item: &Value, evidence_root: &Path) -> Result<EvalSample, WorkflowError> {
    let mut fields = item
        .as_object()
        .cloned()
        .ok_or_else(|| WorkflowError::msg("Invalid evaluation entry"))?;
    let receipt_path = match fields.get("receipt_file") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.contains(['/', '\\']) => Some(evidence_root.join(name)),
        _ => {
            return Err(WorkflowError::msg(
                "Evaluation receipt path must be a filename",
            ));
        }
    };
    if !fields.get("response").is_some_and(Value::is_object) {
        return Err(WorkflowError::msg("Evaluation response must be an object"));
    }
    fields.remove("receipt_path");
    let mut sample: EvalSample = serde_json::from_value(Value::Object(fields))
        .map_err(|_| WorkflowError::msg("Invalid evaluation sample fields"))?;
    sample.receipt_path = receipt_path;
    Ok(sample)
}
