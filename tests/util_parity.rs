#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! `crate::util` must match Python byte-for-byte; expectations come from
//! `tools/gen_util_fixture.py` run under CPython 3.14.

use hermes_harness::util::{
    canonical_json, iso_utc, json_digest, json_digest_line, ordered_json_spaced, pretty_json,
    pretty_json_ordered, sha256_hex, sorted_json_spaced,
};
use serde::Serialize;
use serde_json::Value;

#[test]
fn borrowed_json_shapes_match_python_fixture() {
    use hermes_harness::util::{
        canonical_json_value, json_digest_value, pretty_json_value, sorted_json_spaced_value,
    };

    let fixture: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/util_parity.json")).unwrap();
    for case in &fixture {
        let input: Value = serde_json::from_str(case["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            canonical_json_value(&input).unwrap(),
            case["compact"].as_str().unwrap()
        );
        assert_eq!(
            sorted_json_spaced_value(&input).unwrap(),
            case["spaced"].as_str().unwrap()
        );
        assert_eq!(
            pretty_json_value(&input).unwrap(),
            case["indent2"].as_str().unwrap()
        );
        assert_eq!(
            json_digest_value(&input).unwrap(),
            case["digest"].as_str().unwrap()
        );
    }
}

#[test]
fn json_shapes_match_python_fixture() {
    let fixture: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/util_parity.json")).unwrap();
    for case in &fixture {
        let input: Value = serde_json::from_str(case["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            canonical_json(&input).unwrap(),
            case["compact"].as_str().unwrap()
        );
        assert_eq!(
            sorted_json_spaced(&input).unwrap(),
            case["spaced"].as_str().unwrap()
        );
        assert_eq!(
            pretty_json(&input).unwrap(),
            case["indent2"].as_str().unwrap()
        );
        // `Value` maps are key-sorted, so the field-order writers must equal the sorted output.
        assert_eq!(
            ordered_json_spaced(&input).unwrap(),
            case["spaced"].as_str().unwrap()
        );
        assert_eq!(
            pretty_json_ordered(&input).unwrap(),
            case["indent2"].as_str().unwrap()
        );
        assert_eq!(
            json_digest(&input).unwrap(),
            case["digest"].as_str().unwrap()
        );
        assert_eq!(
            json_digest_line(&input).unwrap(),
            case["digest_line"].as_str().unwrap()
        );
    }
}

#[test]
fn non_finite_floats_become_null() {
    // serde_json maps NaN/inf to null before our writer runs; Python would emit `NaN`.
    // The harness never serialises non-finite floats, so this only pins the behaviour.
    assert_eq!(canonical_json(&f64::NAN).unwrap(), "null");
    assert_eq!(canonical_json(&f64::INFINITY).unwrap(), "null");
}

#[test]
fn sha256_known_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn iso_utc_formats_like_python() {
    assert_eq!(iso_utc(0, 0), "1970-01-01T00:00:00+00:00");
    assert_eq!(
        iso_utc(1_793_276_096, 123_456),
        "2026-10-29T12:14:56.123456+00:00"
    );
    assert_eq!(iso_utc(951_782_400, 0), "2000-02-29T00:00:00+00:00");
}

#[derive(Serialize)]
struct Unsorted {
    b: u8,
    a: &'static str,
    c: [u8; 2],
    d: std::collections::BTreeMap<String, u8>,
}

#[test]
fn ordered_spaced_matches_python_json_dump() {
    // python: json.dumps({"b": 1, "a": "é\x7f\U0001f600", "c": [1, 2], "d": {}})
    let value = Unsorted {
        b: 1,
        a: "\u{e9}\u{7f}\u{1f600}",
        c: [1, 2],
        d: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        ordered_json_spaced(&value).unwrap(),
        r#"{"b": 1, "a": "\u00e9\u007f\ud83d\ude00", "c": [1, 2], "d": {}}"#
    );
}

#[derive(Serialize)]
struct Floats {
    big: f64,
    tiny: f64,
    tenth: f64,
    neg_zero: f64,
    one: f64,
    denorm: f64,
    max: f64,
    wide: f64,
    widened: f32,
    list: Vec<f64>,
    empty: Vec<u8>,
    inner: Option<Box<Self>>,
}

#[test]
fn ordered_writers_format_floats_like_python() {
    let leaf = || Floats {
        big: 1e16,
        tiny: 1e-7,
        tenth: 0.1,
        neg_zero: -0.0,
        one: 1.0,
        denorm: 5e-324,
        max: 1.797_693_134_862_315_7e308,
        wide: 123_456_789_012_345_680.0,
        widened: 0.1,
        list: vec![1e-4, 1e-5],
        empty: vec![],
        inner: None,
    };
    let mut value = leaf();
    value.inner = Some(Box::new(leaf()));
    // python (float32 0.1 seen as a double): json.dumps / json.dumps(indent=2) of the same dict
    let spaced = ordered_json_spaced(&value).unwrap();
    assert!(spaced.starts_with(
        r#"{"big": 1e+16, "tiny": 1e-07, "tenth": 0.1, "neg_zero": -0.0, "one": 1.0, "denorm": 5e-324, "max": 1.7976931348623157e+308, "wide": 1.2345678901234568e+17, "widened": 0.10000000149011612, "list": [0.0001, 1e-05], "empty": [], "inner": {"big": 1e+16,"#
    ));
    let pretty = pretty_json_ordered(&value).unwrap();
    assert!(pretty.starts_with(
        "{
  \"big\": 1e+16,
  \"tiny\": 1e-07,
  \"tenth\": 0.1,
  \"neg_zero\": -0.0,
  \"one\": 1.0,
  \"denorm\": 5e-324,
  \"max\": 1.7976931348623157e+308,
  \"wide\": 1.2345678901234568e+17,
  \"widened\": 0.10000000149011612,
  \"list\": [
    0.0001,
    1e-05
  ],
  \"empty\": [],
  \"inner\": {
    \"big\": 1e+16,"
    ));
}

#[test]
fn pathlib_and_strip_helpers() {
    use hermes_harness::util::{lexical, py_path_str, py_strip};
    use std::path::Path;
    for (raw, want) in [
        ("./a", "a"),
        ("a//b", "a/b"),
        ("a/", "a"),
        (".", "."),
        ("", "."),
        ("../a/./b/", "../a/b"),
    ] {
        assert_eq!(
            py_path_str(Path::new(raw)).replace('\\', "/"),
            want,
            "{raw}"
        );
    }
    assert_eq!(lexical(Path::new("link/")), Path::new("link"));
    assert_eq!(py_strip("\u{1c} x \u{2003}\u{1f}"), "x");
}
