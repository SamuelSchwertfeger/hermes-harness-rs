//! Validate a bounded independent reviewer verdict; gateway success is insufficient.

use crate::util::is_py_space;
use serde::Serialize;
use serde_json::Value;

/// Largest accepted verdict payload in bytes.
pub const MAX_VERDICT_BYTES: usize = 16384;
/// Most findings a verdict may carry.
pub const MAX_FINDINGS: usize = 32;
/// Longest single finding, in characters after stripping.
pub const MAX_FINDING_CHARACTERS: usize = 500;

/// Untrusted reviewer output cannot support a human decision request.
#[derive(Debug, thiserror::Error)]
pub enum ReviewerVerdictError {
    /// The verdict violates the bounded, evidence-bound schema.
    #[error("{0}")]
    Invalid(String),
}

fn fail<T>(msg: &str) -> Result<T, ReviewerVerdictError> {
    Err(ReviewerVerdictError::Invalid(msg.to_owned()))
}

crate::util::wire_enum! {
    /// Reviewer decision; never a human approval.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum Decision {
        /// Reviewer accepts the audited work.
        Accept => "accept",
        /// Reviewer asks for changes.
        ChangesRequested => "changes_requested",
        /// Reviewer rejects the work.
        Reject => "reject",
    }

    /// The Python string value of the decision.
    as_str;
}

impl Decision {
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "accept" => Some(Self::Accept),
            "changes_requested" => Some(Self::ChangesRequested),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

/// Strict, evidence-bound reviewer response (not an approval).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewerVerdict {
    /// Task identity.
    pub task_id: String,
    /// Worker submission identity.
    pub worker_submission_id: String,
    /// Audit digest the verdict is bound to.
    pub audit_sha256: String,
    /// Reviewer decision.
    pub decision: Decision,
    /// Bounded findings.
    pub findings: Vec<String>,
}

/// Expected identity of the Git evidence being independently reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictBinding {
    /// Task identity.
    pub task_id: String,
    /// Worker submission identity.
    pub worker_submission_id: String,
    /// Audit digest.
    pub audit_sha256: String,
}

/// Exact key set of a verdict document.
const EXPECTED: [&str; 5] = [
    "task_id",
    "worker_submission_id",
    "audit_sha256",
    "decision",
    "findings",
];

fn decode(response: &str) -> Result<serde_json::Map<String, Value>, ReviewerVerdictError> {
    if response.is_empty() || response.len() > MAX_VERDICT_BYTES {
        return fail("Reviewer verdict is empty or exceeds bounded size");
    }
    let Ok(document) = serde_json::from_str::<Value>(response) else {
        return fail("Reviewer verdict is not valid JSON");
    };
    match document {
        Value::Object(map)
            if map.len() == EXPECTED.len() && EXPECTED.iter().all(|key| map.contains_key(*key)) =>
        {
            Ok(map)
        }
        _ => fail("Reviewer verdict schema mismatch"),
    }
}

fn validate_findings(
    decision: &Value,
    findings: &Value,
) -> Result<(Decision, Vec<String>), ReviewerVerdictError> {
    let Some(decision) = Decision::parse(decision) else {
        return fail("Reviewer verdict decision is invalid");
    };
    let bounded = "Reviewer findings must be bounded nonempty text";
    let Some(items) = findings
        .as_array()
        .filter(|items| items.len() <= MAX_FINDINGS)
    else {
        return fail(bounded);
    };
    let mut texts = Vec::with_capacity(items.len());
    for item in items {
        let valid = item.as_str().filter(|text| {
            let length = text.trim_matches(is_py_space).chars().count();
            0 < length && length <= MAX_FINDING_CHARACTERS
        });
        let Some(text) = valid else {
            return fail(bounded);
        };
        texts.push(text.to_owned());
    }
    if decision != Decision::Accept && texts.is_empty() {
        return fail("Nonaccepting reviewer verdict requires findings");
    }
    Ok((decision, texts))
}

/// Reject extra fields, identity drift, non-JSON prose, and oversized payloads.
///
/// # Errors
/// The reviewer verdict is malformed or does not match the expected task and handoff binding.
pub fn parse_reviewer_verdict(
    response: &str,
    binding: &VerdictBinding,
) -> Result<ReviewerVerdict, ReviewerVerdictError> {
    let document = decode(response)?;
    let field = |key: &str| document.get(key).and_then(Value::as_str);
    let bound = field("task_id") == Some(binding.task_id.as_str())
        && field("worker_submission_id") == Some(binding.worker_submission_id.as_str())
        && field("audit_sha256") == Some(binding.audit_sha256.as_str());
    if !bound {
        return fail("Reviewer verdict is not bound to audited worker evidence");
    }
    let (decision, findings) = validate_findings(
        document.get("decision").unwrap_or(&Value::Null),
        document.get("findings").unwrap_or(&Value::Null),
    )?;
    Ok(ReviewerVerdict {
        task_id: binding.task_id.clone(),
        worker_submission_id: binding.worker_submission_id.clone(),
        audit_sha256: binding.audit_sha256.clone(),
        decision,
        findings,
    })
}
