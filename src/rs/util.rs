//! Shared helpers that reproduce Python standard-library behaviour the harness relies on
//! (byte-identical JSON for digests, SHA-256 hex, UTC ISO-8601 timestamps).
//!
//! Python call -> helper:
//! - `json.dumps(v, sort_keys=True, separators=(",", ":"))` -> [`canonical_json`]
//! - `sha256(that)` / `sha256(that + "\n")` -> [`json_digest`] / [`json_digest_line`]
//! - `json.dumps(v, sort_keys=True, indent=2)` -> [`pretty_json`]
//! - `json.dumps(v, sort_keys=True)` (default `", "` / `": "` separators) -> [`sorted_json_spaced`]
//! - `json.dumps(v, indent=2)` without `sort_keys` -> [`pretty_json_ordered`] (struct field order)
//! - `json.dump(v, f)` without `sort_keys` -> [`ordered_json_spaced`] (struct field order)
//! - `json.dumps(v, separators=(',', ':'))` without sorting -> [`ordered_json_compact`]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};

// Keep each unit enum's wire vocabulary in one place without exporting the macro.
macro_rules! wire_enum {
    (
        $(#[$enum_meta:meta])* $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $wire:literal),+ $(,)?
        }
        $(#[$as_str_meta:meta])* as_str;
        $(from_str($value:ident) -> $error:ty { $error_expr:expr })?
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl $name {
            $(#[$as_str_meta])*
            #[must_use]
            $vis const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        $crate::util::wire_enum!(@parse $name, [$($variant => $wire),+] $(, $value, $error, $error_expr)?);
    };
    (@parse $name:ident, [$($variant:ident => $wire:literal),+]) => {};
    (@parse $name:ident, [$($variant:ident => $wire:literal),+], $value:ident, $error:ty, $error_expr:expr) => {
        impl std::str::FromStr for $name {
            type Err = $error;

            fn from_str($value: &str) -> Result<Self, Self::Err> {
                match $value {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err($error_expr),
                }
            }
        }
    };
}

pub(crate) use wire_enum;

/// Errors from the Python-compatible JSON writer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JsonError {
    /// The value could not be converted to JSON by serde.
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    /// Non-finite float reached the writer. serde maps NaN/inf to null first, so this is a guard.
    #[error("non-finite float {0} cannot be serialised as JSON")]
    NonFinite(f64),
}

/// Separator/indent layout of one `json.dumps` call shape.
#[derive(Clone, Copy)]
enum Layout {
    Compact,
    Spaced,
    Indent2,
}

/// Python `json` string escaping with `ensure_ascii=True`.
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            _ => {
                let mut buf = [0_u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Python `float.__repr__`: shortest round-trip digits, scientific below 1e-4 or from 1e16.
fn write_float(out: &mut String, x: f64) -> Result<(), JsonError> {
    if !x.is_finite() {
        return Err(JsonError::NonFinite(x));
    }
    let sci = format!("{x:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let plain = format!("{x}");
        out.push_str(&plain);
        if !plain.contains('.') {
            out.push_str(".0");
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        let _ = write!(out, "{mantissa}e{sign}{:02}", exp.unsigned_abs());
    }
    Ok(())
}

fn write_number(out: &mut String, n: &Number) -> Result<(), JsonError> {
    if n.is_f64() {
        write_float(out, n.as_f64().unwrap_or(f64::NAN))
    } else {
        let _ = write!(out, "{n}");
        Ok(())
    }
}

fn newline(out: &mut String, layout: Layout, depth: usize) {
    if matches!(layout, Layout::Indent2) {
        out.push('\n');
        out.extend(std::iter::repeat_n("  ", depth));
    }
}

const fn item_separator(layout: Layout) -> &'static str {
    match layout {
        Layout::Compact | Layout::Indent2 => ",",
        Layout::Spaced => ", ",
    }
}

const fn key_separator(layout: Layout) -> &'static str {
    match layout {
        Layout::Compact => ":",
        Layout::Spaced | Layout::Indent2 => ": ",
    }
}

fn write_object(
    out: &mut String,
    map: &Map<String, Value>,
    layout: Layout,
    depth: usize,
) -> Result<(), JsonError> {
    if map.is_empty() {
        out.push_str("{}");
        return Ok(());
    }
    out.push('{');
    for (i, (key, item)) in map.iter().enumerate() {
        if i > 0 {
            out.push_str(item_separator(layout));
        }
        newline(out, layout, depth + 1);
        write_str(out, key);
        out.push_str(key_separator(layout));
        write_value(out, item, layout, depth + 1)?;
    }
    newline(out, layout, depth);
    out.push('}');
    Ok(())
}

fn write_array(
    out: &mut String,
    items: &[Value],
    layout: Layout,
    depth: usize,
) -> Result<(), JsonError> {
    if items.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(item_separator(layout));
        }
        newline(out, layout, depth + 1);
        write_value(out, item, layout, depth + 1)?;
    }
    newline(out, layout, depth);
    out.push(']');
    Ok(())
}

fn write_value(
    out: &mut String,
    value: &Value,
    layout: Layout,
    depth: usize,
) -> Result<(), JsonError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n)?,
        Value::String(s) => write_str(out, s),
        Value::Array(items) => write_array(out, items, layout, depth)?,
        Value::Object(map) => write_object(out, map, layout, depth)?,
    }
    Ok(())
}

/// Serialise through `serde_json::Value`, whose map is a `BTreeMap` (keys sorted by code
/// point, which equals Python's `sort_keys` order). The `preserve_order` feature must stay off.
fn render<T: Serialize + ?Sized>(value: &T, layout: Layout) -> Result<String, JsonError> {
    let tree = serde_json::to_value(value)?;
    render_value(&tree, layout)
}

fn render_value(value: &Value, layout: Layout) -> Result<String, JsonError> {
    let mut out = String::new();
    write_value(&mut out, value, layout, 0)?;
    Ok(out)
}

/// Like [`canonical_json`], borrowing an existing JSON tree without rebuilding it.
///
/// # Errors
/// A number cannot be represented by the Python-compatible encoder.
pub fn canonical_json_value(value: &Value) -> Result<String, JsonError> {
    render_value(value, Layout::Compact)
}

/// Like [`sorted_json_spaced`], borrowing an existing JSON tree without rebuilding it.
///
/// # Errors
/// A number cannot be represented by the Python-compatible encoder.
pub fn sorted_json_spaced_value(value: &Value) -> Result<String, JsonError> {
    render_value(value, Layout::Spaced)
}

/// Render sorted borrowed object fields without cloning their JSON trees.
pub(crate) fn sorted_json_spaced_fields(
    fields: &BTreeMap<&str, &Value>,
) -> Result<String, JsonError> {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_str(&mut out, key);
        out.push_str(": ");
        write_value(&mut out, value, Layout::Spaced, 1)?;
    }
    out.push('}');
    Ok(out)
}

/// Like [`json_digest`], borrowing an existing JSON tree without rebuilding it.
///
/// # Errors
/// A number cannot be represented by the Python-compatible encoder.
pub fn json_digest_value(value: &Value) -> Result<String, JsonError> {
    Ok(sha256_hex(canonical_json_value(value)?.as_bytes()))
}

/// Like [`pretty_json`], borrowing an existing JSON tree without rebuilding it.
///
/// # Errors
/// A number cannot be represented by the Python-compatible encoder.
pub fn pretty_json_value(value: &Value) -> Result<String, JsonError> {
    render_value(value, Layout::Indent2)
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"))`.
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render(value, Layout::Compact)
}

/// `json.dumps(value, sort_keys=True)` with the default `", "` / `": "` separators.
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn sorted_json_spaced<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render(value, Layout::Spaced)
}

/// `json.dumps(value, sort_keys=True, indent=2)`.
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn pretty_json<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render(value, Layout::Indent2)
}

/// serde formatter that reproduces Python `json` layout and float formatting for the writers
/// that keep struct field order. `indent: None` is the default `", "` / `": "` layout.
struct PyFormatter {
    indent: Option<&'static [u8]>,
    compact: bool,
    depth: usize,
    has_value: bool,
}

impl PyFormatter {
    const fn pretty() -> Self {
        Self {
            indent: Some(b"  "),
            compact: false,
            depth: 0,
            has_value: false,
        }
    }

    const fn spaced() -> Self {
        Self {
            indent: None,
            compact: false,
            depth: 0,
            has_value: false,
        }
    }

    fn element<W: ?Sized + std::io::Write>(
        &self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        match self.indent {
            Some(unit) => {
                writer.write_all(if first { b"\n" } else { b",\n" })?;
                for _ in 0..self.depth {
                    writer.write_all(unit)?;
                }
                Ok(())
            }
            None if first => Ok(()),
            None => writer.write_all(if self.compact { b"," } else { b", " }),
        }
    }

    fn close<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        bracket: &[u8],
    ) -> std::io::Result<()> {
        self.depth = self.depth.saturating_sub(1);
        if let (Some(unit), true) = (self.indent, self.has_value) {
            writer.write_all(b"\n")?;
            for _ in 0..self.depth {
                writer.write_all(unit)?;
            }
        }
        writer.write_all(bracket)
    }

    fn float<W: ?Sized + std::io::Write>(writer: &mut W, x: f64) -> std::io::Result<()> {
        let mut text = String::new();
        write_float(&mut text, x).map_err(std::io::Error::other)?;
        writer.write_all(text.as_bytes())
    }
}

impl serde_json::ser::Formatter for PyFormatter {
    fn write_f32<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f32,
    ) -> std::io::Result<()> {
        Self::float(writer, f64::from(value))
    }

    fn write_f64<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f64,
    ) -> std::io::Result<()> {
        Self::float(writer, value)
    }

    fn begin_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        writer.write_all(b"[")
    }

    fn end_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.close(writer, b"]")
    }

    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.element(writer, first)
    }

    fn end_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        _writer: &mut W,
    ) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        writer.write_all(b"{")
    }

    fn end_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.close(writer, b"}")
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.element(writer, first)
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(if self.compact { b":" } else { b": " })
    }

    fn end_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        _writer: &mut W,
    ) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }
}

/// Serialise with `formatter`, then escape non-ASCII and DEL like `ensure_ascii=True`.
fn render_ordered<T: Serialize + ?Sized>(
    value: &T,
    formatter: PyFormatter,
) -> Result<String, JsonError> {
    let mut buf = Vec::new();
    value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut buf, formatter,
    ))?;
    if buf.iter().all(|byte| *byte < 0x7f) {
        return String::from_utf8(buf)
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
            .map_err(JsonError::Serde);
    }
    // serde_json only writes valid UTF-8.
    let json = String::from_utf8_lossy(&buf);
    Ok(ascii_escape(&json))
}

/// `json.dumps(value, indent=2)` without `sort_keys`: keys stay in struct field order.
/// Floats match Python `repr`; NaN/inf become `null` (Python prints `NaN`).
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn pretty_json_ordered<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render_ordered(value, PyFormatter::pretty())
}

/// `json.dump(value, stream)` / `json.dumps(value)` without `sort_keys`: struct field order,
/// default separators. Floats match Python `repr`; NaN/inf become `null` (Python prints `NaN`).
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn ordered_json_spaced<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render_ordered(value, PyFormatter::spaced())
}

/// Python `json.dumps(value, separators=(',', ':'))`, preserving insertion order.
/// # Errors
/// Returns an error if serialization fails.
pub fn ordered_json_compact<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render_ordered(
        value,
        PyFormatter {
            compact: true,
            ..PyFormatter::spaced()
        },
    )
}

/// Escape every non-ASCII char and DEL as `\uXXXX` (UTF-16 units), like `ensure_ascii=True`.
/// Safe on serde output because such chars only occur inside string literals.
fn ascii_escape(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            out.push(ch);
        } else {
            let mut buf = [0_u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// Lower-case hex SHA-256 of `bytes` (`hashlib.sha256(bytes).hexdigest()`).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// SHA-256 of [`canonical_json`] (no trailing newline).
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn json_digest<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    Ok(sha256_hex(canonical_json(value)?.as_bytes()))
}

/// SHA-256 of [`canonical_json`] followed by `"\n"` (e.g. `task_reservation`, `cyber_evaluation`).
///
/// # Errors
/// JSON serialization fails or a value cannot be represented by the Python-compatible encoder.
pub fn json_digest_line<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    let mut text = canonical_json(value)?;
    text.push('\n');
    Ok(sha256_hex(text.as_bytes()))
}

/// Days since 1970-01-01 to a proleptic Gregorian `(year, month, day)` (Howard Hinnant).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// Format a Unix time as Python's `datetime.fromtimestamp(t, UTC).isoformat()`.
#[must_use]
pub fn iso_utc(secs: i64, micros: u32) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let base = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

/// `datetime.now(UTC).isoformat()`.
#[must_use]
pub fn utc_now_iso() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    iso_utc(secs, now.subsec_micros())
}

pub mod bin_main;
mod casefold_table;
pub mod logging;
pub mod ordered_map;

/// Python `str.casefold()`: per-char `to_lowercase()` except the generated exception table.
#[must_use]
pub fn casefold(s: &str) -> String {
    let table = casefold_table::CASEFOLD_EXCEPTIONS;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match table.binary_search_by_key(&ch, |&(c, _)| c) {
            Ok(index) => {
                if let Some(&(_, folded)) = table.get(index) {
                    out.push_str(folded);
                }
            }
            Err(_) => out.extend(ch.to_lowercase()),
        }
    }
    out
}

const MAX_SYMLINK_DEPTH: usize = 40;

#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    use std::ffi::OsString;
    use std::path::Prefix;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let mut plain = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut unc = OsString::from(r"\\");
            unc.push(server);
            unc.push(r"\");
            unc.push(share);
            PathBuf::from(unc)
        }
        _ => return path,
    };
    plain.extend(components);
    plain
}

#[cfg(all(test, windows))]
mod strip_verbatim_tests {
    use super::strip_verbatim;
    use std::path::PathBuf;

    #[test]
    fn strips_disk_and_unc_prefixes_but_preserves_volume_prefixes() {
        for (input, expected) in [
            (r"\\?\C:\a", r"C:\a"),
            (r"\\?\UNC\srv\share\a", r"\\srv\share\a"),
            (
                r"\\?\Volume{00000000-0000-0000-0000-000000000000}\a",
                r"\\?\Volume{00000000-0000-0000-0000-000000000000}\a",
            ),
        ] {
            assert_eq!(
                strip_verbatim(PathBuf::from(input)),
                PathBuf::from(expected)
            );
        }
    }
}

#[cfg(not(windows))]
const fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// `Path.resolve(strict=True)`: canonical path of an existing entry.
///
/// # Errors
/// The path does not exist or cannot be canonicalized.
pub fn resolve_strict(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(strip_verbatim)
}

fn resolve_components(path: &Path, depth: usize) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if depth < MAX_SYMLINK_DEPTH
                    && std::fs::symlink_metadata(&resolved).is_ok_and(|m| m.is_symlink())
                    && let Ok(target) = std::fs::read_link(&resolved)
                {
                    resolved.pop();
                    resolved = resolve_components(&resolved.join(target), depth + 1);
                }
            }
        }
    }
    resolved
}

/// `Path.resolve(strict=False)`: follows existing symlinks, tolerates missing tails.
#[must_use]
pub fn resolve_lenient(path: &Path) -> PathBuf {
    let start = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    resolve_components(&start, 0)
}

/// In-process stand-in for advisory file locks on platforms without `flock`.
#[cfg(not(unix))]
pub(crate) mod process_lock {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex};

    static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    static RELEASED: Condvar = Condvar::new();

    pub(crate) fn try_acquire(key: &Path) -> Option<Guard> {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(key.to_path_buf())
            .then(|| Guard(key.to_path_buf()))
    }

    pub(crate) fn acquire(key: &Path) {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while held.contains(key) {
            held = RELEASED
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        held.insert(key.to_path_buf());
    }

    pub(crate) fn release(key: &Path) {
        HELD.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
        RELEASED.notify_all();
    }

    /// Releases `key` on drop, including during unwinding.
    #[derive(Debug)]
    pub(crate) struct Guard(pub(crate) PathBuf);

    impl Drop for Guard {
        fn drop(&mut self) {
            release(&self.0);
        }
    }
}

/// `os.urandom(bytes).hex()` / `secrets.token_hex(bytes)`: lowercase hex of OS random bytes.
///
/// # Errors
/// Returns the OS error when the system random source is unavailable.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0_u8; bytes];
    getrandom::getrandom(&mut buf).map_err(std::io::Error::from)?;
    let mut out = String::with_capacity(bytes * 2);
    for byte in buf {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// `uuid.uuid4().hex`: 32 lowercase hex digits with version 4 and RFC 4122 variant bits.
///
/// # Errors
/// Returns the OS error when the system random source is unavailable.
pub fn uuid4_hex() -> std::io::Result<String> {
    let mut buf = [0_u8; 16];
    getrandom::getrandom(&mut buf).map_err(std::io::Error::from)?;
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    Ok(buf.iter().fold(String::with_capacity(32), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    }))
}

#[cfg(test)]
mod random_tests {
    use super::{random_hex, uuid4_hex};

    fn is_lower_hex(value: &str) -> bool {
        value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn random_hex_has_two_digits_per_byte() {
        let value = random_hex(8).unwrap();
        assert_eq!(value.len(), 16);
        assert!(is_lower_hex(&value));
        assert_ne!(value, random_hex(8).unwrap());
    }

    #[test]
    fn uuid4_hex_is_version_4_variant_1() {
        for _ in 0..64 {
            let value = uuid4_hex().unwrap();
            assert_eq!(value.len(), 32);
            assert!(is_lower_hex(&value));
            assert_eq!(value.as_bytes()[12], b'4');
            assert!(matches!(value.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
        }
        assert_ne!(uuid4_hex().unwrap(), uuid4_hex().unwrap());
    }
}

#[cfg(test)]
mod casefold_tests {
    use super::casefold;

    #[test]
    fn matches_python_casefold() {
        assert_eq!(casefold("STRASSE"), casefold("stra\u{df}e"));
        assert_eq!(casefold("stra\u{df}e"), "strasse");
        assert_eq!(casefold("\u{b5}"), "\u{3bc}");
        assert_eq!(casefold("\u{17f}"), "s");
        assert_eq!(casefold("\u{1e9e}"), "ss");
        assert_eq!(casefold("\u{fb01}"), "fi");
        assert_eq!(casefold("\u{3c2}"), "\u{3c3}");
        assert_eq!(casefold("\u{13a0}"), "\u{13a0}");
        assert_eq!(casefold("\u{130}"), "i\u{307}");
        assert_eq!(casefold("Hello"), "hello");
    }

    #[test]
    fn table_is_sorted() {
        let table = super::casefold_table::CASEFOLD_EXCEPTIONS;
        assert!(table.windows(2).all(|w| w[0].0 < w[1].0));
    }
}

/// Open (creating) a SQLite database at a literal path, like Python's `sqlite3.connect`.
///
/// URI filenames are not interpreted, so `file:...?vfs=` paths cannot bypass path checks.
///
/// # Errors
///
/// Returns the SQLite error if the database cannot be opened.
pub fn open_sqlite(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/// Rebuilds `path` from its components the way `pathlib` does: drops trailing
/// separators and `.` components and collapses repeated separators.
///
/// `Path::is_symlink` on `link/` follows the link on POSIX, so every security
/// lstat goes through this first. An empty result is `.`.
#[must_use]
pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `str(PurePosixPath(p))`: `./a` is `a`, `a//b` is `a/b`, `.` stays `.`, `..` is kept.
#[must_use]
pub fn py_path_str(path: &Path) -> String {
    lexical(path).to_string_lossy().into_owned()
}

/// `os.lstat` / `Path.lstat` on the lexical form of `path`.
///
/// # Errors
/// The entry does not exist or cannot be inspected.
pub fn lstat(path: &Path) -> std::io::Result<std::fs::Metadata> {
    std::fs::symlink_metadata(lexical(path))
}

/// Python `str.isspace` for a character (what `str.strip()` removes).
#[must_use]
pub fn is_py_space(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// Expose Python line splitting for external property tests.
#[doc(hidden)]
#[must_use]
pub fn splitlines_keepends_for_testing(text: &str) -> Vec<&str> {
    splitlines_keepends(text)
}

/// Expose slug validation for external property tests.
#[doc(hidden)]
#[must_use]
pub fn is_slug_id_for_testing(value: &str) -> bool {
    is_slug_id(value)
}

/// Python `str.splitlines(keepends=True)`.
pub(crate) fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let mut end = index + ch.len_utf8();
        match ch {
            '\r' => {
                if let Some(&(next, '\n')) = chars.peek() {
                    end = next + 1;
                    chars.next();
                }
            }
            '\n' | '\u{b}' | '\u{c}' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
            }
            _ => continue,
        }
        lines.extend(text.get(start..end));
        start = end;
    }
    lines.extend(text.get(start..).filter(|rest| !rest.is_empty()));
    lines
}

/// True when every character is Python whitespace (`not value.strip()`).
#[must_use]
pub(crate) fn is_py_blank(value: &str) -> bool {
    value.chars().all(is_py_space)
}

/// `re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', value)`.
#[must_use]
pub(crate) fn is_slug_id(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `str.strip()` with no argument.
#[must_use]
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// Pathlib-style symlink test that ignores trailing separators.
pub trait PathLexical {
    /// `Path.is_symlink()` as pathlib evaluates it.
    fn is_symlink_lexical(&self) -> bool;
}

impl PathLexical for Path {
    fn is_symlink_lexical(&self) -> bool {
        lexical(self).is_symlink()
    }
}

/// `str.strip()` as a method.
pub trait PyStr {
    /// `str.strip()` with no argument.
    fn py_strip(&self) -> &str;
}

impl PyStr for str {
    fn py_strip(&self) -> &str {
        py_strip(self)
    }
}

/// Convert a path without replacing invalid Unicode.
pub(crate) fn path_str(path: &Path) -> std::io::Result<&str> {
    path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Path is not valid UTF-8")
    })
}

/// Create one owner-only directory, failing if it already exists.
pub(crate) fn create_private_dir(path: &Path) -> std::io::Result<()> {
    private_dir_builder(false).create(path)
}

fn private_dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
}

/// Create missing parents and the leaf with owner-only permissions.
pub(crate) fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    private_dir_builder(true).create(path)
}

/// Create default-mode parents and a private leaf, accepting existing directories.
pub(crate) fn ensure_private_leaf_dir(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    match create_private_dir(path) {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod path_tests {
    use super::*;

    #[test]
    fn unicode_path_is_preserved() {
        assert_eq!(path_str(Path::new(" src/é\n")).unwrap(), " src/é\n");
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_is_rejected() {
        use std::os::unix::ffi::OsStrExt as _;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"src/\xff"));
        assert_eq!(
            path_str(path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_surrogate_is_rejected() {
        use std::os::windows::ffi::OsStringExt as _;
        let path = PathBuf::from(std::ffi::OsString::from_wide(&[0xd800]));
        assert_eq!(
            path_str(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(test)]
mod splitlines_tests {
    use super::splitlines_keepends as splitlines;

    #[test]
    fn matches_python_splitlines_keepends() {
        assert_eq!(splitlines(""), [] as [&str; 0]);
        for sep in [
            "\n", "\r", "\r\n", "\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}", "\u{85}",
            "\u{2028}", "\u{2029}",
        ] {
            let text = format!("a{sep}b");
            assert_eq!(splitlines(&text), vec![format!("a{sep}"), "b".to_owned()]);
            assert_eq!(splitlines(sep), vec![sep]);
        }
        assert_eq!(splitlines("a\r\n\r\nb\r"), vec!["a\r\n", "\r\n", "b\r"]);
    }
}
