//! Tool-argument validation and JSON-Schema-aware coercion.
//!
//! Mirrors pi-ai's `validateToolArguments`: in addition to required-field and
//! default-value checks, the input is coerced to match the schema types when
//! possible (string ↔ number ↔ boolean), so common LLM outputs like
//! `"count": "3"` or `"verbose": "true"` pass validation instead of being
//! rejected.
//!
//! Union schemas (`anyOf` / `oneOf`) are tried in order; the first branch
//! that successfully coerces and validates is kept. `allOf` branches are
//! applied cumulatively.
use crate::error::{AiError, Result};
use serde_json::Value;

/// Validate `arguments` against `parameters` (a JSON Schema object).
///
/// On success, returns the (possibly coerced and default-filled) arguments.
/// On failure, returns [`AiError::Validation`] describing the first schema
/// violation encountered.
pub fn validate_tool_arguments(parameters: &Value, arguments: &Value) -> Result<Value> {
    let mut args = arguments.clone();

    if let Value::Object(schema) = parameters {
        coerce(&mut args, schema);
    }

    check_required(parameters, &args)?;
    apply_defaults(parameters, &mut args);

    Ok(args)
}

/// Find `tool_call.name` in `tools` and validate its arguments against the
/// tool's parameters. Mirrors pi-ai's `validateToolCall(tools, toolCall)`.
///
/// Returns the coerced argument value on success, or [`AiError::Validation`]
/// if the tool is unknown or validation fails.
pub fn validate_tool_call(
    tools: &[crate::types::Tool],
    tool_call: &crate::types::ToolCall,
) -> Result<Value> {
    let tool = tools
        .iter()
        .find(|t| t.name == tool_call.name)
        .ok_or_else(|| AiError::Validation(format!("Tool \"{}\" not found", tool_call.name)))?;
    validate_tool_arguments(&tool.parameters, &tool_call.arguments)
}

fn check_required(parameters: &Value, args: &Value) -> Result<()> {
    let schema = match parameters {
        Value::Object(map) => map,
        _ => return Ok(()),
    };
    let Some(Value::Array(required)) = schema.get("required") else {
        return Ok(());
    };
    let obj = match args {
        Value::Object(map) => map,
        _ => return Err(AiError::Validation("Arguments must be a JSON object".to_string())),
    };
    for field in required {
        if let Value::String(name) = field {
            if !obj.contains_key(name) {
                return Err(AiError::Validation(format!(
                    "Missing required field: {}",
                    name
                )));
            }
        }
    }
    Ok(())
}

fn apply_defaults(parameters: &Value, args: &mut Value) {
    let Some(schema) = parameters.as_object() else {
        return;
    };
    let Some(Value::Object(properties)) = schema.get("properties") else {
        return;
    };
    let Value::Object(obj) = args else { return };
    for (key, prop_schema) in properties {
        if let Value::Object(prop_map) = prop_schema {
            if let Some(default) = prop_map.get("default") {
                obj.entry(key.clone()).or_insert_with(|| default.clone());
            }
        }
    }
}

/// Best-effort, schema-driven coercion. Mutates `value` in place.
fn coerce(value: &mut Value, schema: &serde_json::Map<String, Value>) {
    if let Some(Value::Array(all_of)) = schema.get("allOf") {
        for nested in all_of {
            if let Value::Object(nested) = nested {
                coerce(value, nested);
            }
        }
    }

    if let Some(Value::Array(branches)) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
    {
        if let Some(coerced) = try_union_branches(value, branches) {
            *value = coerced;
            return;
        }
    }

    let schema_types = get_schema_types(schema);
    let already_matches =
        !schema_types.is_empty() && schema_types.iter().any(|t| matches_type(value, t));

    if !already_matches {
        for ty in &schema_types {
            if let Some(coerced) = coerce_primitive(value, ty) {
                *value = coerced;
                break;
            }
        }
    }

    if schema_types.iter().any(|t| t == "object") {
        if let Value::Object(obj) = value {
            coerce_object(obj, schema);
        }
    }

    if schema_types.iter().any(|t| t == "array") {
        if let Value::Array(arr) = value {
            coerce_array(arr, schema);
        }
    }
}

fn get_schema_types(schema: &serde_json::Map<String, Value>) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

fn matches_type(value: &Value, ty: &str) -> bool {
    match ty {
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn coerce_primitive(value: &Value, ty: &str) -> Option<Value> {
    match ty {
        "number" => match value {
            Value::Null => Some(Value::Number(serde_json::Number::from(0))),
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return None;
                }
                if let Ok(i) = trimmed.parse::<i64>() {
                    return Some(Value::Number(serde_json::Number::from(i)));
                }
                trimmed.parse::<f64>().ok().and_then(|n| {
                    if n.is_finite() {
                        serde_json::Number::from_f64(n).map(Value::Number)
                    } else {
                        None
                    }
                })
            }
            Value::Bool(b) => Some(Value::Number(serde_json::Number::from(if *b { 1 } else { 0 }))),
            _ => None,
        },
        "integer" => match value {
            Value::Null => Some(Value::Number(serde_json::Number::from(0))),
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return None;
                }
                trimmed
                    .parse::<i64>()
                    .ok()
                    .map(|n| Value::Number(serde_json::Number::from(n)))
            }
            Value::Bool(b) => Some(Value::Number(serde_json::Number::from(if *b { 1 } else { 0 }))),
            _ => None,
        },
        "boolean" => match value {
            Value::Null => Some(Value::Bool(false)),
            Value::String(s) => match s.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            Value::Number(n) => {
                if n.as_i64() == Some(1) {
                    Some(Value::Bool(true))
                } else if n.as_i64() == Some(0) {
                    Some(Value::Bool(false))
                } else {
                    None
                }
            }
            _ => None,
        },
        "string" => match value {
            Value::Null => Some(Value::String(String::new())),
            Value::Number(n) => Some(Value::String(n.to_string())),
            Value::Bool(b) => Some(Value::String(b.to_string())),
            _ => None,
        },
        "null" => match value {
            Value::String(s) if s.is_empty() => Some(Value::Null),
            Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::Null),
            Value::Bool(false) => Some(Value::Null),
            _ => None,
        },
        _ => None,
    }
}

fn coerce_object(
    obj: &mut serde_json::Map<String, Value>,
    schema: &serde_json::Map<String, Value>,
) {
    if let Some(Value::Object(properties)) = schema.get("properties") {
        for (key, prop_schema) in properties {
            if let Some(Value::Object(prop_map)) = Some(prop_schema) {
                if let Some(entry) = obj.get_mut(key) {
                    coerce(entry, prop_map);
                }
            }
        }
    }
    if let Some(Value::Object(additional)) = schema.get("additionalProperties") {
        let defined: std::collections::HashSet<String> = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        let keys: Vec<String> = obj.keys().cloned().collect();
        for key in keys {
            if defined.contains(&key) {
                continue;
            }
            if let Some(entry) = obj.get_mut(&key) {
                coerce(entry, additional);
            }
        }
    }
}

fn coerce_array(arr: &mut Vec<Value>, schema: &serde_json::Map<String, Value>) {
    match schema.get("items") {
        Some(Value::Array(per_index)) => {
            for (i, item) in arr.iter_mut().enumerate() {
                if let Some(Value::Object(item_schema)) = per_index.get(i) {
                    coerce(item, item_schema);
                }
            }
        }
        Some(Value::Object(item_schema)) => {
            for item in arr.iter_mut() {
                coerce(item, item_schema);
            }
        }
        _ => {}
    }
}

fn try_union_branches(value: &Value, branches: &[Value]) -> Option<Value> {
    for branch in branches {
        let Value::Object(branch_schema) = branch else {
            continue;
        };
        let mut candidate = value.clone();
        coerce(&mut candidate, branch_schema);
        if matches_branch(&candidate, branch_schema) {
            return Some(candidate);
        }
    }
    None
}

/// Lightweight branch validation: only checks `type` compatibility and, for
/// objects, the `required` field list. Good enough to disambiguate union
/// branches without pulling in a full JSON Schema validator.
fn matches_branch(value: &Value, branch: &serde_json::Map<String, Value>) -> bool {
    let types = get_schema_types(branch);
    if !types.is_empty() && !types.iter().any(|t| matches_type(value, t)) {
        return false;
    }
    if let (Some(Value::Array(required)), Value::Object(obj)) = (branch.get("required"), value) {
        for req in required {
            if let Value::String(name) = req {
                if !obj.contains_key(name) {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_fields_are_enforced() {
        let params = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "number"}
            },
            "required": ["name"]
        });
        assert!(validate_tool_arguments(&params, &json!({"age": 30})).is_err());
        assert!(validate_tool_arguments(&params, &json!({"name": "Alice"})).is_ok());
    }

    #[test]
    fn defaults_are_applied() {
        let params = json!({
            "type": "object",
            "properties": {
                "verbose": {"type": "boolean", "default": false}
            }
        });
        let args = json!({});
        let result = validate_tool_arguments(&params, &args).unwrap();
        assert_eq!(result["verbose"], false);
    }

    #[test]
    fn string_is_coerced_to_number() {
        let params = json!({
            "type": "object",
            "properties": {"count": {"type": "number"}}
        });
        let args = json!({"count": "42"});
        let result = validate_tool_arguments(&params, &args).unwrap();
        assert_eq!(result["count"], 42);
    }

    #[test]
    fn string_is_coerced_to_boolean() {
        let params = json!({
            "type": "object",
            "properties": {"verbose": {"type": "boolean"}}
        });
        let result = validate_tool_arguments(&params, &json!({"verbose": "true"})).unwrap();
        assert_eq!(result["verbose"], true);
    }

    #[test]
    fn union_anyof_picks_matching_branch() {
        let params = json!({
            "type": "object",
            "properties": {
                "target": {
                    "anyOf": [
                        {"type": "number"},
                        {"type": "string"}
                    ]
                }
            }
        });
        let args = json!({"target": "42"});
        let result = validate_tool_arguments(&params, &args).unwrap();
        // Both branches accept "42"; the number branch coerces first.
        assert_eq!(result["target"], 42);
    }

    #[test]
    fn nested_object_coercion_applies() {
        let params = json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": {
                        "count": {"type": "number"}
                    }
                }
            }
        });
        let args = json!({"opts": {"count": "7"}});
        let result = validate_tool_arguments(&params, &args).unwrap();
        assert_eq!(result["opts"]["count"], 7);
    }

    #[test]
    fn array_items_are_coerced() {
        let params = json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": {"type": "number"}
                }
            }
        });
        let args = json!({"ids": ["1", "2", "3"]});
        let result = validate_tool_arguments(&params, &args).unwrap();
        assert_eq!(result["ids"], json!([1, 2, 3]));
    }

    #[test]
    fn no_schema_passes_through() {
        let params = json!({});
        let args = json!({"anything": "goes"});
        assert!(validate_tool_arguments(&params, &args).is_ok());
    }

    #[test]
    fn validate_tool_call_unknown_tool_is_error() {
        use crate::types::{Tool, ToolCall};
        let tools = vec![Tool {
            name: "known".to_string(),
            description: "".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let call = ToolCall {
            content_type: "toolCall".to_string(),
            id: "1".to_string(),
            name: "unknown".to_string(),
            arguments: json!({}),
            thought_signature: None,
        };
        assert!(validate_tool_call(&tools, &call).is_err());
    }

    #[test]
    fn validate_tool_call_delegates_to_schema() {
        use crate::types::{Tool, ToolCall};
        let tools = vec![Tool {
            name: "lookup".to_string(),
            description: "".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"count": {"type": "number"}},
                "required": ["count"],
            }),
        }];
        let call = ToolCall {
            content_type: "toolCall".to_string(),
            id: "1".to_string(),
            name: "lookup".to_string(),
            arguments: json!({"count": "7"}),
            thought_signature: None,
        };
        let result = validate_tool_call(&tools, &call).unwrap();
        assert_eq!(result["count"], 7);
    }
}
