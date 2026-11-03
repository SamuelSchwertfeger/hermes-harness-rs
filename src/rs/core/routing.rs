//! Explicit model registry: selection never grants tool or file privileges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Error raised when a route may not be selected (Python `PermissionError`).
#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    /// Selection refused by policy.
    #[error("{0}")]
    Permission(String),
}

crate::util::wire_enum! {
    /// Agent role; the string value matches the Python `StrEnum`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    pub enum Role {
        /// Planner role.
        Planner => "planner",
        /// Engineer role.
        Engineer => "engineer",
        /// Scout role.
        Scout => "scout",
        /// Reviewer role.
        Reviewer => "reviewer",
        /// Cyber role (disabled).
        Cyber => "cyber",
    }

    /// The Python enum value.
    as_str;
}

impl Role {
    /// Every role, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::Planner,
        Self::Engineer,
        Self::Scout,
        Self::Reviewer,
        Self::Cyber,
    ];
}

impl clap::ValueEnum for Role {
    fn value_variants<'a>() -> &'a [Self] {
        &Self::ALL
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}

/// One routing table entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// Role served by this route.
    pub role: Role,
    /// Backend name.
    pub backend: String,
    /// Model identifier.
    pub model: String,
    /// Whether tools may be used.
    pub tool_access: bool,
    /// Whether the route is read-only.
    pub read_only: bool,
    /// Wall-clock budget in seconds.
    pub max_seconds: u32,
    /// Output token budget.
    pub max_output_tokens: u32,
    /// GPU slots consumed.
    pub gpu_slots: u32,
    /// Whether the route is enabled.
    pub enabled: bool,
}

/// The registered route for `role`.
#[must_use]
pub fn route_for(role: Role) -> Route {
    let (backend, model, read_only, max_seconds, max_output_tokens, gpu_slots, enabled) = match role
    {
        Role::Planner => ("codex", "gpt-6.1-sol", true, 120, 2048, 0, true),
        Role::Engineer => ("codex", "gpt-6.1-sol", false, 180, 3072, 0, true),
        Role::Scout => ("ollama", "granite4:3b", true, 60, 512, 1, true),
        Role::Reviewer => ("ollama", "granite4:3b", true, 90, 1024, 1, true),
        Role::Cyber => ("unconfigured", "", true, 60, 1024, 1, false),
    };
    Route {
        role,
        backend: backend.to_owned(),
        model: model.to_owned(),
        tool_access: false,
        read_only,
        max_seconds,
        max_output_tokens,
        gpu_slots,
        enabled,
    }
}

/// The full routing table (`ROUTES`).
#[must_use]
pub fn routes() -> BTreeMap<Role, Route> {
    Role::ALL
        .into_iter()
        .map(|role| (role, route_for(role)))
        .collect()
}

/// Select the route for `role`, refusing disabled or unauthorised ones.
///
/// # Errors
/// The requested role or tool access is not permitted by the available routes.
pub fn select(role: Role, authorized_target: bool) -> Result<Route, RoutingError> {
    let route = route_for(role);
    if !route.enabled {
        return Err(RoutingError::Permission(format!(
            "Route {role} is disabled pending evaluation"
        )));
    }
    if role == Role::Cyber && !authorized_target {
        return Err(RoutingError::Permission(
            "Cyber operations require an authorized target".to_owned(),
        ));
    }
    Ok(route)
}
