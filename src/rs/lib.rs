//! Hermes Harness: ticket-driven, safety-gated multi-agent orchestration for Hermes.
//!
//! Offline workflows use injected transports, RPC implementations, and agents so
//! callers can validate contracts without contacting live services. Live adapters
//! require explicit configuration and authorization at their transport boundaries.
//!
//! # Stable API
//!
//! The items re-exported at the crate root are the supported library surface:
//!
//! - Delegation: [`Brief`], [`Finding`], [`OfflineAgent`], [`Phase`] and
//!   [`DelegationError`], driven by the functions in [`core::delegation`].
//! - Worktree-isolated delegation: [`TaskWorkspace`], [`WorktreeBinding`] and
//!   [`WorktreeDelegationError`], driven by [`git::worktree_delegation`].
//! - Multi-agent runtime: [`Plan`], [`Node`], [`Role`], [`Decision`],
//!   [`ExecutionConfig`], [`Primary`], [`WorkerFactory`] and [`WorkflowError`]
//!   from [`multiagent::runtime`].
//! - Gateway reads: [`GatewayRpc`], [`GatewayRpcError`] and [`SessionSnapshot`].
//! - Live supervision: [`SupervisorRpc`], [`LiveSupervisorInputs`] and
//!   [`LiveSupervisorError`].
//! - CI gating: [`ci`] and its [`CiError`].
//! - Errors reachable from the stable surface: [`git::worktrees::WorktreeError`],
//!   [`core::task_reservation::ReservationError`], [`util::JsonError`] and
//!   [`ci::github_ci_reader::GitHubTransportError`].
//!
//! All stable error enums are `#[non_exhaustive]`.
//!
//! Those modules ([`core::delegation`], [`git::worktree_delegation`],
//! [`multiagent::runtime`], [`gateway::gateway_readonly`],
//! [`supervised::live_supervisor`] and [`ci`]) and the error types listed above
//! are stable. Other public items back the bundled binaries and integration tests;
//! they are internal and may change without notice.
//!
//! Module layout mirrors the Python `hermes_harness` package it was ported from.

pub mod agents;
pub mod broker;
pub mod ci;
pub mod cli;
pub mod core;
pub mod deployment;
pub mod evaluation;
pub mod gateway;
pub mod git;
pub mod multiagent;
pub mod native;
pub mod operator;
pub mod probe;
pub mod sandbox;
pub mod services;
pub mod supervised;
pub mod util;

pub use ci::CiError;
pub use core::delegation::{Brief, DelegationError, Finding, OfflineAgent, Phase};
pub use gateway::gateway_readonly::{GatewayRpc, GatewayRpcError, SessionSnapshot};
pub use git::worktree_delegation::{TaskWorkspace, WorktreeBinding, WorktreeDelegationError};
pub use multiagent::runtime::{
    Decision, ExecutionConfig, Node, Plan, Primary, Role, RunState, TaskState, TaskStatus,
    WorkerFactory, WorkflowError,
};
pub use supervised::live_supervisor::{LiveSupervisorError, LiveSupervisorInputs, SupervisorRpc};
