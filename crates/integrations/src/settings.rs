//! Settings pass-through: raw API fields a spec sets on an outside system,
//! compared with what that system reports back.

use serde_json::Value;

/// Whether the live value already says what the spec wants. Numbers
/// compare by value (APIs report `6` or `6.0` for the same Float), and an
/// object only needs the fields the spec names: APIs add their own (GitHub
/// reports `security_and_analysis.secret_scanning` as `{status, …}`).
pub fn covers(want: &Value, live: &Value) -> bool {
    match (want, live) {
        (Value::Object(w), Value::Object(l)) => w.iter().all(|(k, v)| covers(v, l.get(k).unwrap_or(&Value::Null))),
        (Value::Array(w), Value::Array(l)) => w.len() == l.len() && w.iter().zip(l).all(|(a, b)| covers(a, b)),
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => want == live,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::covers;

    #[test]
    fn numbers_by_value_objects_by_subset_everything_else_exactly() {
        assert!(covers(&json!(6), &json!(6.0)));
        assert!(!covers(&json!(6), &json!(7)));
        assert!(!covers(&json!(6), &json!("6")), "a string is not a number");
        let live = json!({ "secret_scanning": { "status": "enabled" }, "extra": 1 });
        assert!(covers(&json!({ "secret_scanning": { "status": "enabled" } }), &live));
        assert!(!covers(&json!({ "secret_scanning": { "status": "disabled" } }), &live));
        assert!(!covers(&json!({ "missing": false }), &live), "an absent field is null, not false");
        assert!(covers(&json!(null), &json!(null)));
        assert!(!covers(&json!(["a"]), &json!(["a", "b"])), "lists must match exactly");
    }
}
