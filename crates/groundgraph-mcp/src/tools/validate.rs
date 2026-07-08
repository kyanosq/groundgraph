//! Minimal JSON-Schema-subset validator for `tools/call` arguments (#89).
//!
//! Every tool descriptor advertises an `inputSchema` with
//! `additionalProperties: false`, `required`, and per-property `type` /
//! `enum` / `minimum` / `items`. Before this module the dispatcher trusted
//! the client to honour that contract: a wrong-typed or undeclared field was
//! silently ignored (`as_bool()` → `None` → default), so
//! `additionalProperties: false` was a lie and `required` was never enforced.
//!
//! This validator checks `arguments` against the exact JSON-Schema subset our
//! descriptors actually use, so a malformed request is rejected with
//! `-32602 Invalid params` instead of running with silently-dropped fields.
//! Unknown keywords (`default`, `description`) are ignored on purpose.

use serde_json::Value;

/// Validate a `tools/call.arguments` object against a tool's `inputSchema`.
/// Returns the first violation as a human-readable message.
pub(crate) fn validate_arguments(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(obj) = args.as_object() else {
        return Err("`arguments` must be a JSON object".to_string());
    };
    let props = schema.get("properties").and_then(|p| p.as_object());

    // `additionalProperties: false` → every supplied key must be declared.
    let reject_extra = schema.get("additionalProperties") == Some(&Value::Bool(false));
    if reject_extra {
        for key in obj.keys() {
            let declared = props.is_some_and(|p| p.contains_key(key));
            if !declared {
                return Err(format!(
                    "unknown argument `{key}`; this tool declares additionalProperties:false"
                ));
            }
        }
    }

    // `required` → each named key must be present.
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for req in required {
            if let Some(name) = req.as_str() {
                if !obj.contains_key(name) {
                    return Err(format!("missing required argument `{name}`"));
                }
            }
        }
    }

    // Validate every supplied value against its property schema.
    if let Some(props) = props {
        for (key, value) in obj {
            if let Some(prop_schema) = props.get(key) {
                validate_value(prop_schema, value, key)?;
            }
        }
    }
    Ok(())
}

/// Validate one value against a (sub-)schema: `type`, `enum`, `minimum`, and
/// `items` for arrays. `path` names the offending field for error messages.
fn validate_value(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(ty) = schema.get("type").and_then(|t| t.as_str()) {
        if !type_matches(ty, value) {
            return Err(format!(
                "argument `{path}` must be of type {ty}, got {}",
                value_type_name(value)
            ));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
        if !allowed.iter().any(|a| a == value) {
            let choices: Vec<String> = allowed.iter().map(value_to_choice).collect();
            return Err(format!(
                "argument `{path}` must be one of [{}], got {value}",
                choices.join(", ")
            ));
        }
    }
    if let Some(min) = schema.get("minimum").and_then(|m| m.as_f64()) {
        if let Some(n) = value.as_f64() {
            if n < min {
                return Err(format!("argument `{path}` must be >= {min}, got {n}"));
            }
        }
    }
    if let Some(items) = schema.get("items") {
        if let Some(arr) = value.as_array() {
            for (i, elem) in arr.iter().enumerate() {
                validate_value(items, elem, &format!("{path}[{i}]"))?;
            }
        }
    }
    Ok(())
}

/// Does `value` satisfy JSON-Schema `type`? `integer` accepts any JSON number
/// with no fractional part (`2` and the rarely-seen `2.0`); `number` accepts
/// any numeric.
fn type_matches(ty: &str, value: &Value) -> bool {
    match ty {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // An unrecognised `type` keyword constrains nothing rather than
        // rejecting everything — forward-compatible with future schemas.
        _ => true,
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_to_choice(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A schema mirroring the real descriptor shape: typed properties, an
    /// enum, a minimum, a string-array, `required` and
    /// `additionalProperties:false`.
    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string" },
                "depth": { "type": "integer", "minimum": 0 },
                "flag": { "type": "boolean" },
                "level": { "type": "string", "enum": ["low", "medium", "high"] },
                "kinds": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["node_id"],
            "additionalProperties": false
        })
    }

    #[test]
    fn accepts_a_well_formed_object() {
        let args = json!({
            "node_id": "dart::a#b",
            "depth": 2,
            "flag": true,
            "level": "high",
            "kinds": ["dart_method", "test_case"]
        });
        assert_eq!(validate_arguments(&schema(), &args), Ok(()));
    }

    #[test]
    fn rejects_an_undeclared_property() {
        let args = json!({ "node_id": "x", "bogus": 1 });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(err.contains("bogus"), "must name the offending key: {err}");
    }

    #[test]
    fn rejects_a_missing_required_field() {
        let args = json!({ "depth": 1 });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(
            err.contains("node_id"),
            "must name the missing field: {err}"
        );
    }

    #[test]
    fn rejects_a_wrong_typed_field() {
        // `depth` is an integer; a string must be refused, not coerced.
        let args = json!({ "node_id": "x", "depth": "abc" });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(err.contains("depth") && err.contains("integer"), "{err}");
    }

    #[test]
    fn rejects_a_boolean_given_as_string() {
        let args = json!({ "node_id": "x", "flag": "yes" });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(err.contains("flag") && err.contains("boolean"), "{err}");
    }

    #[test]
    fn rejects_an_enum_violation() {
        let args = json!({ "node_id": "x", "level": "extreme" });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(err.contains("level"), "must name the enum field: {err}");
    }

    #[test]
    fn rejects_a_minimum_violation() {
        let args = json!({ "node_id": "x", "depth": -1 });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(
            err.contains("depth"),
            "must name the field below minimum: {err}"
        );
    }

    #[test]
    fn rejects_a_non_string_array_element() {
        let args = json!({ "node_id": "x", "kinds": ["ok", 7] });
        let err = validate_arguments(&schema(), &args).unwrap_err();
        assert!(
            err.contains("kinds[1]"),
            "must point at the bad element: {err}"
        );
    }

    #[test]
    fn rejects_non_object_arguments() {
        let err = validate_arguments(&schema(), &json!("not an object")).unwrap_err();
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn empty_object_passes_when_nothing_is_required() {
        let s = json!({
            "type": "object",
            "properties": { "repo_root": { "type": "string" } },
            "required": [],
            "additionalProperties": false
        });
        assert_eq!(validate_arguments(&s, &json!({})), Ok(()));
    }
}
