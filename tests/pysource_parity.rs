#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::print_stderr
)]
//! Differential test: the hand-written `pysource` scanner must report the same per-function
//! facts as CPython's `ast` (see `tools/gen_contract_fixture.py`, run under CPython 3.14).
//! Python sources are read from the vendored corpus `tests/fixtures/pycorpus`.

use std::path::Path;

use hermes_harness::gateway::gateway_contract_probe::all_method_hints;
use hermes_harness::util::sha256_hex;
use serde_json::Value;

const TRICKY: &str = include_str!("fixtures/pysource_tricky.py");

fn fixture() -> serde_json::Map<String, Value> {
    match serde_json::from_str(include_str!("fixtures/pysource_parity.json")).unwrap() {
        Value::Object(map) => map,
        other => panic!("unexpected fixture shape: {other}"),
    }
}

/// Scanner output as the fixture's JSON shape, sorted like the generator, or null on error.
fn scanned(source: &str) -> Value {
    all_method_hints(source).map_or(Value::Null, |mut hints| {
        hints.sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
        serde_json::to_value(hints).unwrap()
    })
}

fn compare(name: &str, source: &str, expected: &Value, mismatches: &mut Vec<String>) -> usize {
    let actual = scanned(source);
    if &actual == expected {
        return actual.as_array().map_or(0, Vec::len);
    }
    let (Some(got), Some(want)) = (actual.as_array(), expected.as_array()) else {
        mismatches.push(format!("{name}: parse result differs"));
        return 0;
    };
    if got.len() != want.len() {
        mismatches.push(format!(
            "{name}: {} functions, python has {}",
            got.len(),
            want.len()
        ));
    }
    for (g, w) in got.iter().zip(want) {
        if g != w {
            mismatches.push(format!("{name}: rust {g} != python {w}"));
        }
    }
    0
}

#[test]
fn tricky_corpus_matches_cpython() {
    let expected = &fixture()["pysource_tricky.py"];
    let source = TRICKY.replace("\r\n", "\n");
    let mut mismatches = Vec::new();
    let compared = compare("tricky", &source, &expected["functions"], &mut mismatches);
    compare(
        "tricky-crlf",
        &source.replace('\n', "\r\n"),
        &expected["functions"],
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(compared > 30, "too few tricky functions: {compared}");
}

#[test]
fn upstream_corpus_matches_cpython() {
    // The Python sources are vendored under tests/fixtures/pycorpus, so CI always runs this.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pycorpus");
    let mut mismatches = Vec::new();
    let mut compared = 0;
    let mut files = 0;
    for (key, entry) in &fixture() {
        if key == "pysource_tricky.py" {
            continue;
        }
        let raw = std::fs::read_to_string(root.join(key)).unwrap_or_else(|err| {
            panic!("{key}: {err}; regenerate with tools/gen_contract_fixture.py")
        });
        let source = raw.replace("\r\n", "\n");
        assert_eq!(
            sha256_hex(source.as_bytes()),
            entry["sha256"].as_str().unwrap(),
            "{key} changed since the fixture was generated; rerun tools/gen_contract_fixture.py"
        );
        files += 1;
        compared += compare(key, &source, &entry["functions"], &mut mismatches);
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(
        files > 200 && compared > 1000,
        "corpus too small: {compared} functions in {files} files"
    );
}
