//! Opt-in local inference with an immutable review receipt and no application authority.

use crate::util::PathLexical as _;
use crate::util::splitlines_keepends;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::multiagent::local_inference::{LocalEndpoint, Transport};
use crate::multiagent::model_pipeline::generate_and_review;
use crate::multiagent::ollama_native::{NativeOllamaJsonTransport, proposal_schema, review_schema};
use crate::multiagent::runtime::WorkflowError;
use crate::native::workflows::{NativeError, repository};
use crate::util::{pretty_json_value, py_path_str};

/// Maximum originating session length.
pub const MAX_SESSION_ID_LENGTH: usize = 128;

/// Inputs for one new-file proposal and independent review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveReview<'a> {
    /// Source checkout.
    pub repo: &'a Path,
    /// Existing evidence root.
    pub state: &'a Path,
    /// Originating session.
    pub session: &'a str,
    /// Task identity.
    pub task_id: &'a str,
    /// Requested objective.
    pub objective: &'a str,
    /// Exact new-file ownership.
    pub owned_path: &'a str,
    /// Explicit inference grant.
    pub authorized: bool,
}

fn denied<T>(message: &str) -> Result<T, NativeError> {
    Err(WorkflowError::msg(message).into())
}

pub(crate) fn new_file_diff(content: &str, path: &str) -> String {
    let lines = splitlines_keepends(content);
    if lines.is_empty() {
        return String::new();
    }
    let range = if lines.len() == 1 {
        "1".to_owned()
    } else {
        format!("1,{}", lines.len())
    };
    let mut diff = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +{range} @@\n");
    for line in lines {
        diff.push('+');
        diff.push_str(line);
    }
    diff
}

fn head(root: &Path) -> Result<String, WorkflowError> {
    #[cfg(unix)]
    let executable = "/usr/bin/git";
    #[cfg(not(unix))]
    let executable = "git";
    let mut child = Command::new(executable)
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stream) = stdout.as_mut() {
            stream.read_to_end(&mut bytes)?;
        }
        Ok::<_, std::io::Error>(bytes)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(
                std::io::Error::new(std::io::ErrorKind::TimedOut, "git command timed out").into(),
            );
        }
        thread::sleep(Duration::from_millis(2));
    };
    let bytes = reader
        .join()
        .map_err(|_| std::io::Error::other("Git output reader failed"))??;
    if !status.success() {
        return Err(WorkflowError::msg("Cannot resolve repository HEAD"));
    }
    Ok(crate::util::py_strip(&String::from_utf8_lossy(&bytes)).to_owned())
}

/// Request one new-file proposal and independent review, then save evidence.
/// # Errors
/// Rejects unauthorized, invalid, existing destinations and inference or receipt failures.
pub fn reviewed_live_task(request: &LiveReview<'_>) -> Result<Value, NativeError> {
    reviewed_live_task_with(request, None)
}

/// Review a live task with a caller-supplied inference transport.
/// # Errors
/// Same refusals as [`reviewed_live_task`].
pub fn reviewed_live_task_with(
    request: &LiveReview<'_>,
    transport: Option<&dyn Transport>,
) -> Result<Value, NativeError> {
    let r = request;
    if !r.authorized {
        return denied("Explicit inference authorization required");
    }
    if !crate::core::state::is_ticket_slug(r.task_id)
        || r.session.chars().all(crate::util::is_py_space)
        || r.session.chars().count() > MAX_SESSION_ID_LENGTH
    {
        return denied("Invalid task or session identity");
    }
    if !(1..=4096).contains(&r.objective.chars().count()) {
        return denied("Invalid objective length");
    }
    if r.state.is_symlink_lexical() || !r.state.is_dir() {
        return denied("State must be an existing real directory");
    }
    let root = repository(r.repo)?;
    let destination = root.join(r.owned_path);
    if destination.exists() || destination.is_symlink_lexical() {
        return denied("Live new-file mode refuses existing destination");
    }
    let receipt = r.state.join(format!("{}.json", r.task_id));
    if receipt.exists() || receipt.is_symlink_lexical() {
        return denied("Task receipt already exists; refusing redispatch");
    }
    let url = "http://127.0.0.1:11434/v1/chat/completions";
    let worker = LocalEndpoint::new(url, "qwen2.5-coder:7b").with_timeout_seconds(120.0);
    let reviewer = LocalEndpoint::new(url, "qwen3:4b")
        .with_timeout_seconds(120.0)
        .with_max_tokens(512);
    let native = NativeOllamaJsonTransport::new()
        .with_schema(&worker.model, proposal_schema())
        .with_schema(&reviewer.model, review_schema());
    let result = generate_and_review(
        r.task_id,
        r.objective,
        &[r.owned_path.to_owned()],
        &worker,
        &reviewer,
        transport.unwrap_or(&native),
        true,
    )?;
    let mut files = Vec::new();
    for item in &result.proposal.files {
        let diff = new_file_diff(&item.content, &item.path);
        if diff.chars().count() > 64_000 {
            return denied("Proposal diff exceeds inspection limit");
        }
        files.push(json!({"path":item.path,"sha256":item.sha256,"diff":diff}));
    }
    let mut evidence = json!({"action":"agents-assemble","mode":"live-reviewed-proposal-only","session_id":r.session,"task_id":r.task_id,"repository":root.to_string_lossy(),"head":head(&root)?,"worker_model":worker.model,"reviewer_model":reviewer.model,"verdict":result.review.verdict.as_str(),"findings":result.review.findings,"files":files,"applied":false,"executed_generated_code":false,"merged":false,"human_decision_pending":true});
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&receipt).map_err(WorkflowError::from)?;
    let payload = format!(
        "{}\n",
        pretty_json_value(&evidence).map_err(WorkflowError::from)?
    );
    // Python text-mode output translates newlines on Windows.
    #[cfg(windows)]
    let payload = payload.replace('\n', "\r\n");
    file.write_all(payload.as_bytes())
        .map_err(WorkflowError::from)?;
    if let Some(fields) = evidence.as_object_mut() {
        fields.insert("receipt".to_owned(), json!(py_path_str(&receipt)));
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    #[test]
    fn task_identity_uses_ticket_slug_rule() {
        let request = super::LiveReview {
            repo: std::path::Path::new("."),
            state: std::path::Path::new("."),
            session: "session",
            task_id: "Foo_1",
            objective: "test",
            owned_path: "example.py",
            authorized: true,
        };
        let result = super::reviewed_live_task_with(&request, None);
        assert!(result.is_err_and(|error| error.to_string().contains("Invalid task")));
    }
}
