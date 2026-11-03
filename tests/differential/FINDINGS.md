# Differential findings (Rust vs vendored Python)

Each open finding is allowlisted in `tests/differential.rs` (`allow`) under the same number.
Finding 6 was fixed in `src/`. Python 3.14.7, seed default.

1. **broker `_decode_request` / ollama `parse_ollama_tags`: bytes encoding autodetect.**
   Input: UTF-8 BOM + valid JSON, or the document encoded UTF-16LE/BE or UTF-32.
   Rust: reject (serde_json::from_slice). Python: accept (`json.loads(bytes)` runs `detect_encoding`).
   Rust stricter; broker frames are specified as UTF-8 JSON. Documented in DIVERGENCES.md.
2. **Lone surrogates (all JSON parsers).**
   Input: `"\ud800"` escape in any string, or (bytes APIs) raw bytes `ED A0 80`.
   Rust: reject. Python: accept (`json.loads` keeps lone surrogate escapes; bytes path decodes with `surrogatepass`).
   Rust stricter and safer (surrogates cannot be encoded to UTF-8 later). Documented in DIVERGENCES.md.
3. **ollama: `NaN` / `Infinity` literal in an ignored field**, e.g. `{"models":[],"x":NaN}`.
   Rust: reject. Python: accept. Rust stricter (RFC 8259). Documented in DIVERGENCES.md.
4. **ollama: nesting depth >= 128 in an ignored field**, e.g. `"digest": [[[...]]]` x200.
   Rust: reject (serde_json recursion limit 128). Python: accept (limit ~1000). Rust stricter; a real Ollama listing never nests that deep. Documented in DIVERGENCES.md.
5. **ollama: integer range.** `size` 18446744073709551616 (> u64::MAX): Rust reject, Python accept. Conversely an integer literal over 4300 digits (even in an ignored field): Python reject (int-digit limit), Rust accept. The u64 case is documented in DIVERGENCES.md; the 4300-digit case is a Python limit.
6. **git `split_z_paths` kept empty entries.** Input `"a\0\0b\0"`. Rust returned `["a","","b"]`; Python (`worktrees.py:114`, `if name`) returns `["a","b"]`. Fixed; see DIVERGENCES.md.
7. **ollama: `"size": -0`.** Rust: reject (serde parses `-0` as float, `as_u64` is None). Python: accept (int 0). Rust stricter. Documented in DIVERGENCES.md.
