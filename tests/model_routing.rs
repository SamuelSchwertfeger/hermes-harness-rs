#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Host-observed model routing policy: no live inference in tests.

use hermes_harness::multiagent::ollama_discovery::OllamaModel;
use hermes_harness::multiagent::routing::{CYBER_MODEL, OllamaRouting, qualify_models};

#[test]
fn routing_defaults_match_observed_installation() {
    let routing = OllamaRouting::default();
    assert_eq!(
        routing.endpoint("coding").unwrap().model,
        "qwen2.5-coder:7b"
    );
    assert_eq!(
        routing.endpoint("cyber-warrior").unwrap().model,
        CYBER_MODEL
    );
    assert_eq!(routing.endpoint("reviewer").unwrap().model, "qwen3:4b");
}

#[test]
fn inventory_validation_is_fail_closed() {
    let routing = OllamaRouting::default();
    let models = [
        OllamaModel::new("qwen2.5-coder:7b", 4_700_000_000),
        OllamaModel::new("qwen3:4b", 2_500_000_000),
    ];
    let result = qualify_models(&routing, &models, &["coding", "reviewer"]).unwrap();
    assert_eq!(result.max_parallel, 1);
    let error = qualify_models(&routing, &models, &["cyber-warrior"]).unwrap_err();
    assert!(error.to_string().contains("not installed"));
}

#[test]
fn unqualified_parallelism_is_rejected() {
    let routing = OllamaRouting {
        max_parallel: 2,
        ..OllamaRouting::default()
    };
    let error = routing.endpoint("coding").unwrap_err();
    assert!(error.to_string().contains("capacity qualification"));
}

#[test]
fn review_identity_cannot_overlap() {
    let routing = OllamaRouting {
        reviewer: "qwen2.5-coder:7b".to_owned(),
        ..OllamaRouting::default()
    };
    let models = [OllamaModel::new("qwen2.5-coder:7b", 100)];
    let error = qualify_models(&routing, &models, &["coding", "reviewer"]).unwrap_err();
    assert!(error.to_string().contains("distinct models"));
}

#[test]
fn model_roles_round_trip_and_preserve_validation_order() {
    use hermes_harness::multiagent::routing::ModelRole;
    for role in ["coding", "lightweight", "reviewer", "cyber-warrior"] {
        let parsed = role.parse::<ModelRole>().unwrap();
        assert_eq!(parsed.as_str(), role);
        assert_eq!(parsed.to_string(), role);
    }
    for role in ["", "Coding", "cyber_warrior"] {
        assert_eq!(
            role.parse::<ModelRole>().unwrap_err().to_string(),
            "Unknown local model role"
        );
    }
    let routing = OllamaRouting {
        max_parallel: 2,
        ..OllamaRouting::default()
    };
    assert_eq!(
        routing.endpoint("unknown").unwrap_err().to_string(),
        "Concurrent inference requires separate GPU capacity qualification"
    );
}
