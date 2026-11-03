#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline deployment qualification parity tests.
use clap::Parser;
use hermes_harness::deployment::bridge::add_deployment_commands;
use hermes_harness::deployment::evaluation::{EvalSample, evaluate_samples, load_samples};
use hermes_harness::deployment::gpu::gpu_lane;
use hermes_harness::deployment::gpu::{
    GPUPolicy, GPUUnit, SwitchRequest, Systemd, switch_gpu_slot,
};
use hermes_harness::deployment::preflight::{PreflightHost, hp_preflight_with};
use hermes_harness::deployment::providers::{
    ConfiguredEndpoint, HttpRequest, OpenAICompatibleTransport, Opener, ProviderError,
    ProviderProbe, ProviderRegistry, ProviderSpec, load_registry,
};
use hermes_harness::multiagent::local_inference::LocalEndpoint;
use hermes_harness::multiagent::runtime::WorkflowError;
use hermes_harness::native::skill_bridge::{Args as SkillBridgeArgs, BridgeCommand};
use hermes_harness::util::{ordered_json_spaced, sha256_hex};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;
fn remote_spec() -> ProviderSpec {
    ProviderSpec {
        name: "long".into(),
        backend: "llama.cpp".into(),
        model: "qwen35-2b".into(),
        url: "https://llm-long.example.home.arpa/v1/chat/completions".into(),
        context_tokens: 65536,
        key_env: "LOCAL_LLM_API_KEY".into(),
        tool_calls: false,
        unit: "llama-long.service".into(),
        gpu_host: "gpu-host-01".into(),
        max_concurrency: 1,
    }
}
fn local_spec(model: &str) -> ProviderSpec {
    ProviderSpec {
        name: "xps-coding".into(),
        backend: "ollama".into(),
        model: model.into(),
        url: "http://127.0.0.1:11435/v1/chat/completions".into(),
        context_tokens: 4096,
        key_env: String::new(),
        tool_calls: false,
        unit: String::new(),
        gpu_host: String::new(),
        max_concurrency: 1,
    }
}
fn error<T: std::fmt::Debug, E: std::fmt::Display>(result: Result<T, E>, expected: &str) {
    assert!(result.err().unwrap().to_string().contains(expected));
}
#[derive(Default)]
struct FakeOpener {
    headers: RefCell<Vec<(String, String)>>,
    wrong: bool,
}
impl Opener for FakeOpener {
    fn open(&self, request: &HttpRequest<'_>) -> Result<Vec<u8>, ProviderError> {
        assert!(request.timeout > Duration::ZERO);
        *self.headers.borrow_mut() = request.headers.to_vec();
        Ok(if self.wrong {
            br#"{"model":"other","choices":[{"message":{"content":"OK"}}]}"#.to_vec()
        } else {
            br#"{"id":"chatcmpl-1","model":"qwen35-2b","choices":[{"message":{"content":"OK"}}]}"#
                .to_vec()
        })
    }
}
fn endpoint(spec: ProviderSpec) -> ConfiguredEndpoint {
    ConfiguredEndpoint {
        endpoint: LocalEndpoint::new(&spec.url, &spec.model).with_max_tokens(32),
        spec: Some(spec),
    }
}
#[test]
fn ollama_registry_accepts_actual_tagged_model_ids() {
    for model in [
        "qwen2.5-coder:7b",
        "qwen3:8b",
        "hf.co/mradermacher/Cyber-Ornith-1.5-9B-OBLITERATED-i1-GGUF:Q4_K_M",
    ] {
        let spec = local_spec(model);
        spec.validate().unwrap();
        spec.admit_role("repair-worker").unwrap();
    }
}
#[test]
fn ollama_registry_rejects_unsafe_model_ids() {
    for model in ["../model:7b", "team//model:7b", "model?token=x", ""] {
        error(local_spec(model).validate(), "invalid-provider-identity");
    }
}
#[test]
fn remote_https_validation_and_context() {
    let spec = remote_spec();
    spec.validate().unwrap();
    error(spec.admit_role("hermes-tool-worker"), "tool-calling");
    spec.admit_role("repair-worker").unwrap();
    let short = ProviderSpec {
        name: "short".into(),
        model: "qwen8".into(),
        url: "https://llm.example.home.arpa/v1/chat/completions".into(),
        context_tokens: 4096,
        tool_calls: true,
        ..spec
    };
    error(
        short.admit_role("hermes-tool-worker"),
        "insufficient-context",
    );
}
#[test]
fn remote_url_fail_closed() {
    for url in [
        "http://llm.example.home.arpa/v1/chat/completions",
        "https://evil.example/v1/chat/completions",
        "https://user:secret@llm.example.home.arpa/v1/chat/completions",
        "https://llm.example.home.arpa/v1/chat/completions?key=123",
        "https://llm.example.home.arpa/admin",
    ] {
        let spec = ProviderSpec {
            name: "x".into(),
            model: "m".into(),
            url: url.into(),
            context_tokens: 4096,
            key_env: "KEY".into(),
            ..remote_spec()
        };
        assert!(spec.validate().is_err());
    }
}
#[test]
fn registry_rejects_inline_secrets_and_duplicate_roles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.json");
    let mut spec = serde_json::to_value(remote_spec()).unwrap();
    spec["api_key"] = json!("secret");
    std::fs::write(
        &path,
        ordered_json_spaced(&json!({"profiles":[spec], "roles":{"repair-worker":"long"}})).unwrap(),
    )
    .unwrap();
    assert!(load_registry(&path).is_err());
}
#[test]
fn remote_adapter_bearer_not_logged() {
    let endpoint = endpoint(remote_spec());
    let fake = FakeOpener::default();
    let credentials = |_: &str| Some("VERY_SECRET_VALUE".to_owned());
    let data = OpenAICompatibleTransport
        .complete_with(&endpoint, b"{}", &fake, &credentials)
        .unwrap();
    assert!(data.windows(7).any(|bytes| bytes == b"choices"));
    assert_eq!(
        fake.headers
            .borrow()
            .iter()
            .find(|(key, _)| key == "Authorization")
            .unwrap()
            .1,
        "Bearer VERY_SECRET_VALUE"
    );
    error(
        OpenAICompatibleTransport.complete_with(&endpoint, b"{}", &fake, &|_: &str| None),
        "credential-environment-missing",
    );
}
#[test]
fn registry_roles_are_explicit() {
    let registry = ProviderRegistry {
        profiles: [("long".to_owned(), remote_spec())].into_iter().collect(),
        roles: [("repair-reviewer".to_owned(), "long".to_owned())]
            .into_iter()
            .collect(),
    };
    assert_eq!(
        registry.endpoint("repair-reviewer").unwrap().endpoint.model,
        "qwen35-2b"
    );
    error(registry.endpoint("repair-worker"), "role-not-configured");
}
fn policy(root: &Path) -> GPUPolicy {
    GPUPolicy {
        host: "gpu-host-01".into(),
        capacity_mib: 8192,
        reserved_mib: 512,
        lock_path: root.join("lane.lock"),
        units: vec![
            GPUUnit {
                name: "fallback.service".into(),
                vram_mib: 1966,
                pinned: true,
                managed: false,
                model: "qwen35".into(),
            },
            GPUUnit {
                name: "worker.service".into(),
                vram_mib: 5230,
                pinned: false,
                managed: true,
                model: "qwen8".into(),
            },
            GPUUnit {
                name: "granite.service".into(),
                vram_mib: 5500,
                pinned: false,
                managed: true,
                model: "granite".into(),
            },
        ],
    }
}
#[derive(Default)]
struct FakeSystemd {
    active: VecDeque<bool>,
    starts: Vec<String>,
    stops: Vec<String>,
    fail_start: bool,
}
impl Systemd for FakeSystemd {
    fn active(&mut self, _: &str) -> Result<bool, WorkflowError> {
        Ok(self.active.pop_front().unwrap_or(false))
    }
    fn start(&mut self, unit: &str) -> Result<(), WorkflowError> {
        self.starts.push(unit.into());
        if self.fail_start {
            Err(WorkflowError::msg("failed"))
        } else {
            Ok(())
        }
    }
    fn stop(&mut self, unit: &str) -> Result<(), WorkflowError> {
        self.stops.push(unit.into());
        Ok(())
    }
}
fn request<'a>(
    policy: &'a GPUPolicy,
    journal: &'a Path,
    transaction: &'a str,
) -> SwitchRequest<'a> {
    SwitchRequest {
        policy,
        prior: "worker.service",
        target: "granite.service",
        transaction,
        journal,
        authorized: true,
    }
}
#[test]
fn gpu_budget_and_protected_unit() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    setting.admit(&["worker.service"]).unwrap();
    error(
        setting.admit(&["worker.service", "granite.service"]),
        "overcommit",
    );
    error(
        setting.admit(&["granite.service", "worker.service"]),
        "overcommit",
    );
}
#[test]
fn gpu_lease_contention() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    {
        let _lane = gpu_lane(&setting).unwrap();
        error(gpu_lane(&setting), "busy");
    }
    let _lane = gpu_lane(&setting).unwrap();
}
#[test]
fn gpu_switch_rejects_pinned_and_unmanaged() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let mut systemd = FakeSystemd::default();
    error(
        switch_gpu_slot(
            SwitchRequest {
                prior: "fallback.service",
                ..request(&setting, &journal, "test1")
            },
            &mut systemd,
            &mut |_| Ok(true),
            None,
        ),
        "Pinned or unmanaged",
    );
    assert!(systemd.stops.is_empty());
}
fn result(journal: &Path, transaction: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(journal.join(format!("{transaction}-result.json"))).unwrap(),
    )
    .unwrap()
}
#[test]
fn gpu_switch_failure_records_unknown_without_retry() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let mut systemd = FakeSystemd {
        active: VecDeque::from([true, false, false, false]),
        fail_start: true,
        ..FakeSystemd::default()
    };
    error(
        switch_gpu_slot(
            request(&setting, &journal, "txn1"),
            &mut systemd,
            &mut |_| Ok(true),
            None,
        ),
        "UNKNOWN",
    );
    let result = result(&journal, "txn1");
    assert_eq!(result["phase"], "UNKNOWN");
    assert_eq!(result["manual_reconciliation_required"], true);
    assert!(
        switch_gpu_slot(
            request(&setting, &journal, "txn1"),
            &mut systemd,
            &mut |_| Ok(true),
            None
        )
        .is_err()
    );
    assert_eq!(systemd.starts.len(), 1);
}
#[test]
fn gpu_switch_success_requires_probed_ready() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let mut systemd = FakeSystemd {
        active: VecDeque::from([true, false, false, true]),
        ..FakeSystemd::default()
    };
    let record = switch_gpu_slot(
        request(&setting, &journal, "txn2"),
        &mut systemd,
        &mut |unit| Ok(unit.model == "granite"),
        None,
    )
    .unwrap();
    assert_eq!(record.phase, "READY");
    assert_eq!(systemd.stops, ["worker.service"]);
    assert_eq!(systemd.starts, ["granite.service"]);
}
fn sample(model: &str, session: &str, response: Value) -> EvalSample {
    EvalSample {
        model: model.into(),
        task: "pwd".into(),
        session: session.into(),
        expected_tool: "terminal".into(),
        response,
        receipt_path: None,
        receipt_sha256: String::new(),
        policy_denied: false,
        duration_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
    }
}
fn report(samples: &[EvalSample]) -> Value {
    serde_json::to_value(evaluate_samples(samples).unwrap()).unwrap()
}
#[test]
fn evaluation_requires_real_bound_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let response = json!({"tool_calls":[{"function":{"name":"terminal","arguments":"{\"command\":\"pwd\"}"}}]});
    let no_receipt = sample("qwen", "s1", response.clone());
    let missing = report(&[no_receipt]);
    assert_eq!(
        missing["results"]["qwen"]["categories"]["missing_terminal_receipt"],
        1
    );
    assert_eq!(missing["results"]["qwen"]["tool_worker_eligible"], false);
    let receipt = dir.path().join("receipt.json");
    std::fs::write(&receipt, ordered_json_spaced(&json!({"session_id":"s1", "tool":"terminal", "command":"pwd", "executed":true, "exit_code":0})).unwrap()).unwrap();
    let valid = EvalSample {
        receipt_sha256: sha256_hex(&std::fs::read(&receipt).unwrap()),
        receipt_path: Some(receipt),
        ..sample("qwen", "s1", response)
    };
    let result = report(&[valid]);
    assert_eq!(
        result["results"]["qwen"]["categories"]["reported_tool_success_requires_origin_review"],
        1
    );
    assert_eq!(result["results"]["qwen"]["verified_successes"], 0);
    assert_eq!(result["unattended_worker_admission"], false);
}
#[test]
fn evaluation_distinguishes_policy_denial_and_tool_schema() {
    let denied = EvalSample {
        policy_denied: true,
        task: "terminal".into(),
        ..sample("granite", "s1", json!({}))
    };
    let bad = sample(
        "granite",
        "s2",
        json!({"tool_calls":[{"function":{"name":"cat","arguments":"{\"command\":\"pwd\"}"}}]}),
    );
    assert_eq!(
        report(&[denied, bad])["results"]["granite"]["categories"],
        json!({"policy_denial":1,"wrong_tool_name":1})
    );
}
#[test]
fn evaluation_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("samples.json");
    std::fs::write(&file, ordered_json_spaced(&json!([{"model":"x","task":"t","session":"s","expected_tool":"terminal","response":{},"receipt_file":"../secret"}])).unwrap()).unwrap();
    error(load_samples(&file, dir.path()), "filename");
}
#[test]
fn gpu_observed_capacity_rejection_keeps_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let setting = policy(dir.path());
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let mut systemd = FakeSystemd {
        active: VecDeque::from([true, false, false]),
        ..FakeSystemd::default()
    };
    error(
        switch_gpu_slot(
            request(&setting, &journal, "low-vram"),
            &mut systemd,
            &mut |_| Ok(true),
            Some(&mut || Ok(128)),
        ),
        "UNKNOWN",
    );
    assert_eq!(result(&journal, "low-vram")["phase"], "UNKNOWN");
    assert!(systemd.starts.is_empty());
}
#[test]
fn openai_completion_model_mismatch() {
    let fake = FakeOpener {
        wrong: true,
        ..FakeOpener::default()
    };
    error(
        OpenAICompatibleTransport.complete_with(
            &endpoint(remote_spec()),
            b"{}",
            &fake,
            &|_: &str| Some("secret".to_owned()),
        ),
        "identity-or-schema",
    );
}
#[test]
fn bridge_parser_exposes_optin_deployment_commands() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = SkillBridgeArgs::try_parse_from([
        "bridge",
        "repair-dispatch",
        "--state",
        dir.path().join("state").to_str().unwrap(),
        "--task-id",
        "task",
        "--checkpoint",
        dir.path().join("cp").to_str().unwrap(),
        "--objective",
        "fix",
        "--owned-path",
        "x.py",
        "--session",
        "s",
        "--provider-registry",
        dir.path().join("providers.json").to_str().unwrap(),
    ])
    .unwrap();
    let BridgeCommand::RepairDispatch {
        provider_registry,
        authorize_inference,
        ..
    } = bridge.command
    else {
        panic!("wrong command");
    };
    assert_eq!(provider_registry, Some(dir.path().join("providers.json")));
    assert!(!authorize_inference);
    let args = add_deployment_commands(clap::Command::new("bridge"))
        .try_get_matches_from([
            "bridge",
            "deploy-preflight",
            "--workspace",
            dir.path().to_str().unwrap(),
            "--session",
            "s",
        ])
        .unwrap();
    assert_eq!(args.subcommand_name(), Some("deploy-preflight"));
    assert!(
        !args
            .subcommand_matches("deploy-preflight")
            .unwrap()
            .get_flag("probe_network")
    );
}
struct OfflineHost;
impl PreflightHost for OfflineHost {
    fn python_314(&self) -> bool {
        true
    }
    fn which(&self, _: &str) -> bool {
        true
    }
    fn workspace_access(&self, _: &Path) -> bool {
        true
    }
    fn dedicated_identity(&self) -> bool {
        true
    }
    fn gateway_active(&self, _: &str) -> Result<bool, WorkflowError> {
        panic!("unexpected process probe")
    }
    fn probe(&self, _: &ProviderSpec) -> Result<ProviderProbe, ProviderError> {
        panic!("unexpected network")
    }
}
#[test]
fn preflight_does_not_grant_native_session() {
    let dir = tempfile::tempdir().unwrap();
    let result = hp_preflight_with(dir.path(), None, None, false, "", &OfflineHost).unwrap();
    assert!(!result.ready);
    assert_eq!(result.checks.get("native_session").unwrap().status, "BLOCK");
    assert_eq!(result.checks.get("context_64k").unwrap().status, "BLOCK");
    assert!(!result.gateway_modified);
}
struct DispatchProbe {
    source_hash: String,
    models: RefCell<Vec<(String, String)>>,
}
impl hermes_harness::native::repair_dispatch::DispatchHost for DispatchProbe {
    fn audit(
        &self,
        _root: &Path,
        _record: &serde_json::Map<String, Value>,
    ) -> Result<std::collections::BTreeMap<String, String>, WorkflowError> {
        Ok([("existing.py".to_owned(), self.source_hash.clone())].into())
    }
    fn credential(&self, name: &str) -> Option<String> {
        (name == "LOCAL_LLM_API_KEY").then(|| "DO_NOT_LOG".to_owned())
    }
    fn hostname(&self) -> String {
        "probe".to_owned()
    }
    fn generate(
        &self,
        worker: &LocalEndpoint,
        reviewer: &LocalEndpoint,
        _task: &hermes_harness::multiagent::replacement_pipeline::ReplacementTask<'_>,
        _transport: &dyn hermes_harness::multiagent::local_inference::Transport,
    ) -> Result<
        hermes_harness::multiagent::replacement_pipeline::ReplacementReview,
        hermes_harness::multiagent::replacement_pipeline::ReplacementPipelineError,
    > {
        self.models
            .borrow_mut()
            .push((worker.model.clone(), reviewer.model.clone()));
        Ok(
            hermes_harness::multiagent::replacement_pipeline::ReplacementReview {
                task_id: "one".to_owned(),
                replacements: vec![hermes_harness::multiagent::replace_edits::FileReplacement {
                    path: "existing.py".to_owned(),
                    expected_sha256: self.source_hash.clone(),
                    content: "print(1)\n".to_owned(),
                }],
                reviewer_model: reviewer.model.clone(),
                verdict: hermes_harness::multiagent::model_review::Verdict::Accept,
                findings: Vec::new(),
                applied: false,
            },
        )
    }
    fn python_parses(&self, _name: &str, _content: &str) -> Result<bool, WorkflowError> {
        Ok(true)
    }
}
struct NoWire;
impl hermes_harness::multiagent::local_inference::Transport for NoWire {
    fn complete(
        &self,
        _: &LocalEndpoint,
        _: &[u8],
    ) -> Result<Vec<u8>, hermes_harness::multiagent::local_inference::LocalInferenceError> {
        panic!("the injected generator never reaches the wire")
    }
}
#[test]
fn repair_dispatch_uses_explicit_registry_without_sending_credentials() {
    use hermes_harness::native::repair_controller::{open_repair, reserve_repair};
    use hermes_harness::native::repair_dispatch::{DispatchRequest, dispatch_repair_with};
    let tmp = tempfile::tempdir().unwrap();
    let root = hermes_harness::util::resolve_strict(tmp.path()).unwrap();
    let state = root.join("evidence");
    std::fs::create_dir(&state).unwrap();
    let workspace = root.join("worktree");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("existing.py"), "print(0)\n").unwrap();
    let source_hash = sha256_hex(b"print(0)\n");
    let checkpoint = root.join("checkpoint.json");
    let workspace_text = workspace.to_string_lossy().into_owned();
    std::fs::write(
        &checkpoint,
        serde_json::to_string(&json!({
            "mode": "integrity-checkpoint",
            "task_id": "one",
            "baseline": "a".repeat(40),
            "workspace": workspace_text,
            "max_repair_attempts": 3,
            "files": [{"path": "existing.py", "sha256": source_hash}],
        }))
        .unwrap(),
    )
    .unwrap();
    let checkpoint_hash = sha256_hex(&std::fs::read(&checkpoint).unwrap());
    let failed = root.join("failed.json");
    std::fs::write(
        &failed,
        serde_json::to_string(&json!({
            "mode": "sandboxed-functional-verification",
            "task_id": "one",
            "baseline": "a".repeat(40),
            "workspace": workspace_text,
            "checkpoint_sha256": checkpoint_hash,
            "functional_verified": false,
            "tests_executed": true,
            "sandbox_started": true,
            "timed_out": false,
            "human_decision_pending": true,
        }))
        .unwrap(),
    )
    .unwrap();
    open_repair(&failed, &checkpoint, &state, true).unwrap();
    reserve_repair(&state, "one", true).unwrap();
    let worker = ProviderSpec {
        name: "worker".into(),
        backend: "llama.cpp".into(),
        model: "qwen8".into(),
        url: "https://llm.example.home.arpa/v1/chat/completions".into(),
        context_tokens: 4096,
        key_env: "LOCAL_LLM_API_KEY".into(),
        tool_calls: false,
        unit: String::new(),
        gpu_host: String::new(),
        max_concurrency: 1,
    };
    let reviewer = ProviderSpec {
        unit: String::new(),
        gpu_host: String::new(),
        ..remote_spec()
    };
    let profile = root.join("profile.json");
    std::fs::write(
        &profile,
        ordered_json_spaced(&json!({
            "profiles": [worker, reviewer],
            "roles": {"repair-worker": "worker", "repair-reviewer": "long"},
        }))
        .unwrap(),
    )
    .unwrap();
    let probe = DispatchProbe {
        source_hash,
        models: RefCell::new(Vec::new()),
    };
    let owned = vec!["existing.py".to_owned()];
    let mut request = DispatchRequest::new(&state, "one", &checkpoint, "fix", &owned);
    request.authorized = true;
    request.registry_file = Some(&profile);
    request.transport = Some(&NoWire);
    let result = dispatch_repair_with(&probe, &request).unwrap();
    assert_eq!(result["worker_model"], "qwen8");
    assert_eq!(result["reviewer_model"], "qwen35-2b");
    let models = probe.models.borrow();
    assert_eq!(models[0], ("qwen8".to_owned(), "qwen35-2b".to_owned()));
    let saved = std::fs::read_to_string(state.join("one-repair-1-review.json")).unwrap();
    assert!(!saved.contains("DO_NOT_LOG"));
}

#[test]
fn registry_transport_validates_remote_endpoints() {
    use hermes_harness::multiagent::local_inference::{LoopbackTransport, Transport};
    let mut spec = remote_spec();
    // PATH supplies a nonempty credential reference without mutating the environment.
    spec.key_env = "PATH".into();
    let mut registry = ProviderRegistry {
        profiles: Default::default(),
        roles: Default::default(),
    };
    registry
        .roles
        .insert("repair-worker".into(), spec.name.clone());
    registry.profiles.insert(spec.name.clone(), spec);
    let endpoint = registry.endpoint("repair-worker").unwrap().endpoint;
    let transport = registry
        .transport(&["repair-worker"], &Default::default())
        .unwrap();
    transport.validate(&endpoint).unwrap();
    struct Offline<'a>(&'a dyn Transport);
    impl Transport for Offline<'_> {
        fn validate(
            &self,
            endpoint: &LocalEndpoint,
        ) -> Result<(), hermes_harness::multiagent::local_inference::LocalInferenceError> {
            self.0.validate(endpoint)
        }
        fn complete(
            &self,
            _: &LocalEndpoint,
            _: &[u8],
        ) -> Result<Vec<u8>, hermes_harness::multiagent::local_inference::LocalInferenceError>
        {
            Ok(br#"{"choices":[{"message":{"content":"OK"}}]}"#.to_vec())
        }
    }
    assert_eq!(
        hermes_harness::multiagent::local_inference::complete_text(
            &endpoint,
            "system",
            "prompt",
            &Offline(&transport)
        )
        .unwrap(),
        "OK"
    );
    let mut mismatch = endpoint.clone();
    mismatch.url = "https://other.example.home.arpa/v1/chat/completions".into();
    error(
        transport.validate(&mismatch),
        "endpoint-does-not-match-registry",
    );
    assert!(LoopbackTransport.validate(&endpoint).is_err());
    let mut over_budget = endpoint;
    over_budget.max_tokens = 8193;
    error(transport.validate(&over_budget), "invalid-inference-budget");
}

#[test]
fn gpu_policy_accepts_bare_relative_lock_filename() {
    let mut setting = policy(Path::new("."));
    setting.lock_path = "bare-gpu-lane.lock".into();
    setting.validate().unwrap();
}

#[test]
fn provider_probe_uses_origin_with_empty_query() {
    use hermes_harness::deployment::providers::probe_provider_with;
    use hermes_harness::multiagent::local_inference::LoopbackTransport;
    struct Catalogue;
    impl Opener for Catalogue {
        fn open(&self, request: &HttpRequest<'_>) -> Result<Vec<u8>, ProviderError> {
            assert!(!request.url.contains('?'));
            match request.url {
                "https://llm-long.example.home.arpa/health" => Ok(b"OK".to_vec()),
                "https://llm-long.example.home.arpa/v1/models" => {
                    Ok(br#"{"data":[{"id":"qwen35-2b"}]}"#.to_vec())
                }
                _ => FakeOpener::default().open(request),
            }
        }
    }
    let mut spec = remote_spec();
    spec.url.push('?');
    // The registered completion URL retains its empty query; only probe GETs use the origin.
    struct ProbeOpener;
    impl Opener for ProbeOpener {
        fn open(&self, request: &HttpRequest<'_>) -> Result<Vec<u8>, ProviderError> {
            if request.method == "POST" {
                FakeOpener::default().open(request)
            } else {
                Catalogue.open(request)
            }
        }
    }
    let credentials = |_: &str| Some("test-key".to_owned());
    assert!(
        probe_provider_with(&spec, &ProbeOpener, &credentials, &LoopbackTransport)
            .unwrap()
            .model_identity_verified
    );
}

#[test]
fn gpu_policy_rejects_service_patterns_and_option_names() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "*.service",
        "-x.service",
        "x?.service",
        "[ab].service",
        ".service",
    ] {
        let mut setting = policy(dir.path());
        setting.units[1].name = name.into();
        error(setting.validate(), "Invalid GPU unit");
    }
    let mut setting = policy(dir.path());
    setting.units[1].name = "model@worker-1.2.service".into();
    setting.validate().unwrap();
}
