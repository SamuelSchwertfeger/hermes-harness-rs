//! Frozen wire vocabularies shared by serde, string access, display and parsing.

use std::error::Error;

macro_rules! agreement {
    ($test:ident, $ty:path, $parse:tt, $deserialize:tt, {$($variant:ident => $wire:literal),+ $(,)?}) => {
        #[test]
        fn $test() -> Result<(), Box<dyn Error>> {
            use $ty as Wire;
            for (variant, wire) in [$( (Wire::$variant, $wire), )+] {
                assert_eq!(variant.as_str(), wire);
                assert_eq!(variant.to_string(), wire);
                assert_eq!(serde_json::to_string(&variant)?, format!(r#""{wire}""#));
                agreement!(@parse Wire, variant, wire, $parse);
                agreement!(@deserialize Wire, variant, wire, $deserialize);
            }
            agreement!(@unknown_parse Wire, $parse);
            agreement!(@unknown_json Wire, $deserialize);
            Ok(())
        }
    };
    (@parse $ty:ident, $variant:ident, $wire:ident, []) => {};
    (@parse $ty:ident, $variant:ident, $wire:ident, [$error:literal]) => {
        assert_eq!($wire.parse::<$ty>()?, $variant);
    };
    (@deserialize $ty:ident, $variant:ident, $wire:ident, []) => {};
    (@deserialize $ty:ident, $variant:ident, $wire:ident, [$error:literal]) => {
        assert_eq!(serde_json::from_str::<$ty>(&format!(r#""{}""#, $wire))?, $variant);
    };
    (@unknown_parse $ty:ident, []) => {};
    (@unknown_parse $ty:ident, [$error:literal]) => {
        assert_eq!("unknown".parse::<$ty>().err().map(|error| error.to_string()).as_deref(), Some($error));
    };
    (@unknown_json $ty:ident, []) => {};
    (@unknown_json $ty:ident, [$error:literal]) => {
        assert_eq!(serde_json::from_str::<$ty>(r#""unknown""#).err().map(|error| error.to_string()).as_deref(), Some($error));
    };
}

agreement!(multiagent_runtime_role, hermes_harness::multiagent::runtime::Role, ["Unknown worker role"], [], {
    Coding => "coding",
    CyberWarrior => "cyber-warrior",
});

agreement!(multiagent_runtime_decision, hermes_harness::multiagent::runtime::Decision, ["Invalid human decision"], [], {
    Approve => "approve",
    Reject => "reject",
    RequestChanges => "request-changes",
});

agreement!(multiagent_runtime_task_state, hermes_harness::multiagent::runtime::TaskState, [], [], {
    Completed => "completed",
    Failed => "failed",
    Unknown => "unknown",
    Pending => "pending",
});

agreement!(multiagent_runtime_run_state, hermes_harness::multiagent::runtime::RunState, [], [], {
    PendingHumanDecision => "pending_human_decision",
    FencedOrIncomplete => "fenced_or_incomplete",
});

agreement!(core_delegation_phase, hermes_harness::core::delegation::Phase, [], ["unknown variant `unknown`, expected one of `created`, `worker_done`, `reviewed`, `awaiting_approval` at line 1 column 9"], {
    Created => "created",
    WorkerDone => "worker_done",
    Reviewed => "reviewed",
    AwaitingApproval => "awaiting_approval",
});

agreement!(core_state_stage, hermes_harness::core::state::Stage, [], ["unknown variant `unknown`, expected one of `investigate`, `consolidate-facts`, `open-ticket`, `plan`, `approved-implementation`, `deterministic-verify`, `independent-review`, `ci-and-pr`, `human-review`, `archive-and-baton-pass` at line 1 column 9"], {
    Investigate => "investigate",
    Facts => "consolidate-facts",
    Ticket => "open-ticket",
    Plan => "plan",
    Implement => "approved-implementation",
    Verify => "deterministic-verify",
    Review => "independent-review",
    Pr => "ci-and-pr",
    HumanReview => "human-review",
    Archive => "archive-and-baton-pass",
});

agreement!(core_routing_role, hermes_harness::core::routing::Role, [], ["unknown variant `unknown`, expected one of `planner`, `engineer`, `scout`, `reviewer`, `cyber` at line 1 column 9"], {
    Planner => "planner",
    Engineer => "engineer",
    Scout => "scout",
    Reviewer => "reviewer",
    Cyber => "cyber",
});

agreement!(core_task_reservation_recovery_state, hermes_harness::core::task_reservation::RecoveryState, [], ["unknown variant `unknown`, expected one of `reserved`, `preparing_uncertain`, `prepared`, `dispatch_uncertain`, `worker_recorded`, `manual_intervention` at line 1 column 9"], {
    Reserved => "reserved",
    Preparing => "preparing_uncertain",
    Prepared => "prepared",
    DispatchUncertain => "dispatch_uncertain",
    WorkerRecorded => "worker_recorded",
    ManualIntervention => "manual_intervention",
});

agreement!(gateway_gateway_lifecycle_admission_state, hermes_harness::gateway::gateway_lifecycle::AdmissionState, [], ["unknown variant `unknown`, expected one of `new`, `submit_unknown`, `receipt_recorded`, `cancel_requested`, `cancel_unknown`, `terminal_unverified` at line 1 column 9"], {
    New => "new",
    SubmitUnknown => "submit_unknown",
    ReceiptRecorded => "receipt_recorded",
    CancelRequested => "cancel_requested",
    CancelUnknown => "cancel_unknown",
    TerminalUnverified => "terminal_unverified",
});

agreement!(gateway_hermes_adapter_task_kind, hermes_harness::gateway::hermes_adapter::TaskKind, [], ["unknown variant `unknown`, expected one of `scout`, `plan`, `review`, `cyber` at line 1 column 9"], {
    Scout => "scout",
    Plan => "plan",
    Review => "review",
    Cyber => "cyber",
});

agreement!(services_live_service_journal_service_state, hermes_harness::services::live_service_journal::ServiceState, [], ["unknown variant `unknown`, expected one of `launch_uncertain`, `running_observed`, `stop_uncertain`, `stopped_observed` at line 1 column 9"], {
    LaunchUncertain => "launch_uncertain",
    RunningObserved => "running_observed",
    StopUncertain => "stop_uncertain",
    StoppedObserved => "stopped_observed",
});

agreement!(supervised_live_reviewer_verdict_decision, hermes_harness::supervised::live_reviewer_verdict::Decision, [], [], {
    Accept => "accept",
    ChangesRequested => "changes_requested",
    Reject => "reject",
});

agreement!(supervised_live_supervised_recovery_phase, hermes_harness::supervised::live_supervised_recovery::Phase, ["\"unknown\" is not a valid Phase"], [], {
    Planned => "planned",
    WorkerAdmitted => "worker_admitted",
    WorkerAudited => "worker_audited",
    ReviewerAdmitted => "reviewer_admitted",
    PendingHuman => "pending_human",
    Halted => "halted",
});

agreement!(supervised_orchestration_task_status, hermes_harness::supervised::orchestration::TaskStatus, [], [], {
    Succeeded => "succeeded",
    Failed => "failed",
});

agreement!(sandbox_live_sandbox_plan_role, hermes_harness::sandbox::live_sandbox_plan::Role, [], ["unknown variant `unknown`, expected `worker` or `reviewer` at line 1 column 9"], {
    Worker => "worker",
    Reviewer => "reviewer",
});

agreement!(multiagent_sandbox_tools_sandbox_tool, hermes_harness::multiagent::sandbox_tools::SandboxTool, ["Unknown sandbox tool"], [], {
    ListFiles => "list-files",
    SearchFixed => "search-fixed",
    GitStatus => "git-status",
});
