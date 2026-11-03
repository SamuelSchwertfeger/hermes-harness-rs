//! Insertion-ordered string-keyed map with Python `dict` semantics.
//!
//! Python dicts keep insertion order, and `json.load` preserves file order, so
//! state files must round-trip with their keys in the original order.

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Map from `String` to `V` that iterates in insertion order.
#[derive(Clone)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
    index: Option<HashMap<String, usize>>,
}

// Equality and `Debug` ignore `index`: it is derived from `entries`.
impl<V: PartialEq> PartialEq for OrderedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<V: Eq> Eq for OrderedMap<V> {}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "preserve the existing debug output"
)]
impl<V: fmt::Debug> fmt::Debug for OrderedMap<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrderedMap")
            .field("entries", &self.entries)
            .finish()
    }
}

impl<V> OrderedMap<V> {
    /// An empty map.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: None,
        }
    }

    /// Value for `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        let slot = self.index.as_ref()?.get(key)?;
        self.entries.get(*slot).map(|(_, value)| value)
    }

    /// Whether `key` is present.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.index
            .as_ref()
            .is_some_and(|index| index.contains_key(key))
    }

    /// Set `key`; an existing key keeps its original position (like `dict[key] = value`).
    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        let index = self.index.get_or_insert_with(HashMap::new);
        if let Some((_, slot)) = index.get(&key).and_then(|slot| self.entries.get_mut(*slot)) {
            return Some(std::mem::replace(slot, value));
        }
        index.insert(key.clone(), self.entries.len());
        self.entries.push((key, value));
        None
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> FromIterator<(String, V)> for OrderedMap<V> {
    fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<V: Serialize> Serialize for OrderedMap<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            out.serialize_entry(key, value)?;
        }
        out.end()
    }
}

struct MapVisitor<V>(PhantomData<V>);

impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
    type Value = OrderedMap<V>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map with string keys")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut map = OrderedMap::new();
        while let Some((key, value)) = access.next_entry::<String, V>()? {
            map.insert(key, value);
        }
        Ok(map)
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for OrderedMap<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

impl<V> Extend<(String, V)> for OrderedMap<V> {
    fn extend<T: IntoIterator<Item = (String, V)>>(&mut self, iter: T) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<V> IntoIterator for OrderedMap<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, V> IntoIterator for &'a OrderedMap<V> {
    type Item = (&'a String, &'a V);
    type IntoIter =
        std::iter::Map<std::slice::Iter<'a, (String, V)>, fn(&'a (String, V)) -> Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter().map(|(key, value)| (key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedMap;

    #[test]
    fn extend_and_iteration_preserve_overwrite_position() {
        let mut map = OrderedMap::new();
        map.extend([("b".to_owned(), 1), ("a".to_owned(), 2)]);
        map.extend([("b".to_owned(), 3), ("c".to_owned(), 4)]);
        let borrowed: Vec<_> = (&map).into_iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(borrowed, [("b", 3), ("a", 2), ("c", 4)]);
        let owned: Vec<_> = map.into_iter().collect();
        assert_eq!(
            owned,
            [
                ("b".to_owned(), 3),
                ("a".to_owned(), 2),
                ("c".to_owned(), 4)
            ]
        );
    }

    #[test]
    fn insert_overwrite_keeps_position() {
        let mut map = OrderedMap::new();
        assert_eq!(map.insert("b".to_owned(), 1), None);
        map.insert("a".to_owned(), 2);
        assert_eq!(map.insert("b".to_owned(), 3), Some(1));
        let keys: Vec<_> = map.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(keys, [("b", 3), ("a", 2)]);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("a"));
        assert!(!map.is_empty());
    }

    #[test]
    fn json_roundtrip_keeps_order_and_duplicates_collapse() {
        let text = r#"{"z":1,"a":2,"z":3,"m":4}"#;
        let map: OrderedMap<i32> = serde_json::from_str(text).unwrap();
        assert_eq!(
            serde_json::to_string(&map).unwrap(),
            r#"{"z":3,"a":2,"m":4}"#
        );
        let collected: OrderedMap<i32> = [("y".to_owned(), 1), ("x".to_owned(), 2)]
            .into_iter()
            .collect();
        assert_eq!(
            serde_json::to_string(&collected).unwrap(),
            r#"{"y":1,"x":2}"#
        );
    }
}
