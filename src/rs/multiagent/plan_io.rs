//! Strict admission of model- or Hermes-produced multi-agent plans.
//!
//! No provider calls or task execution occur here. Untrusted model text is admitted
//! only after size, schema, field, role and canonical DAG validation.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::runtime::{Node, Plan, WorkflowError, ordered_nodes};
use crate::util::is_py_space;

/// Maximum accepted plan document size in bytes.
pub const MAX_PLAN_BYTES: usize = 64 * 1024;
/// Maximum characters of an objective.
pub const MAX_OBJECTIVE_CHARS: usize = 4096;
/// Maximum characters of an identity field.
pub const MAX_IDENTITY_CHARS: usize = 128;
const PLAN_KEYS: [&str; 4] = ["id", "objective", "nodes", "primary_identity"];
const NODE_KEYS: [&str; 5] = ["id", "objective", "owned_paths", "dependencies", "role"];
const DUPLICATE_KEY: &str = "Duplicate JSON key";

fn strings(value: Option<&Value>, field: &str) -> Result<Vec<String>, WorkflowError> {
    let invalid = || WorkflowError::msg(&format!("{field} must be an array of strings"));
    let Some(Value::Array(items)) = value else {
        return Err(invalid());
    };
    items
        .iter()
        .map(|item| item.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect()
}

fn bounded_text(value: Option<&Value>, field: &str, limit: usize) -> Result<String, WorkflowError> {
    match value {
        Some(Value::String(text))
            if !text.chars().all(is_py_space) && text.chars().count() <= limit =>
        {
            Ok(text.clone())
        }
        _ => Err(WorkflowError::msg(&format!("Invalid {field}"))),
    }
}

fn has_unknown(object: &Map<String, Value>, allowed: &[&str]) -> bool {
    object.keys().any(|key| !allowed.contains(&key.as_str()))
}

fn node(value: &Value) -> Result<Node, WorkflowError> {
    let Value::Object(object) = value else {
        return Err(WorkflowError::msg("Invalid node object or unknown fields"));
    };
    if has_unknown(object, &NODE_KEYS) {
        return Err(WorkflowError::msg("Invalid node object or unknown fields"));
    }
    let default_deps = Value::Array(Vec::new());
    let default_role = Value::String("coding".to_owned());
    Ok(Node {
        id: bounded_text(object.get("id"), "node ID", MAX_IDENTITY_CHARS)?,
        objective: bounded_text(
            object.get("objective"),
            "node objective",
            MAX_OBJECTIVE_CHARS,
        )?,
        owned_paths: strings(object.get("owned_paths"), "owned_paths")?,
        dependencies: strings(
            Some(object.get("dependencies").unwrap_or(&default_deps)),
            "dependencies",
        )?,
        role: bounded_text(
            Some(object.get("role").unwrap_or(&default_role)),
            "role",
            MAX_IDENTITY_CHARS,
        )?
        .parse()?,
    })
}

/// Reject unknown fields, ambiguous types and invalid DAGs before execution.
///
/// # Errors
/// [`WorkflowError::Message`] describing the first violation.
pub fn parse_plan_document(value: &Value) -> Result<Plan, WorkflowError> {
    let Value::Object(object) = value else {
        return Err(WorkflowError::msg("Invalid plan object or unknown fields"));
    };
    if has_unknown(object, &PLAN_KEYS) {
        return Err(WorkflowError::msg("Invalid plan object or unknown fields"));
    }
    let id = bounded_text(object.get("id"), "plan ID", MAX_IDENTITY_CHARS)?;
    let objective = bounded_text(
        object.get("objective"),
        "plan objective",
        MAX_OBJECTIVE_CHARS,
    )?;
    let default_identity = Value::String("hermes-primary".to_owned());
    let primary_identity = bounded_text(
        Some(object.get("primary_identity").unwrap_or(&default_identity)),
        "primary identity",
        MAX_IDENTITY_CHARS,
    )?;
    let Some(Value::Array(rows)) = object.get("nodes") else {
        return Err(WorkflowError::msg("Plan nodes must be an array"));
    };
    let nodes = rows.iter().map(node).collect::<Result<Vec<_>, _>>()?;
    let plan = Plan {
        id,
        objective,
        nodes,
        primary_identity,
    };
    ordered_nodes(&plan)?;
    Ok(plan)
}

/// JSON visitor and seed whose decoding fails on duplicate object keys.
struct UniqueVisitor<'a>(&'a Cell<bool>);

impl<'de> Visitor<'de> for UniqueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(UniqueVisitor(self.0))? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut seen: BTreeMap<String, Value> = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(UniqueVisitor(self.0))?;
            if seen.insert(key, value).is_some() {
                self.0.set(true);
                return Err(de::Error::custom(DUPLICATE_KEY));
            }
        }
        Ok(Value::Object(seen.into_iter().collect()))
    }
}

impl<'de> DeserializeSeed<'de> for UniqueVisitor<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

/// Parse one JSON document; refuse oversized or duplicate-key model output.
///
/// # Errors
/// Oversized, malformed or duplicate-key JSON, or any [`parse_plan_document`] failure.
pub fn parse_plan_json(payload: &str) -> Result<Plan, WorkflowError> {
    if payload.len() > MAX_PLAN_BYTES {
        return Err(WorkflowError::msg("Plan exceeds input byte limit"));
    }
    let duplicate = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_str(payload);
    let result = UniqueVisitor(&duplicate)
        .deserialize(&mut deserializer)
        .and_then(|value| {
            deserializer.end()?;
            Ok(value)
        });
    match result {
        Ok(value) => parse_plan_document(&value),
        Err(error) if duplicate.get() => Err(WorkflowError::AdmissionJson {
            message: DUPLICATE_KEY.to_owned(),
            source: error,
        }),
        Err(source) => Err(WorkflowError::AdmissionJson {
            message: "Malformed plan JSON".to_owned(),
            source,
        }),
    }
}

impl std::str::FromStr for Plan {
    type Err = WorkflowError;

    fn from_str(payload: &str) -> Result<Self, Self::Err> {
        parse_plan_json(payload)
    }
}

impl TryFrom<&Value> for Plan {
    type Error = WorkflowError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        parse_plan_document(value)
    }
}
