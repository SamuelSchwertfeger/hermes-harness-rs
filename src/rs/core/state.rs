//! Persistent ticket state with guarded transitions and explicit human approvals.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::util::ordered_map::OrderedMap;
use crate::util::{create_private_dir, pretty_json_ordered, utc_now_iso};

/// Errors raised by ticket state handling.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Python `ValueError`: forbidden transition, missing evidence, bad slug or ID mismatch.
    #[error("{0}")]
    Value(String),
    /// Python `PermissionError`: a human approval gate is not satisfied.
    #[error("{0}")]
    Permission(String),
    /// Python `FileExistsError`: the ticket already exists.
    #[error("{}", .0.display())]
    Exists(PathBuf),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON failure (including an unknown stage value).
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

crate::util::wire_enum! {
    /// Lifecycle stage of a ticket.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Stage {
        /// `investigate`
        Investigate => "investigate",
        /// `consolidate-facts`
        Facts => "consolidate-facts",
        /// `open-ticket`
        Ticket => "open-ticket",
        /// `plan`
        Plan => "plan",
        /// `approved-implementation`
        Implement => "approved-implementation",
        /// `deterministic-verify`
        Verify => "deterministic-verify",
        /// `independent-review`
        Review => "independent-review",
        /// `ci-and-pr`
        Pr => "ci-and-pr",
        /// `human-review`
        HumanReview => "human-review",
        /// `archive-and-baton-pass`
        Archive => "archive-and-baton-pass",
    }

    /// The exact Python string value.
    as_str;
}

impl Stage {
    /// Every stage, in declaration order.
    pub const ALL: [Self; 10] = [
        Self::Investigate,
        Self::Facts,
        Self::Ticket,
        Self::Plan,
        Self::Implement,
        Self::Verify,
        Self::Review,
        Self::Pr,
        Self::HumanReview,
        Self::Archive,
    ];
}

impl clap::ValueEnum for Stage {
    fn value_variants<'a>() -> &'a [Self] {
        &Self::ALL
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}

/// Stages reachable from `stage` (Python `NEXT`).
#[must_use]
pub const fn next_stages(stage: Stage) -> &'static [Stage] {
    match stage {
        Stage::Investigate => &[Stage::Facts],
        Stage::Facts => &[Stage::Ticket],
        Stage::Ticket => &[Stage::Plan],
        Stage::Plan => &[Stage::Implement],
        Stage::Implement => &[Stage::Verify],
        Stage::Verify => &[Stage::Review, Stage::Implement],
        Stage::Review => &[Stage::Pr, Stage::Plan],
        Stage::Pr => &[Stage::HumanReview, Stage::Implement],
        Stage::HumanReview => &[Stage::Archive, Stage::Plan],
        Stage::Archive => &[],
    }
}

/// Human approval required to enter `stage` (Python `APPROVAL_GATES`).
#[must_use]
pub const fn approval_gate(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Implement => Some("plan"),
        Stage::Archive => Some("merge"),
        _ => None,
    }
}

/// Evidence key required to enter `stage`.
const fn required_evidence(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Facts => Some("facts"),
        Stage::Plan => Some("plan"),
        Stage::Verify => Some("commit"),
        Stage::Review => Some("verification"),
        Stage::Pr => Some("review"),
        Stage::HumanReview => Some("ci"),
        Stage::Archive => Some("merged_pr"),
        _ => None,
    }
}

/// A persisted ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    /// Lower-case slug.
    pub id: String,
    /// One-line summary.
    pub summary: String,
    /// Current stage.
    pub stage: Stage,
    /// Paths the ticket may touch.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Human approvals by gate name.
    #[serde(default)]
    pub approvals: OrderedMap<bool>,
    /// Evidence by key.
    #[serde(default)]
    pub evidence: OrderedMap<String>,
    /// Transition history entries (`at`, `from`, `to`).
    #[serde(default)]
    pub history: Vec<OrderedMap<String>>,
}

impl Ticket {
    /// A fresh ticket at `Stage::Investigate`.
    #[must_use]
    pub fn new(id: &str, summary: &str) -> Self {
        Self {
            id: id.to_owned(),
            summary: summary.to_owned(),
            stage: Stage::Investigate,
            allowed_paths: Vec::new(),
            approvals: OrderedMap::new(),
            evidence: OrderedMap::new(),
            history: Vec::new(),
        }
    }

    /// Move to `target` if the transition, approval gate and evidence rules allow it.
    ///
    /// # Errors
    /// The requested stage transition is not allowed.
    pub fn advance(&mut self, target: Stage) -> Result<(), StateError> {
        if !next_stages(self.stage).contains(&target) {
            return Err(StateError::Value(format!(
                "Transition forbidden: {} -> {target}",
                self.stage
            )));
        }
        let gate = approval_gate(target)
            .filter(|gate| !self.approvals.get(gate).copied().unwrap_or(false));
        if let Some(gate) = gate {
            return Err(StateError::Permission(format!(
                "Human approval required: {gate}"
            )));
        }
        let missing = required_evidence(target)
            .filter(|key| self.evidence.get(key).is_none_or(String::is_empty));
        if let Some(required) = missing {
            return Err(StateError::Value(format!("Missing evidence: {required}")));
        }
        let old = self.stage;
        self.stage = target;
        self.history.push(OrderedMap::from_iter([
            ("at".to_owned(), utc_now_iso()),
            ("from".to_owned(), old.as_str().to_owned()),
            ("to".to_owned(), target.as_str().to_owned()),
        ]));
        Ok(())
    }
}

fn is_ticket_slug(text: &str) -> bool {
    let mut chars = text.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Path of a ticket's `state.json` under `root`, after validating the slug.
///
/// # Errors
/// The ticket ID contains unsupported characters or escapes the state directory.
pub fn ticket_path(root: &Path, ticket_id: &str) -> Result<PathBuf, StateError> {
    if !is_ticket_slug(ticket_id) {
        return Err(StateError::Value(
            "Ticket ID must be a lower-case slug".to_owned(),
        ));
    }
    Ok(root.join("tickets").join(ticket_id).join("state.json"))
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match create_private_dir(path) {
        Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => Err(err),
        Err(_) if !path.is_dir() => Err(std::io::ErrorKind::AlreadyExists.into()),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn open_private_new(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a uniquely named `.state-*` file in `dir` (like `tempfile.mkstemp`).
fn create_temp(dir: &Path) -> std::io::Result<(File, PathBuf)> {
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".state-{:x}{nanos:x}{count:x}", std::process::id()));
        match open_private_new(&path) {
            Ok(file) => return Ok((file, path)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
}

/// Atomically persist `ticket` to its `state.json` (fsync, then rename).
///
/// # Errors
/// The task state or stage transition is invalid, or its files cannot be read, decoded, or written.
pub fn save(root: &Path, ticket: &Ticket) -> Result<(), StateError> {
    let path = ticket_path(root, &ticket.id)?;
    let parent = path.parent().unwrap_or(root);
    ensure_private_dir(parent)?;
    let payload = format!("{}\n", pretty_json_ordered(ticket)?);
    let (mut file, temp) = create_temp(parent)?;
    let written = file
        .write_all(payload.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            fs::rename(&temp, &path)
        });
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    Ok(written?)
}

/// Load a ticket, rejecting an unknown stage or a mismatched ID.
///
/// # Errors
/// The task state or stage transition is invalid, or its files cannot be read, decoded, or written.
pub fn load(root: &Path, ticket_id: &str) -> Result<Ticket, StateError> {
    let text = fs::read_to_string(ticket_path(root, ticket_id)?)?;
    let ticket: Ticket = serde_json::from_str(&text)?;
    if ticket.id != ticket_id {
        return Err(StateError::Value("Ticket ID mismatch".to_owned()));
    }
    Ok(ticket)
}

/// Create a new ticket directory with its skeleton files; refuse to overwrite.
///
/// # Errors
/// The task state or stage transition is invalid, or its files cannot be read, decoded, or written.
pub fn create(root: &Path, ticket_id: &str, summary: &str) -> Result<Ticket, StateError> {
    let path = ticket_path(root, ticket_id)?;
    if path.exists() {
        return Err(StateError::Exists(path));
    }
    let ticket = Ticket::new(ticket_id, summary);
    save(root, &ticket)?;
    let base = path.parent().unwrap_or(root);
    for dirname in ["briefs", "review", "handoffs", "runs"] {
        match fs::create_dir(base.join(dirname)) {
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => return Err(err.into()),
            _ => {}
        }
    }
    for (name, heading) in [
        ("facts.md", "Facts and unresolved questions"),
        ("plan.md", "Implementation plan and verification contract"),
        ("REPORT.md", "Outcome report"),
    ] {
        fs::write(base.join(name), format!("# {heading}\n"))?;
    }
    Ok(ticket)
}
