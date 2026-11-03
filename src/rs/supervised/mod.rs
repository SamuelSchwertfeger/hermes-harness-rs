//! Supervised execution pipeline: orchestration, live supervision and recovery.
pub mod live_execution_binding;
pub mod live_execution_coordinator;
pub mod live_human_decision;
pub mod live_reviewer_verdict;
pub mod live_supervised_finalization;
pub mod live_supervised_operator_flow;
pub mod live_supervised_recovery;
pub mod live_supervisor;
pub mod offline_orchestrator;
pub mod orchestration;
pub mod pipeline;
