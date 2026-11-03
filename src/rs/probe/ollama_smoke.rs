//! Bounded local Ollama worker/reviewer acceptance probe, with no file application.
use super::ProbeError;
use crate::multiagent::{
    local_inference::LocalEndpoint,
    model_pipeline::generate_and_review,
    ollama_native::{NativeOllamaJsonTransport, proposal_schema, review_schema},
};
use serde_json::{Value, json};

/// Run the fixed worker/reviewer transaction against loopback Ollama.
pub fn run() -> Result<Value, ProbeError> {
    let url = "http://127.0.0.1:11434/v1/chat/completions";
    let worker = LocalEndpoint::new(url, "qwen2.5-coder:7b")
        .with_timeout_seconds(120.0)
        .with_max_tokens(2048);
    let reviewer = LocalEndpoint::new(url, "qwen3:4b")
        .with_timeout_seconds(120.0)
        .with_max_tokens(512);
    let transport = NativeOllamaJsonTransport::new()
        .with_schema(&worker.model, proposal_schema())
        .with_schema(&reviewer.model, review_schema());
    let result = generate_and_review("ollama-native-acceptance-001", "Create exactly one file smoke_math.py, with valid indented Python defining add(a: int, b: int) -> int returning a + b.", &["smoke_math.py".into()], &worker, &reviewer, &transport, true).map_err(ProbeError::runtime)?;
    let files: Vec<_> = result
        .proposal
        .files
        .iter()
        .map(|f| json!({"path":f.path,"sha256":f.sha256,"content":f.content}))
        .collect();
    Ok(
        json!({"proposal":{"task_id":result.proposal.task_id,"files":files},"review":{"model":result.review.reviewer_model,"verdict":result.review.verdict.as_str(),"findings":result.review.findings},"files_applied":false,"python_syntax_checked":false}),
    )
}
