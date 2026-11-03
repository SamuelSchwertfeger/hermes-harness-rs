//! Differential tests: Rust parsers versus the vendored Python originals.
//!
//! Runs only when `HH_DIFF_PYTHON` names a Python 3.14 interpreter; otherwise the
//! test prints a skip note and passes. `HH_DIFF_CASES` sets the random case count
//! per pair (default 2000) and `HH_DIFF_SEED` the generator seed. Every pair is one
//! batch over stdin to `python3 -I tests/differential/oracle.py` (bytes are base64).
//! Known gaps are allowlisted by narrow predicates in [`allow`]; see `DIVERGENCES.md`
//! and `tests/differential/FINDINGS.md`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::option_if_let_else,
    clippy::range_plus_one,
    clippy::needless_pass_by_value
)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::process::{Command, Stdio};

use hermes_harness::broker::live_capability_broker::decode_request_for_fuzzing;
use hermes_harness::git::worktrees::split_z_paths;
use hermes_harness::multiagent::model_edits::parse_edit_proposal;
use hermes_harness::multiagent::ollama_discovery::parse_ollama_tags;
use hermes_harness::multiagent::plan_io::parse_plan_json;
use hermes_harness::multiagent::replacement_proposals::parse_replacement_proposal;
use hermes_harness::util::{is_slug_id_for_testing, splitlines_keepends_for_testing};
use serde_json::{Map, Value, json};

const DEFAULT_CASES: usize = 2000;
const DEFAULT_SEED: u64 = 0x4845_524d_4553_0001;
const REQUEST_LIMIT: usize = 131_072;

/// Deterministic splitmix64 generator so failures reproduce from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// One generated input.
#[derive(Debug, Clone)]
enum Case {
    Bytes(Vec<u8>),
    Text(String),
    Task {
        text: String,
        task_id: String,
        owned: Vec<String>,
    },
}

impl Case {
    fn raw(&self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) => bytes.clone(),
            Self::Text(text) | Self::Task { text, .. } => text.as_bytes().to_vec(),
        }
    }

    fn line(&self, pair: &str) -> String {
        let value = match self {
            Self::Bytes(bytes) => json!({"pair": pair, "b64": base64(bytes)}),
            Self::Text(text) => json!({"pair": pair, "text": text}),
            Self::Task {
                text,
                task_id,
                owned,
            } => json!({"pair": pair, "text": text, "task_id": task_id, "owned": owned}),
        };
        value.to_string()
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Rust result: accepted flag plus normalized value.
type Rs = (bool, Value);

/// Rust-side runner.
type RustFn = fn(&Case) -> Rs;

/// Python oracle result.
struct Py {
    ok: bool,
    value: Value,
    surrogate: bool,
    error: String,
    alt: Option<Value>,
}

impl Py {
    fn parse(line: &str) -> Self {
        let v: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad oracle line {line:?}: {e}"));
        Self {
            ok: v["ok"].as_bool().unwrap(),
            value: v["value"].clone(),
            surrogate: v.get("surrogate").and_then(Value::as_bool).unwrap_or(false),
            error: v["error"].as_str().unwrap_or_default().to_owned(),
            alt: v.get("alt").cloned(),
        }
    }
}

struct Pair {
    name: &'static str,
    cases: Vec<Case>,
    rust: fn(&Case) -> Rs,
}

// ---------------------------------------------------------------- Rust side

fn text_of(case: &Case) -> (&str, &str, &[String]) {
    match case {
        Case::Task {
            text,
            task_id,
            owned,
        } => (text, task_id, owned),
        Case::Text(text) => (text, "", &[]),
        Case::Bytes(_) => ("", "", &[]),
    }
}

fn rs_broker(case: &Case) -> Rs {
    match decode_request_for_fuzzing(&case.raw()) {
        Ok(map) => (true, json!(map)),
        Err(_) => (false, Value::Null),
    }
}

fn rs_plan(case: &Case) -> Rs {
    let (text, _, _) = text_of(case);
    match parse_plan_json(text) {
        Ok(plan) => {
            let nodes: Vec<Value> = plan
                .nodes
                .iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "objective": n.objective,
                        "owned_paths": n.owned_paths,
                        "dependencies": n.dependencies,
                        "role": n.role.as_str(),
                    })
                })
                .collect();
            (
                true,
                json!({
                    "id": plan.id,
                    "objective": plan.objective,
                    "primary_identity": plan.primary_identity,
                    "nodes": nodes,
                }),
            )
        }
        Err(_) => (false, Value::Null),
    }
}

fn rs_replacement(case: &Case) -> Rs {
    let (text, task_id, owned) = text_of(case);
    match parse_replacement_proposal(text, task_id, owned) {
        Ok(rows) => (
            true,
            Value::Array(
                rows.iter()
                    .map(|r| {
                        json!({
                            "path": r.path,
                            "expected_sha256": r.expected_sha256,
                            "content": r.content,
                        })
                    })
                    .collect(),
            ),
        ),
        Err(_) => (false, Value::Null),
    }
}

fn rs_edit(case: &Case) -> Rs {
    let (text, task_id, owned) = text_of(case);
    match parse_edit_proposal(text, task_id, owned) {
        Ok(p) => (
            true,
            json!({
                "task_id": p.task_id,
                "files": p.files.iter()
                    .map(|f| json!({"path": f.path, "content": f.content, "sha256": f.sha256}))
                    .collect::<Vec<_>>(),
            }),
        ),
        Err(_) => (false, Value::Null),
    }
}

fn rs_ollama(case: &Case) -> Rs {
    match parse_ollama_tags(&case.raw()) {
        Ok(models) => (
            true,
            Value::Array(
                models
                    .iter()
                    .map(|m| json!([m.name, m.size_bytes]))
                    .collect(),
            ),
        ),
        Err(_) => (false, Value::Null),
    }
}

fn rs_splitlines(case: &Case) -> Rs {
    let (text, _, _) = text_of(case);
    (true, json!(splitlines_keepends_for_testing(text)))
}

fn rs_slug(case: &Case) -> Rs {
    let (text, _, _) = text_of(case);
    (true, json!(is_slug_id_for_testing(text)))
}

fn rs_splitz(case: &Case) -> Rs {
    let (text, _, _) = text_of(case);
    (true, json!(split_z_paths(text).collect::<Vec<_>>()))
}

// ------------------------------------------------------------- allowlist

fn contains_surrogate_escape(raw: &[u8]) -> bool {
    // Raw UTF-8-encoded surrogates (ED A0..BF xx): json.loads(bytes) decodes with surrogatepass.
    if raw
        .windows(2)
        .any(|w| w[0] == 0xED && (0xA0..=0xBF).contains(&w[1]))
    {
        return true;
    }
    let lower = String::from_utf8_lossy(raw).to_ascii_lowercase();
    lower.match_indices("\\ud").any(|(i, _)| {
        lower
            .as_bytes()
            .get(i + 3)
            .is_some_and(|b| matches!(b, b'8'..=b'9' | b'a'..=b'f'))
    })
}

fn contains(raw: &[u8], needle: &[u8]) -> bool {
    raw.windows(needle.len()).any(|w| w == needle)
}

fn longest_digit_run(raw: &[u8]) -> usize {
    let mut best = 0;
    let mut run = 0;
    for b in raw {
        if b.is_ascii_digit() {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// True when `serde_json` itself rejects the bytes (syntax or range level), so the
/// mismatch is provably a JSON-layer difference and not domain validation.
fn serde_rejects(raw: &[u8]) -> bool {
    serde_json::from_slice::<Value>(raw).is_err()
}

/// Sizes of `models[*].size` in the document, when it parses at all.
fn model_sizes(raw: &[u8]) -> Vec<Value> {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|v| v.get("models").and_then(Value::as_array).cloned())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("size").cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// Narrow allowlist of documented divergences and open findings.
fn allow(pair: &str, case: &Case, rs: &Rs, py: &Py) -> Option<&'static str> {
    let raw = case.raw();
    // Python accepted, Rust rejected, and serde_json rejects the bytes at JSON level.
    let rust_json_only = py.ok && !rs.0 && serde_rejects(&raw);
    match pair {
        "broker" | "ollama" | "plan" | "replacement" | "edit"
            if rust_json_only && (py.surrogate || contains_surrogate_escape(&raw)) =>
        {
            // FINDING 2: Python's json accepts lone-surrogate escapes; serde_json rejects them.
            Some("FINDING 2 (lone surrogate escape)")
        }
        "broker" | "ollama"
            if rust_json_only
                && (raw.starts_with(&[0xEF, 0xBB, 0xBF])
                    || raw.starts_with(&[0xFF, 0xFE])
                    || raw.starts_with(&[0xFE, 0xFF])
                    || raw.iter().take(4).any(|b| *b == 0)) =>
        {
            // FINDING 1: json.loads(bytes) auto-detects UTF-8-BOM/UTF-16/UTF-32; serde does not.
            Some("FINDING 1 (bytes encoding autodetect/BOM)")
        }
        "ollama" if rust_json_only && (contains(&raw, b"NaN") || contains(&raw, b"Infinity")) => {
            // FINDING 3: Python json accepts NaN/Infinity literals in ignored fields.
            Some("FINDING 3 (NaN/Infinity literal)")
        }
        "ollama"
            if rust_json_only && raw.iter().filter(|b| matches!(b, b'[' | b'{')).count() >= 128 =>
        {
            // FINDING 4: serde_json recursion limit (128) versus Python's ~1000.
            Some("FINDING 4 (nesting depth >= 128)")
        }
        "ollama"
            if py.ok
                && !rs.0
                && model_sizes(&raw)
                    .iter()
                    .any(|s| s.as_f64().is_some_and(|f| f == 0.0 && f.is_sign_negative())) =>
        {
            // FINDING 7: Python reads `-0` as int 0; serde_json reads it as float -0.0 and as_u64 fails.
            Some("FINDING 7 (size -0)")
        }
        "ollama"
            if py.ok
                && !rs.0
                && longest_digit_run(&raw) >= 20
                && (serde_rejects(&raw)
                    || model_sizes(&raw).iter().any(|s| {
                        s.as_u64().is_none()
                            && s.as_f64()
                                .is_some_and(|f| f >= 18_446_744_073_709_552_000.0)
                    })) =>
        {
            // FINDING 5a: size above u64::MAX (Python accepts; Rust has no u64 and rejects).
            Some("FINDING 5 (integer above u64::MAX)")
        }
        "ollama"
            if !py.ok
                && rs.0
                && longest_digit_run(&raw) > 4300
                && py.error.starts_with("ValueError: Exceeds the limit") =>
        {
            // FINDING 5b: Python's int str-digits limit (4300) rejects; Rust accepts.
            Some("FINDING 5 (python int digit limit)")
        }
        "splitz" => {
            let alt = py.alt.as_ref()?;
            if &rs.1 == alt {
                // DIVERGENCES.md: "NUL-delimited Git paths retain whitespace" (no strip).
                Some("DIVERGENCES.md (git -z output not stripped)")
            } else {
                None
            }
        }
        _ => None,
    }
}

// ------------------------------------------------------------- generators

const EDGE: &[&str] = &[
    "", " ", "\t", "\u{feff}", "\0", "\u{2028}", "\u{2029}", "\u{b}", "\u{c}", "\u{1c}", "\u{1d}",
    "\u{1e}", "\u{1f}", "\u{85}", "\u{a0}", "\u{3000}", "\u{200b}", "\u{180e}", "\r\n", "\r", "\n",
    "..", ".", "/abs", "a\\b", "../x", "a/../b", "./a", "a//b", ".git/x", "src/", "src\0x", "é",
    "😀", "\u{ffff}", "a b",
];

const WS_CHARS: &[char] = &[
    '\t', '\n', '\u{b}', '\u{c}', '\r', '\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', ' ', '\u{85}',
    '\u{a0}', '\u{1680}', '\u{180e}', '\u{2000}', '\u{2005}', '\u{200a}', '\u{200b}', '\u{2028}',
    '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
];

fn rand_string(rng: &mut Rng) -> String {
    match rng.below(10) {
        0..=5 => {
            let len = 1 + rng.below(10);
            (0..len)
                .map(|_| char::from(*rng.pick(b"abcdefghijklmnopqrstuvwxyz0123456789-_ .")))
                .collect()
        }
        6 | 7 => (*rng.pick(EDGE)).to_owned(),
        8 => format!("{}{}", rng.pick(EDGE), rand_string_ascii(rng)),
        _ => {
            let tail = rng.below(2);
            (0..1 + rng.below(4))
                .map(|_| *rng.pick(WS_CHARS))
                .chain("x".chars().take(tail))
                .collect()
        }
    }
}

fn rand_string_ascii(rng: &mut Rng) -> String {
    (0..1 + rng.below(6))
        .map(|_| char::from(*rng.pick(b"abcdefghijklmnopqrstuvwxyz0123456789-")))
        .collect()
}

fn rand_nonblank(rng: &mut Rng) -> String {
    let s = rand_string_ascii(rng);
    if rng.chance(10) {
        format!("{}{s}{}", rng.pick(WS_CHARS), rng.pick(WS_CHARS))
    } else {
        s
    }
}

fn weird_value(rng: &mut Rng) -> Value {
    match rng.below(12) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(0),
        3 => json!(-1),
        4 => json!(1.5),
        5 => json!(u64::MAX),
        6 => json!([]),
        7 => json!({}),
        8 => json!([1, "a"]),
        9 => json!({"a": 1}),
        _ => Value::String(rand_string(rng)),
    }
}

/// Replace, drop or add something at a random place in the tree.
fn mutate_value(rng: &mut Rng, value: &mut Value) {
    match value {
        Value::Object(map) if !map.is_empty() && rng.chance(70) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            let key = rng.pick(&keys).clone();
            match rng.below(5) {
                0 => {
                    map.remove(&key);
                }
                1 => {
                    map.insert(rand_string_ascii(rng), weird_value(rng));
                }
                _ => {
                    if let Some(child) = map.get_mut(&key) {
                        mutate_value(rng, child);
                    }
                }
            }
        }
        Value::Array(items) if !items.is_empty() && rng.chance(70) => {
            let i = rng.below(items.len());
            match rng.below(5) {
                0 => {
                    items.remove(i);
                }
                1 => items.push(weird_value(rng)),
                2 => {
                    let copy = items[i].clone();
                    items.push(copy);
                }
                _ => mutate_value(rng, &mut items[i]),
            }
        }
        other => *other = weird_value(rng),
    }
}

fn mutate_text(rng: &mut Rng, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let at = rng.below(chars.len() + 1);
    let head: String = chars[..at].iter().collect();
    let tail: String = chars[at..].iter().collect();
    match rng.below(8) {
        0 => format!("{head}{}{tail}", rng.pick(EDGE)),
        1 => {
            let cut = (at + 1 + rng.below(8)).min(chars.len());
            format!("{head}{}", chars[cut..].iter().collect::<String>())
        }
        2 => head,
        3 => format!("\u{feff}{text}"),
        4 => format!("{}{text}{}", rng.pick(WS_CHARS), rng.pick(WS_CHARS)),
        5 => text.replacen('{', &format!("{{\"{}\":null,", rand_string_ascii(rng)), 1),
        6 => {
            // Duplicate an existing key by repeating the first quoted token as a key.
            let key = text.split('"').nth(1).unwrap_or("id").to_owned();
            text.replacen('{', &format!("{{\"{key}\":\"dup\","), 1)
        }
        _ => {
            let token = rng.pick(&[
                "NaN",
                "Infinity",
                "-Infinity",
                "1e999",
                "01",
                "+1",
                "\\ud800",
                "\\u0000",
                "\\x41",
            ]);
            format!("{head}{token}{tail}")
        }
    }
}

fn mutate_bytes(rng: &mut Rng, bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let at = rng.below(out.len() + 1);
    match rng.below(7) {
        0 if !out.is_empty() => {
            let i = at.min(out.len() - 1);
            out[i] = rng.next() as u8;
        }
        1 => out.truncate(at),
        2 => {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend(&out);
            out = v;
        }
        3 => out.insert(at, 0),
        4 => out.insert(at, 0xFF),
        5 => {
            // Re-encode as UTF-16LE or UTF-32BE.
            let s = String::from_utf8_lossy(&out).into_owned();
            out = if rng.chance(50) {
                s.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                s.chars().flat_map(|c| u32::from(c).to_be_bytes()).collect()
            };
        }
        _ => {
            let seq: &[u8] = rng.pick::<&[u8]>(&[
                &[0xC0u8, 0x80][..],
                &[0xED, 0xA0, 0x80],
                &[0x80],
                &[0xF8, 0x88, 0x80, 0x80, 0x80],
            ]);
            for (k, b) in seq.iter().enumerate() {
                out.insert((at + k).min(out.len()), *b);
            }
        }
    }
    out
}

/// Turn a document into text: mostly mutated structurally, sometimes textually.
fn render(rng: &mut Rng, mut doc: Value) -> String {
    let roll = rng.below(100);
    if roll >= 30 {
        for _ in 0..1 + rng.below(2) {
            mutate_value(rng, &mut doc);
        }
    }
    let mut text = serde_json::to_string(&doc).unwrap();
    if roll >= 75 {
        text = mutate_text(rng, &text);
    }
    if roll >= 93 {
        text = rand_string(rng);
    }
    text
}

fn pad_to(mut text: String, len: usize) -> String {
    while text.len() < len {
        text.push(' ');
    }
    text
}

fn broker_doc(rng: &mut Rng) -> Value {
    let mut map = Map::new();
    for key in [
        "capability",
        "task_id",
        "role",
        "session_id",
        "submission_id",
        "method",
        "text",
    ] {
        map.insert(key.to_owned(), Value::String(rand_string(rng)));
    }
    Value::Object(map)
}

fn gen_owned(rng: &mut Rng) -> Vec<String> {
    let pool = [
        "src",
        "docs/a.md",
        "lib/core",
        "tests",
        "a b",
        "src/",
        ".",
        "",
        "x",
    ];
    (0..1 + rng.below(3))
        .map(|_| (*rng.pick(&pool)).to_owned())
        .collect()
}

fn gen_path(rng: &mut Rng, owned: &[String]) -> String {
    if rng.chance(70) {
        let base = rng.pick(owned).clone();
        if rng.chance(40) {
            base
        } else {
            format!("{base}/{}", rand_string_ascii(rng))
        }
    } else {
        (*rng.pick(EDGE)).to_owned()
    }
}

fn hex64(rng: &mut Rng) -> String {
    let s: String = (0..64)
        .map(|_| char::from(*rng.pick(b"0123456789abcdef")))
        .collect();
    if rng.chance(5) { s.to_uppercase() } else { s }
}

fn plan_doc(rng: &mut Rng) -> Value {
    let count = 1 + rng.below(5);
    let mut nodes = Vec::new();
    for i in 0..count {
        let deps: Vec<String> = (0..i)
            .filter(|_| rng.chance(30))
            .map(|j| format!("n{j}"))
            .collect();
        let mut node = json!({
            "id": format!("n{i}"),
            "objective": rand_nonblank(rng),
            "owned_paths": [format!("src/m{i}")],
            "role": *rng.pick(&["coding", "cyber-warrior"]),
        });
        if rng.chance(70) {
            node["dependencies"] = json!(deps);
        }
        if rng.chance(20) {
            node["owned_paths"] = json!([format!("src/m{i}"), format!("docs/d{i}.md")]);
        }
        nodes.push(node);
    }
    let mut doc = json!({"id": "plan-1", "objective": rand_nonblank(rng), "nodes": nodes});
    if rng.chance(50) {
        doc["primary_identity"] = json!(rand_nonblank(rng));
    }
    doc
}

fn gen_task(rng: &mut Rng) -> (String, Vec<String>) {
    let task = if rng.chance(90) {
        "task-1".to_owned()
    } else {
        rand_string(rng)
    };
    (task, gen_owned(rng))
}

fn replacement_doc(rng: &mut Rng, owned: &[String]) -> Value {
    let rows: Vec<Value> = (0..1 + rng.below(3))
        .map(|_| {
            json!({
                "path": gen_path(rng, owned),
                "expected_sha256": hex64(rng),
                "content": rand_string(rng),
            })
        })
        .collect();
    json!({"task_id": "task-1", "replacements": rows})
}

fn edit_doc(rng: &mut Rng, owned: &[String]) -> Value {
    let rows: Vec<Value> = (0..1 + rng.below(3))
        .map(|_| json!({"path": gen_path(rng, owned), "content": rand_string(rng)}))
        .collect();
    json!({"task_id": "task-1", "files": rows})
}

fn ollama_doc(rng: &mut Rng) -> Value {
    let models: Vec<Value> = (0..rng.below(4))
        .map(|_| {
            let mut m = json!({"name": rand_nonblank(rng), "size": rng.next() >> rng.below(60)});
            if rng.chance(30) {
                m["digest"] = json!("sha256:abc");
                m["details"] = json!({"family": "x", "nested": [1, 2, {"a": null}]});
            }
            m
        })
        .collect();
    let mut doc = json!({"models": models});
    if rng.chance(20) {
        doc["extra"] = weird_value(rng);
    }
    doc
}

fn random_text(rng: &mut Rng) -> String {
    (0..rng.below(24))
        .map(|_| match rng.below(6) {
            0 => *rng.pick(WS_CHARS),
            1 => char::from(*rng.pick(b"abc012-_ ")),
            2 => '\0',
            3 => char::from_u32(rng.next() as u32 % 0x30000).unwrap_or('?'),
            4 => *rng.pick(&['\n', '\r']),
            _ => char::from(*rng.pick(b"abcdefghijklmnopqrstuvwxyz0123456789-")),
        })
        .collect()
}

fn slug_text(rng: &mut Rng) -> String {
    match rng.below(5) {
        0 => random_text(rng),
        1 => {
            let n = 60 + rng.below(8);
            let mut s: String = (0..n).map(|_| char::from(*rng.pick(b"ab0-"))).collect();
            if rng.chance(30) {
                s.push_str(rng.pick::<&str>(&["\n", "é", "\u{ff11}", "A", "_"]));
            }
            s
        }
        _ => {
            let mut s = rand_string_ascii(rng).replace('_', "-");
            if rng.chance(30) {
                s.push_str(rng.pick::<&str>(&["\n", "\r", " ", "-", "--"]));
            }
            if rng.chance(15) {
                s.insert(0, '-');
            }
            s
        }
    }
}

fn z_text(rng: &mut Rng) -> String {
    let n = rng.below(6);
    let mut s = String::new();
    for _ in 0..n {
        match rng.below(6) {
            0 => s.push(' '),
            1 => s.push_str(rng.pick::<&str>(EDGE)),
            2 => {}
            _ => s.push_str(&rand_string_ascii(rng)),
        }
        if rng.chance(85) {
            s.push('\0');
        }
    }
    if rng.chance(20) {
        s.push_str(rng.pick::<&str>(&["\n", " ", "\t\0", "\0\0"]));
    }
    s
}

// --------------------------------------------------------- hand-written cases

fn nested(depth: usize, inner: &str) -> String {
    format!("{}{inner}{}", "[".repeat(depth), "]".repeat(depth))
}

fn hand_broker() -> Vec<Case> {
    let plain = json!({"capability":"c","task_id":"t","role":"r","session_id":"s","submission_id":"i","method":"m","text":"hello"});
    let text = serde_json::to_string(&plain).unwrap();
    let with_text = |t: &str| {
        let mut d = plain.clone();
        d["text"] = json!(t);
        serde_json::to_string(&d).unwrap()
    };
    let mut cases: Vec<Vec<u8>> = vec![
        vec![],
        b" ".to_vec(),
        b"\n".to_vec(),
        b"{}".to_vec(),
        b"null".to_vec(),
        text.as_bytes().to_vec(),
        [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        text.replace("hello", "he\\u0000llo").into_bytes(),
        text.replace("hello", "he\0llo").into_bytes(),
        text.replace("hello", "a\u{2028}b\u{85}c\u{b}d\u{c}e\u{1c}f")
            .into_bytes(),
        text.replace(',', ",\r\n").into_bytes(),
        text.replace("\"text\":\"hello\"", "\"text\":\"a\",\"text\":\"b\"")
            .into_bytes(),
        text.replace("\"hello\"", "NaN").into_bytes(),
        text.replace("\"hello\"", "Infinity").into_bytes(),
        text.replace("\"hello\"", "123456789012345678901234567890")
            .into_bytes(),
        text.replace("\"hello\"", &nested(200, "1")).into_bytes(),
        nested(100_000, "1").into_bytes(),
        text.replace("hello", "\\ud800").into_bytes(),
        text.replace("hello", "\\ud83d\\ude00").into_bytes(),
        b"\xff\xfe".to_vec(),
        b"\xff".to_vec(),
        vec![0xC0, 0x80],
        pad_to(text.clone(), 16_384).into_bytes(),
        pad_to(text.clone(), 16_385).into_bytes(),
        pad_to(text.clone(), 16_383).into_bytes(),
        with_text(&"a".repeat(16_384)).into_bytes(),
        text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        text.chars()
            .flat_map(|c| u32::from(c).to_le_bytes())
            .collect(),
        text.chars()
            .flat_map(|c| u32::from(c).to_be_bytes())
            .collect(),
        [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect(),
    ];
    let mut inv = text.into_bytes();
    let at = inv.len() - 8;
    inv[at] = 0xFF;
    cases.push(inv);
    cases.into_iter().map(Case::Bytes).collect()
}

fn hand_plan() -> Vec<Case> {
    let base = json!({"id":"p1","objective":"do it","nodes":[
        {"id":"a","objective":"x","owned_paths":["src/a"],"role":"coding"},
        {"id":"b","objective":"y","owned_paths":["src/b"],"dependencies":["a"]}]});
    let text = serde_json::to_string(&base).unwrap();
    let with = |f: &dyn Fn(&mut Value)| {
        let mut d = base.clone();
        f(&mut d);
        serde_json::to_string(&d).unwrap()
    };
    let mut cases = vec![
        String::new(),
        " ".into(),
        text.clone(),
        format!("\u{feff}{text}"),
        text.replace("do it", "do\0it"),
        text.replace("do it", "a\u{2028}b\u{85}c\u{b}d\u{c}e\u{1c}f"),
        text.replace("do it", "\u{1c}"),
        text.replace("do it", "\u{85}"),
        text.replace("do it", "\u{200b}"),
        text.replace(',', ",\r\n"),
        text.replace("\"id\":\"p1\"", "\"id\":\"p1\",\"id\":\"p2\""),
        text.replace("\"id\":\"p1\"", "\"id\":\"p1\",\"extra\":NaN"),
        text.replace("\"do it\"", "Infinity"),
        text.replace("\"id\":\"p1\"", "\"id\":123456789012345678901234567890"),
        nested(200, "1"),
        pad_to(text.clone(), 65_536),
        pad_to(text.clone(), 65_537),
        with(&|d| d["objective"] = json!("o".repeat(4096))),
        with(&|d| d["objective"] = json!("o".repeat(4097))),
        with(&|d| d["primary_identity"] = json!("i".repeat(128))),
        with(&|d| d["primary_identity"] = json!("i".repeat(129))),
        with(&|d| d["primary_identity"] = json!("é".repeat(128))),
        with(&|d| d["objective"] = json!("\u{3000}\u{1f}\t")),
        with(&|d| d["nodes"][0]["role"] = json!("root")),
        with(&|d| d["nodes"][0]["owned_paths"] = json!(["../x"])),
        with(&|d| d["nodes"][0]["owned_paths"] = json!(["/abs"])),
        with(&|d| d["nodes"][0]["owned_paths"] = json!(["a\\b"])),
        with(&|d| d["nodes"][0]["owned_paths"] = json!(["src/b/x"])),
        with(&|d| d["nodes"][1]["dependencies"] = json!(["b"])),
        with(&|d| d["nodes"][0]["dependencies"] = json!(["b"])),
        with(&|d| d["nodes"] = json!([])),
        text.replace("\"a\"", "\"\\ud800\""),
        "null".into(),
        "[]".into(),
    ];
    let many: Vec<Value> = (0..33)
        .map(
            |i| json!({"id": format!("n{i}"), "objective": "x", "owned_paths": [format!("p/{i}")]}),
        )
        .collect();
    cases.push(json!({"id":"p","objective":"o","nodes":many[..32]}).to_string());
    cases.push(json!({"id":"p","objective":"o","nodes":many}).to_string());
    cases.into_iter().map(Case::Text).collect()
}

fn hand_task(kind: &str) -> Vec<Case> {
    let owned = vec!["src".to_owned(), "docs/a.md".to_owned()];
    let sha = "a".repeat(64);
    let replacement = kind == "replacement";
    let row = |path: &str, content: &str| {
        if replacement {
            json!({"path": path, "expected_sha256": sha, "content": content})
        } else {
            json!({"path": path, "content": content})
        }
    };
    let key = if replacement { "replacements" } else { "files" };
    let wrap = |rows: Vec<Value>| json!({"task_id": "t1", key: rows}).to_string();
    let ok = wrap(vec![row("src/a.py", "print(1)")]);
    let mk = |text: String| Case::Task {
        text,
        task_id: "t1".into(),
        owned: owned.clone(),
    };
    let mut cases = vec![
        String::new(),
        " ".into(),
        ok.clone(),
        format!("\u{feff}{ok}"),
        ok.replace("print(1)", "a\0b"),
        ok.replace("print(1)", "a\u{2028}b\u{85}c\u{b}d\u{c}e\u{1c}f\r\n"),
        ok.replace(
            "\"task_id\":\"t1\"",
            "\"task_id\":\"t1\",\"task_id\":\"t2\"",
        ),
        ok.replace("print(1)", "\\ud800"),
        ok.replace("\"print(1)\"", &nested(200, "1")),
        ok.replace("print(1)", "\\ud83d\\ude00"),
        ok.replace("\"print(1)\"", "NaN"),
        pad_to(ok.clone(), REQUEST_LIMIT),
        pad_to(ok.clone(), REQUEST_LIMIT + 1),
        wrap(vec![row("src/a.py", &"x".repeat(49_152))]),
        wrap(vec![row("src/a.py", &"x".repeat(49_153))]),
        wrap(vec![row("src/a.py", &"é".repeat(24_576))]),
        wrap(vec![row("src/a.py", &"é".repeat(24_577))]),
        wrap(vec![row("../x", "c")]),
        wrap(vec![row("src/../x", "c")]),
        wrap(vec![row("/src/a", "c")]),
        wrap(vec![row("src\\a", "c")]),
        wrap(vec![row("src/a\0b", "c")]),
        wrap(vec![row(".git/x", "c")]),
        wrap(vec![row("src//a", "c")]),
        wrap(vec![row("src/./a", "c")]),
        wrap(vec![row("src/", "c")]),
        wrap(vec![row("src", "c")]),
        wrap(vec![row("srcx/a", "c")]),
        wrap(vec![row("docs/a.md", "c")]),
        wrap(vec![row("docs/a.md/b", "c")]),
        wrap(vec![row("src/a", "c"), row("src/a", "d")]),
        wrap(vec![row("src/\u{ff0f}a", "c")]),
        wrap(vec![]),
        wrap((0..16).map(|i| row(&format!("src/{i}"), "c")).collect()),
        wrap((0..17).map(|i| row(&format!("src/{i}"), "c")).collect()),
        "null".into(),
        "[]".into(),
    ];
    if replacement {
        for digest in [
            "A".repeat(64),
            "a".repeat(63),
            format!("{}\n", "a".repeat(64)),
            "\u{ff11}".repeat(64),
        ] {
            cases.push(
                json!({"task_id":"t1","replacements":[
                    {"path":"src/a","expected_sha256":digest,"content":"c"}]})
                .to_string(),
            );
        }
    }
    cases.into_iter().map(mk).collect()
}

fn hand_ollama() -> Vec<Case> {
    let ok = r#"{"models":[{"name":"qwen3:4b","size":123,"digest":"x"}]}"#.to_owned();
    let model = |name: &str| format!("{{\"models\":[{{\"name\":{},\"size\":1}}]}}", json!(name));
    let digest = |v: &str| ok.replace("\"x\"", v).into_bytes();
    let size = |v: &str| ok.replace("123", v).into_bytes();
    let mut cases: Vec<Vec<u8>> = vec![
        vec![],
        b" ".to_vec(),
        b"{}".to_vec(),
        b"null".to_vec(),
        b"{\"models\":[]}".to_vec(),
        ok.clone().into_bytes(),
        [&[0xEF, 0xBB, 0xBF][..], ok.as_bytes()].concat(),
        ok.replace("qwen3:4b", "a\0b").into_bytes(),
        ok.replace("qwen3:4b", "a\u{2028}b").into_bytes(),
        ok.replace(',', ",\r\n").into_bytes(),
        ok.replace("\"size\":123", "\"size\":1,\"size\":2")
            .into_bytes(),
        size("NaN"),
        digest("NaN"),
        digest("Infinity"),
        size("18446744073709551615"),
        size("18446744073709551616"),
        size("123456789012345678901234567890"),
        size("-0"),
        size("-1"),
        size("1.0"),
        size("1e2"),
        size("true"),
        digest(&"9".repeat(5000)),
        digest(&nested(200, "1")),
        digest(&nested(120, "1")),
        digest(&nested(100_000, "1")),
        ok.replace("qwen3:4b", "\\ud800").into_bytes(),
        ok.replace("qwen3:4b", "\\ud83d\\ude00").into_bytes(),
        b"\xff\xfe".to_vec(),
        model(&"a".repeat(200)).into_bytes(),
        model(&"a".repeat(201)).into_bytes(),
        model(&"é".repeat(200)).into_bytes(),
        model(&"é".repeat(201)).into_bytes(),
        model("\u{1c}").into_bytes(),
        model("\u{85}").into_bytes(),
        model("\u{200b}").into_bytes(),
        pad_to(ok.clone(), 65_536).into_bytes(),
        pad_to(ok.clone(), 65_537).into_bytes(),
        ok.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ok.chars()
            .flat_map(|c| u32::from(c).to_be_bytes())
            .collect(),
    ];
    for count in [256, 257] {
        let models: Vec<Value> = (0..count)
            .map(|i| json!({"name": format!("m{i}"), "size": i}))
            .collect();
        cases.push(json!({"models": models}).to_string().into_bytes());
    }
    let mut bad = ok.into_bytes();
    bad[12] = 0xFF;
    cases.push(bad);
    let mut out: Vec<Case> = cases.into_iter().map(Case::Bytes).collect();
    for ch in WS_CHARS {
        out.push(Case::Bytes(model(&ch.to_string()).into_bytes()));
        out.push(Case::Bytes(model(&format!("{ch}x")).into_bytes()));
    }
    out
}

fn hand_texts() -> Vec<Case> {
    let mut out: Vec<String> = Vec::from_iter(EDGE.iter().map(|s| (*s).to_owned()));
    out.extend(
        [
            "a\nb\r\nc\rd\u{b}e\u{c}f\u{1c}g\u{1d}h\u{1e}i\u{85}j\u{2028}k\u{2029}l",
            "a\r\r\nb\n\r",
            "\r\n\r\n",
            "x\u{1f}y",
            "a\0b\0\0c\0",
            "\0",
            "\0\0",
            "a\0 b\0",
            " a\0b ",
            "\ta\0",
            "abc",
            "a-b-c",
            "-a",
            "a_b",
            "A",
            "a\n",
            "\u{ff11}",
        ]
        .map(str::to_owned),
    );
    for len in [63, 64, 65] {
        out.push("a".repeat(len));
    }
    out.extend(WS_CHARS.iter().map(char::to_string));
    out.into_iter().map(Case::Text).collect()
}

// ---------------------------------------------------------------- driver

fn maybe_mutate_bytes(rng: &mut Rng, text: String) -> Case {
    let mut bytes = text.into_bytes();
    if rng.chance(15) {
        bytes = mutate_bytes(rng, &bytes);
    }
    Case::Bytes(bytes)
}

fn gen_cases(name: &str, rng: &mut Rng, count: usize) -> Vec<Case> {
    let mut cases = match name {
        "broker" => hand_broker(),
        "plan" => hand_plan(),
        "replacement" | "edit" => hand_task(name),
        "ollama" => hand_ollama(),
        _ => hand_texts(),
    };
    for _ in 0..count {
        let case = match name {
            "broker" => {
                let doc = broker_doc(rng);
                let text = render(rng, doc);
                maybe_mutate_bytes(rng, text)
            }
            "plan" => {
                let doc = plan_doc(rng);
                Case::Text(render(rng, doc))
            }
            "replacement" | "edit" => {
                let (task_id, owned) = gen_task(rng);
                let doc = if name == "edit" {
                    edit_doc(rng, &owned)
                } else {
                    replacement_doc(rng, &owned)
                };
                Case::Task {
                    text: render(rng, doc),
                    task_id,
                    owned,
                }
            }
            "ollama" => {
                let doc = ollama_doc(rng);
                let text = render(rng, doc);
                maybe_mutate_bytes(rng, text)
            }
            "splitlines" => Case::Text(random_text(rng)),
            "slug" => Case::Text(slug_text(rng)),
            _ => Case::Text(z_text(rng)),
        };
        cases.push(case);
    }
    cases
}

fn run_oracle(python: &std::ffi::OsStr, pair: &Pair) -> Vec<Py> {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = format!("{root}/tests/fixtures/pycorpus/src");
    let mut input = String::new();
    for case in &pair.cases {
        input.push_str(&case.line(pair.name));
        input.push('\n');
    }
    let mut child = Command::new(python)
        .args(["-I", &format!("{root}/tests/differential/oracle.py")])
        .env("HH_PYCORPUS_SRC", &src)
        .env("PYTHONPATH", &src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python oracle");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        stdin.write_all(input.as_bytes()).unwrap();
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(
        output.status.success(),
        "oracle failed for {}: {}",
        pair.name,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let results: Vec<Py> = stdout.lines().map(Py::parse).collect();
    assert_eq!(
        results.len(),
        pair.cases.len(),
        "oracle line count for {}",
        pair.name
    );
    results
}

fn show(case: &Case) -> String {
    let text = format!("{case:?}");
    if text.chars().count() > 600 {
        format!("{}...", text.chars().take(600).collect::<String>())
    } else {
        text
    }
}

#[test]
fn rust_matches_python_oracle() {
    let Some(python) = std::env::var_os("HH_DIFF_PYTHON") else {
        println!(
            "differential: HH_DIFF_PYTHON not set; skipping (set it to a Python 3.14 interpreter)"
        );
        return;
    };
    let count = std::env::var("HH_DIFF_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CASES);
    let seed = std::env::var("HH_DIFF_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SEED);
    let table: [(&str, RustFn); 8] = [
        ("broker", rs_broker),
        ("plan", rs_plan),
        ("replacement", rs_replacement),
        ("edit", rs_edit),
        ("ollama", rs_ollama),
        ("splitlines", rs_splitlines),
        ("slug", rs_slug),
        ("splitz", rs_splitz),
    ];
    let mut failures = Vec::new();
    for (index, (name, rust)) in table.into_iter().enumerate() {
        let mut rng = Rng(seed.wrapping_add(index as u64 * 0x1000_0000_0001));
        let pair = Pair {
            name,
            cases: gen_cases(name, &mut rng, count),
            rust,
        };
        let results = run_oracle(&python, &pair);
        let (mut accept, mut reject) = (0_usize, 0_usize);
        let mut allowed = BTreeMap::<&str, usize>::new();
        for (case, py) in pair.cases.iter().zip(&results) {
            let rs = (pair.rust)(case);
            if rs.0 == py.ok && (!rs.0 || rs.1 == py.value) {
                if rs.0 {
                    accept += 1;
                } else {
                    reject += 1;
                }
            } else if let Some(label) = allow(name, case, &rs, py) {
                *allowed.entry(label).or_default() += 1;
            } else {
                failures.push(format!(
                    "[{name}] seed={seed}\n  input: {}\n  rust: ok={} {}\n  python: ok={} {}",
                    show(case),
                    rs.0,
                    rs.1,
                    py.ok,
                    py.value
                ));
            }
        }
        println!(
            "differential {name}: cases={} both-accept={accept} both-reject={reject} allowlisted={allowed:?}",
            pair.cases.len()
        );
    }
    assert!(
        failures.is_empty(),
        "{} unexplained mismatches:\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
