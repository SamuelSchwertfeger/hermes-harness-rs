//! Read-only gateway contract validation and explicit session listing.
use super::{Args, Connector, ProbeError};
use crate::gateway::{
    gateway_contract_validation::validate_rpc,
    gateway_session_discovery::{SessionScope, list_sessions},
};
use serde_json::{Value, json};

/// Inspect only an explicitly attached existing gateway.
pub fn run<C: Connector>(args: &Args, connector: &mut C) -> Result<Value, ProbeError> {
    if args.text("profile-home").trim().is_empty() {
        return Err(ProbeError::value("profile_home must be a nonblank path"));
    }
    let limit = if args.has("list-sessions") && args.has("limit") {
        args.text("limit")
            .parse::<usize>()
            .map_err(|_| ProbeError::value("limit must be between 1 and 20"))?
    } else {
        10
    };
    if args.has("list-sessions") && !(1..=20).contains(&limit) {
        return Err(ProbeError::value("limit must be between 1 and 20"));
    }
    let mut rpc = args.connect(connector)?;
    if args.has("list-sessions") {
        let scope = if args.text("list-sessions") == "live" {
            SessionScope::Live
        } else {
            SessionScope::Stored
        };
        let report = list_sessions(&mut rpc, scope, limit).map_err(ProbeError::runtime)?;
        let sessions: Vec<_> = report
            .sessions
            .iter()
            .map(|s| json!({"session_id":s.session_id,"running":s.running}))
            .collect();
        Ok(
            json!({"result":"verified", "session_listing":{"scope":report.scope.as_str(),"sessions":sessions,"limit":report.limit}}),
        )
    } else {
        let session = args.has("session-id").then(|| args.text("session-id"));
        let report = validate_rpc(&mut rpc, session).map_err(ProbeError::runtime)?;
        Ok(json!({"result":"verified", "contract":{
            "ping_verified":report.ping_verified,"sampled_live_session_count":report.sampled_live_session_count,
            "info_verified":report.info_verified,"replay_epoch_present":report.replay_epoch_present,
            "replay_latest_seq":report.replay_latest_seq,"replay_consistent":report.replay_consistent
        }}))
    }
}
