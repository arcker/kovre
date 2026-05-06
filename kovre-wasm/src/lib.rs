//! Data-layer primitives for the kovre dashboard, compiled to WebAssembly.
//!
//! The frontend (SvelteKit) is responsible for rendering and event handling
//! only. Every operation that mutates or projects a data array — sorting,
//! filtering, searching, validating, retention preview — lives here as a
//! Rust function exposed via `wasm-bindgen`.
//!
//! This is the "no JS for data logic" constraint from the Phase 2 DoD:
//! Svelte components call into this WASM module rather than re-implementing
//! the same primitives in TypeScript.
//!
//! ## Module layout
//!
//! Pure Rust functions (`sort_runs`, `compare_values`, …) are testable on
//! the host target via `cargo test -p kovre-wasm`. The `#[wasm_bindgen]`
//! façades are cfg-gated to wasm32 so the native build path does not need
//! a JS runtime to compile.

use std::cmp::Ordering;

use serde_json::Value;

#[cfg(target_family = "wasm")]
use wasm_bindgen::prelude::*;

/// Sort a slice of JSON objects by `key`, in `direction` (`"asc"` /
/// `"desc"`). Stable order; mixed-type values fall back to "equal" so
/// the sort never panics on inconsistent data.
///
/// `null` values cluster at the start in ascending order and at the end
/// in descending order, regardless of the type of the present values.
/// Missing keys are treated the same as `null`.
pub fn sort_runs(runs: &mut [Value], key: &str, direction: &str) {
    let asc = direction != "desc";
    runs.sort_by(|a, b| {
        let ord = compare_values(a.get(key), b.get(key));
        if asc { ord } else { ord.reverse() }
    });
}

/// Three-way compare two `serde_json::Value` references with
/// null-aware ordering.
///
/// Order rules:
///   - `None` (missing key) is equivalent to `Some(Null)` and sorts
///     before any concrete value in ascending direction.
///   - `Number`/`String`/`Bool` use their natural ordering.
///   - Cross-type comparisons (e.g. number vs string) return `Equal` so
///     the sort remains total but does not panic; this should never
///     happen against Lithair-shaped data because each field has a
///     consistent JSON type, but we stay defensive against external
///     callers.
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    let a = a.unwrap_or(&Value::Null);
    let b = b.unwrap_or(&Value::Null);
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// JS-facing entry point.
///
/// Accepts a JS array of objects (typically the parsed `data` array from
/// `GET /api/job_runs`), sorts in place, and returns the sorted array.
/// Mirrors the host-side `sort_runs` so the contract is identical
/// modulo JSON ↔ JsValue marshaling.
#[cfg(target_family = "wasm")]
#[wasm_bindgen(js_name = sortRunsBy)]
pub fn sort_runs_by(runs: JsValue, key: &str, direction: &str) -> Result<JsValue, JsValue> {
    let mut values: Vec<Value> = serde_wasm_bindgen::from_value(runs)
        .map_err(|e| JsValue::from_str(&format!("kovre-wasm: deserialize: {e}")))?;
    sort_runs(&mut values, key, direction);
    serde_wasm_bindgen::to_value(&values)
        .map_err(|e| JsValue::from_str(&format!("kovre-wasm: serialize: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn samples() -> Vec<Value> {
        vec![
            json!({"id": "c", "started_at": "2026-05-03T10:00:00Z", "bytes_processed": 30}),
            json!({"id": "a", "started_at": "2026-05-01T10:00:00Z", "bytes_processed": 10}),
            json!({"id": "b", "started_at": "2026-05-02T10:00:00Z", "bytes_processed": null}),
        ]
    }

    fn ids(runs: &[Value]) -> Vec<&str> {
        runs.iter().map(|r| r["id"].as_str().unwrap()).collect()
    }

    #[test]
    fn sorts_strings_ascending_by_default() {
        let mut runs = samples();
        sort_runs(&mut runs, "started_at", "asc");
        assert_eq!(ids(&runs), vec!["a", "b", "c"]);
    }

    #[test]
    fn sorts_strings_descending() {
        let mut runs = samples();
        sort_runs(&mut runs, "started_at", "desc");
        assert_eq!(ids(&runs), vec!["c", "b", "a"]);
    }

    #[test]
    fn sorts_numbers_ascending_with_nulls_first() {
        let mut runs = samples();
        sort_runs(&mut runs, "bytes_processed", "asc");
        // null on `b` clusters first; then 10, 30.
        assert_eq!(ids(&runs), vec!["b", "a", "c"]);
    }

    #[test]
    fn sorts_numbers_descending_with_nulls_last() {
        let mut runs = samples();
        sort_runs(&mut runs, "bytes_processed", "desc");
        assert_eq!(ids(&runs), vec!["c", "a", "b"]);
    }

    #[test]
    fn unknown_direction_falls_back_to_ascending() {
        // Anything that isn't exactly "desc" is treated as ascending; we
        // intentionally don't error — the JS caller might pass through a
        // user input directly and we'd rather sort than throw.
        let mut runs = samples();
        sort_runs(&mut runs, "id", "garbage");
        assert_eq!(ids(&runs), vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_key_treats_all_entries_as_equal() {
        // Sorting on a non-existent field must keep the array stable
        // (no panic, no reorder beyond what `sort_by` allows on Equal).
        let mut runs = samples();
        let original_ids = ids(&runs).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        sort_runs(&mut runs, "no_such_field", "asc");
        let after_ids = ids(&runs).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(after_ids, original_ids);
    }

    #[test]
    fn empty_input_is_left_alone() {
        let mut runs: Vec<Value> = Vec::new();
        sort_runs(&mut runs, "id", "asc");
        assert!(runs.is_empty());
    }

    #[test]
    fn cross_type_values_compare_as_equal() {
        // Defensive against malformed inputs: a string vs a number
        // should never panic the sort.
        let mut runs = vec![json!({"x": "foo"}), json!({"x": 42})];
        sort_runs(&mut runs, "x", "asc");
        // Order is unspecified for cross-type; we only verify no panic.
        assert_eq!(runs.len(), 2);
    }
}
