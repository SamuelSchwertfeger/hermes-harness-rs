//! Pins the crate-root re-exports that form the stable library surface: this file stops
//! compiling if any of them is removed or renamed.

use hermes_harness::{
    Brief, CiError, Decision, DelegationError, ExecutionConfig, Finding, GatewayRpc,
    GatewayRpcError, LiveSupervisorError, LiveSupervisorInputs, Node, OfflineAgent, Phase, Plan,
    Primary, Role, RunState, SessionSnapshot, SupervisorRpc, TaskState, TaskStatus, TaskWorkspace,
    WorkerFactory, WorkflowError, WorktreeBinding, WorktreeDelegationError,
};

#[test]
fn stable_types_are_reexported_at_the_root() {
    let _ = (
        size_of::<Brief>(),
        size_of::<Finding>(),
        size_of::<Phase>(),
        size_of::<DelegationError>(),
        size_of::<TaskWorkspace>(),
        size_of::<WorktreeBinding>(),
        size_of::<WorktreeDelegationError>(),
        size_of::<Plan>(),
        size_of::<Node>(),
        size_of::<Role>(),
        size_of::<Decision>(),
        size_of::<ExecutionConfig>(),
        size_of::<WorkflowError>(),
        size_of::<RunState>(),
        size_of::<TaskState>(),
        size_of::<TaskStatus>(),
        size_of::<GatewayRpcError>(),
        size_of::<SessionSnapshot>(),
        size_of::<LiveSupervisorInputs>(),
        size_of::<LiveSupervisorError>(),
        size_of::<CiError>(),
    );
}

#[test]
fn stable_traits_are_object_safe() {
    let _: Option<&dyn GatewayRpc> = None;
    let _: Option<&dyn SupervisorRpc> = None;
    let _: Option<&dyn OfflineAgent> = None;
    let _: Option<&dyn Primary> = None;
    let _: Option<&dyn WorkerFactory> = None;
}
