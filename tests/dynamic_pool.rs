#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! On-demand XPS model pool authorization and inventory tests.

use std::time::Duration;

use hermes_harness::core::ollama::{HttpResponse, TransportError};
use hermes_harness::multiagent::dynamic_pool::{
    XPS_BASE, available_models, available_models_with, infer, infer_with,
};
use hermes_harness::multiagent::ollama_discovery::HttpGet;
use serde_json::json;

struct FakeTags {
    models: Vec<(&'static str, u64)>,
}

impl HttpGet for FakeTags {
    fn get(
        &self,
        url: &str,
        _timeout: Duration,
        _limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        assert_eq!(url, "http://127.0.0.1:11435/api/tags");
        let rows: Vec<_> = self
            .models
            .iter()
            .map(|(name, size)| json!({"name": name, "size": size}))
            .collect();
        Ok(HttpResponse {
            status: 200,
            body: json!({"models": rows}).to_string().into_bytes(),
        })
    }
}

#[test]
fn any_installed_model_can_be_discovered() {
    let http = FakeTags {
        models: vec![
            ("granite4:3b", 200),
            ("qwen2.5-coder:7b", 300),
            ("hf.co/org/cyber:Q4_K_M", 500),
        ],
    };
    let names = available_models_with(&http, XPS_BASE).unwrap();
    assert_eq!(
        names,
        ["granite4:3b", "qwen2.5-coder:7b", "hf.co/org/cyber:Q4_K_M"]
    );
}

#[test]
fn operator_approval_required() {
    let error = infer("granite4:3b", "test", false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
}

#[test]
fn unavailable_model_cannot_run() {
    let http = FakeTags { models: vec![] };
    let error = infer_with(&http, None, "granite4:3b", "test", true).unwrap_err();
    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn only_loopback_xps_inventory() {
    let error = available_models("http://example.com").unwrap_err();
    assert!(error.to_string().contains("Unapproved"));
}
