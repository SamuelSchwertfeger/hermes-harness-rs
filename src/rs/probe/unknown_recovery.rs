//! Read-only recovery of a synthetic, never-submitted identity.
use super::{Args, Connector, ProbeError};
use crate::core::reconciliation::ReconciliationStatus;
use crate::gateway::{
    admission_store::{AdmissionStore, LocalState},
    gateway_async_recovery::recover_with_async_rpc,
    gateway_identity::Submission,
};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) struct Scratch(pub PathBuf);

impl Scratch {
    pub(crate) fn new(prefix: &str) -> Result<Self, ProbeError> {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes).map_err(ProbeError::runtime)?;
        // 128 random bits plus exclusive 0700 creation: safe in a shared temp dir.
        let path = std::env::temp_dir().join(format!("{prefix}-{}", hex::encode(bytes)));
        crate::util::create_private_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only known probe-owned files; no recursive deletion or symlink traversal.
        for name in [
            "admissions.sqlite",
            "admissions.sqlite-wal",
            "admissions.sqlite-shm",
            "admissions.sqlite-journal",
        ] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Check that a disposable unknown record remains unknown after read-only inspection.
pub fn run<C: Connector>(args: &Args, connector: &mut C) -> Result<Value, ProbeError> {
    if args.text("profile-home").trim().is_empty() || args.text("session-id").trim().is_empty() {
        return Err(ProbeError::value(
            "profile-home and session-id must be nonblank",
        ));
    }
    let mut rpc = args.connect(connector)?;
    let scratch = Scratch::new("hermes-recovery-probe")?;
    let store =
        AdmissionStore::new(&scratch.0.join("admissions.sqlite")).map_err(ProbeError::runtime)?;
    let submission_id = format!(
        "never-submitted-{}",
        scratch.0.file_name().unwrap_or_default().to_string_lossy()
    );
    store
        .prepare(&Submission::new(args.text("session-id"), &submission_id))
        .map_err(ProbeError::runtime)?;
    store
        .mark_outcome_unknown(&submission_id)
        .map_err(ProbeError::runtime)?;
    let status =
        recover_with_async_rpc(&store, &submission_id, &mut rpc).map_err(ProbeError::runtime)?;
    let record = store
        .get(&submission_id)
        .map_err(ProbeError::runtime)?
        .ok_or_else(|| ProbeError::runtime("Disposable admission record disappeared"))?;
    let safe = status == ReconciliationStatus::StillUnknown
        && record.local_state == LocalState::OutcomeUnknown
        && record.admission_id.is_none();
    if !safe {
        return Err(ProbeError::runtime(
            "Unexpected synthetic admission resolution; inspect gateway behavior",
        ));
    }
    Ok(
        json!({"result":"verified","scenario":"synthetic_never_submitted","recovery_status":"still_unknown","ledger_unchanged":safe}),
    )
}
