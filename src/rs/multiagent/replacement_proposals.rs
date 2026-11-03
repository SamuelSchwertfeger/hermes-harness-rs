//! Admit model-proposed replacements with explicit expected preimage hashes.
//!
//! No file is read or modified while parsing. Execution remains a separate,
//! operator-authorized disposable-worktree operation.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use crate::multiagent::model_edits::{is_lexically_safe_path, is_owned};
use crate::multiagent::replace_edits::FileReplacement;
use crate::multiagent::runtime::WorkflowError;
use crate::multiagent::runtime::fail;

/// Maximum size of a whole replacement proposal in bytes.
pub const MAX_REQUEST_BYTES: usize = 128 * 1024;
pub use crate::multiagent::replace_edits::{MAX_REPLACEMENT_BYTES, MAX_REPLACEMENTS};

const DUPLICATE_FIELD: &str = "Duplicate replacement JSON field";

/// JSON visitor and seed rejecting duplicate keys at any depth (Python `object_pairs_hook`).
struct StrictVisitor<'a>(&'a Cell<bool>);

impl<'de> DeserializeSeed<'de> for StrictVisitor<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Number::from_f64(value).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(StrictVisitor(self.0))? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if map.contains_key(&key) {
                self.0.set(true);
                return Err(de::Error::custom(DUPLICATE_FIELD));
            }
            let value = access.next_value_seed(StrictVisitor(self.0))?;
            map.insert(key, value);
        }
        Ok(Value::Object(map))
    }
}

fn valid_replacement_path(name: &str, owned_paths: &[String], seen: &BTreeSet<String>) -> bool {
    is_lexically_safe_path(name) && !seen.contains(name) && is_owned(name, owned_paths)
}

fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn has_exact_keys(row: &Map<String, Value>, keys: &[&str]) -> bool {
    row.len() == keys.len() && keys.iter().all(|key| row.contains_key(*key))
}

fn replacement_row(
    row: &Value,
    owned_paths: &[String],
    seen: &mut BTreeSet<String>,
) -> Result<FileReplacement, WorkflowError> {
    let Some(map) = row
        .as_object()
        .filter(|map| has_exact_keys(map, &["path", "expected_sha256", "content"]))
    else {
        return fail("Invalid replacement entry schema");
    };
    let Some(name) = map.get("path").and_then(Value::as_str) else {
        return fail("Invalid replacement path");
    };
    if !valid_replacement_path(name, owned_paths, seen) {
        return fail("Unsafe, duplicate or unowned replacement path");
    }
    let digest = match map.get("expected_sha256").and_then(Value::as_str) {
        Some(text) if valid_sha256(text) => text,
        _ => return fail("Invalid expected preimage SHA-256"),
    };
    let content = match map.get("content").and_then(Value::as_str) {
        Some(text) if text.len() <= MAX_REPLACEMENT_BYTES => text,
        _ => return fail("Invalid replacement content"),
    };
    seen.insert(name.to_owned());
    Ok(FileReplacement {
        path: name.to_owned(),
        expected_sha256: digest.to_owned(),
        content: content.to_owned(),
    })
}

/// Reject ambiguous or unowned replacements before reading worktree data.
///
/// # Errors
///
/// Returns a [`WorkflowError`] for oversized, malformed or duplicate-key JSON, schema or task
/// mismatches, a bad replacement count, or any unsafe, unowned or invalid entry.
pub fn parse_replacement_proposal(
    payload: &str,
    task_id: &str,
    owned_paths: &[String],
) -> Result<Vec<FileReplacement>, WorkflowError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return fail("Oversized replacement proposal");
    }
    let duplicate = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_str(payload);
    let result = StrictVisitor(&duplicate)
        .deserialize(&mut deserializer)
        .and_then(|value| {
            deserializer.end()?;
            Ok(value)
        });
    let document = match result {
        Ok(value) => value,
        Err(error) if duplicate.get() => {
            return Err(WorkflowError::AdmissionJson {
                message: DUPLICATE_FIELD.to_owned(),
                source: error,
            });
        }
        Err(source) => {
            return Err(WorkflowError::AdmissionJson {
                message: "Malformed replacement proposal JSON".to_owned(),
                source,
            });
        }
    };
    let Some(map) = document
        .as_object()
        .filter(|map| has_exact_keys(map, &["task_id", "replacements"]))
    else {
        return fail("Invalid replacement proposal schema");
    };
    if map.get("task_id").and_then(Value::as_str) != Some(task_id) {
        return fail("Replacement task mismatch");
    }
    let rows = match map.get("replacements").and_then(Value::as_array) {
        Some(rows) if (1..=MAX_REPLACEMENTS).contains(&rows.len()) => rows,
        _ => return fail("Invalid replacement count"),
    };
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| replacement_row(row, owned_paths, &mut seen))
        .collect()
}
