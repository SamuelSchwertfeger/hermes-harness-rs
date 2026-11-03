//! Operator-observed Ollama role routing and serial capacity admission.
//!
//! No model is automatically started or downloaded. Role selections are explicit
//! and model inventory is checked before inference, not inferred from file names.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::local_inference::{LocalEndpoint, LocalInferenceError};
use super::ollama_discovery::OllamaModel;
use crate::util::ordered_map::OrderedMap;

/// Default model for the Cyber Warrior role.
pub const CYBER_MODEL: &str = "hf.co/mradermacher/Cyber-Ornith-1.5-9B-OBLITERATED-i1-GGUF:Q4_K_M";

fn fail<T>(message: &str) -> Result<T, LocalInferenceError> {
    Err(LocalInferenceError::msg(message))
}

/// Supported local inference roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// Coding worker.
    Coding,
    /// Lightweight worker.
    Lightweight,
    /// Independent reviewer.
    Reviewer,
    /// Cyber Warrior worker.
    CyberWarrior,
}

impl ModelRole {
    /// Python role string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Lightweight => "lightweight",
            Self::Reviewer => "reviewer",
            Self::CyberWarrior => "cyber-warrior",
        }
    }
}

impl fmt::Display for ModelRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModelRole {
    type Err = LocalInferenceError;

    fn from_str(role: &str) -> Result<Self, Self::Err> {
        match role {
            "coding" => Ok(Self::Coding),
            "lightweight" => Ok(Self::Lightweight),
            "reviewer" => Ok(Self::Reviewer),
            "cyber-warrior" => Ok(Self::CyberWarrior),
            _ => fail("Unknown local model role"),
        }
    }
}

/// Local model choices; all roles use the same bounded loopback endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaRouting {
    /// Coding model.
    pub coding: String,
    /// Lightweight model.
    pub lightweight: String,
    /// Reviewer model.
    pub reviewer: String,
    /// Cyber Warrior model.
    pub cyber_warrior: String,
    /// Ollama base URL.
    pub base_url: String,
    /// Requested parallelism; only 1 is qualified.
    pub max_parallel: i64,
}

impl Default for OllamaRouting {
    fn default() -> Self {
        Self {
            coding: "qwen2.5-coder:7b".to_owned(),
            lightweight: "qwen2.5-coder:1.5b".to_owned(),
            reviewer: "qwen3:4b".to_owned(),
            cyber_warrior: CYBER_MODEL.to_owned(),
            base_url: "http://127.0.0.1:11434".to_owned(),
            max_parallel: 1,
        }
    }
}

impl OllamaRouting {
    /// Validated chat-completions endpoint for `role`.
    ///
    /// # Errors
    ///
    /// Returns [`LocalInferenceError`] for unqualified parallelism, an unknown role, or an
    /// endpoint that fails validation.
    pub fn endpoint(&self, role: &str) -> Result<LocalEndpoint, LocalInferenceError> {
        if self.max_parallel != 1 {
            return fail("Concurrent inference requires separate GPU capacity qualification");
        }
        let model = match role.parse::<ModelRole>()? {
            ModelRole::Coding => &self.coding,
            ModelRole::Lightweight => &self.lightweight,
            ModelRole::Reviewer => &self.reviewer,
            ModelRole::CyberWarrior => &self.cyber_warrior,
        };
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let endpoint = LocalEndpoint::new(&url, model);
        endpoint.validate()?;
        Ok(endpoint)
    }
}

/// Result of [`qualify_models`] (JSON field order is the Python dict order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Qualification {
    /// Selected model per requested role, in request order.
    pub roles: OrderedMap<String>,
    /// Always 1: only serial inference is qualified.
    pub max_parallel: u8,
    /// Always true: only installed models are admitted.
    pub qualified_inventory_only: bool,
}

/// Fail closed on missing installed models and misconfigured role separation.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] when a role cannot be routed, its model is not installed, or
/// the worker, reviewer and Cyber Warrior roles share a model.
pub fn qualify_models(
    routing: &OllamaRouting,
    inventory: &[OllamaModel],
    roles: &[&str],
) -> Result<Qualification, LocalInferenceError> {
    let available: BTreeSet<&str> = inventory.iter().map(|item| item.name.as_str()).collect();
    let mut selected: OrderedMap<String> = OrderedMap::new();
    for role in roles {
        let endpoint = routing.endpoint(role)?;
        if !available.contains(endpoint.model.as_str()) {
            return Err(LocalInferenceError::Message(format!(
                "Model for role {role} is not installed: {}",
                endpoint.model
            )));
        }
        selected.insert((*role).to_owned(), endpoint.model);
    }
    if selected.get("coding").is_some() && selected.get("coding") == selected.get("reviewer") {
        return fail("Worker and reviewer must use distinct models");
    }
    if selected.get("cyber-warrior").is_some()
        && selected.get("cyber-warrior") == selected.get("reviewer")
    {
        return fail("Cyber Warrior and reviewer must use distinct models");
    }
    Ok(Qualification {
        roles: selected,
        max_parallel: 1,
        qualified_inventory_only: true,
    })
}
