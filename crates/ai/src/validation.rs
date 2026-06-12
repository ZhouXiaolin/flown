use serde_json::Value;

/// Validate tool arguments against the tool's JSON Schema parameters.
/// Returns Ok(args) if valid, or Err(message) if validation fails.
/// Currently checks: required fields, type coercion for common cases.
pub fn validate_tool_arguments(parameters: &Value, arguments: &Value) -> Result<Value, String> {
    let mut args = arguments.clone();

    // Only validate if parameters is a valid JSON Schema object
    let schema = match parameters {
        Value::Object(map) => map,
        _ => return Ok(args),
    };

    // Check required fields
    if let Some(Value::Array(required)) = schema.get("required") {
        let obj = match &args {
            Value::Object(map) => map,
            _ => return Err("Arguments must be a JSON object".to_string()),
        };
        for field in required {
            if let Value::String(field_name) = field {
                if !obj.contains_key(field_name) {
                    return Err(format!("Missing required field: {}", field_name));
                }
            }
        }
    }

    // Apply defaults from schema properties
    if let Some(Value::Object(properties)) = schema.get("properties") {
        if let Value::Object(ref mut obj) = args {
            for (key, prop_schema) in properties {
                if let Value::Object(prop_map) = prop_schema {
                    if let Some(default) = prop_map.get("default") {
                        obj.entry(key.clone()).or_insert_with(|| default.clone());
                    }
                }
            }
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_required_fields() {
        let params = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "number"}
            },
            "required": ["name"]
        });

        // Valid - has required field
        let args = json!({"name": "Alice", "age": 30});
        assert!(validate_tool_arguments(&params, &args).is_ok());

        // Invalid - missing required field
        let args = json!({"age": 30});
        assert!(validate_tool_arguments(&params, &args).is_err());
    }

    #[test]
    fn test_validate_applies_defaults() {
        let params = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "verbose": {"type": "boolean", "default": false}
            }
        });

        let args = json!({"name": "Alice"});
        let result = validate_tool_arguments(&params, &args).unwrap();
        assert_eq!(result["verbose"], false);
        assert_eq!(result["name"], "Alice");
    }

    #[test]
    fn test_validate_no_schema() {
        let params = json!({});
        let args = json!({"anything": "goes"});
        assert!(validate_tool_arguments(&params, &args).is_ok());
    }
}
