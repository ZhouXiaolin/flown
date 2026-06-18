pub fn parse_streaming_json(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }

    parse_json_with_repair(raw)
        .or_else(|| parse_partial_json(raw))
        .or_else(|| parse_partial_json(&repair_json(raw)))
        .unwrap_or_else(|| serde_json::json!({}))
}

fn parse_json_with_repair(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str(&repair_json(raw)))
        .ok()
}

fn parse_partial_json(raw: &str) -> Option<serde_json::Value> {
    let completed = complete_partial_json(raw)?;
    parse_json_with_repair(&completed)
}

fn complete_partial_json(raw: &str) -> Option<String> {
    let mut completed = String::with_capacity(raw.len() + 8);
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in raw.chars() {
        completed.push(ch);

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(ch) {
                    return None;
                }
            }
            _ => {}
        }
    }

    if escaped {
        completed.push('\\');
    }
    if in_string {
        completed.push('"');
    }

    trim_trailing_comma(&mut completed);

    while let Some(ch) = stack.pop() {
        completed.push(ch);
    }

    Some(completed)
}

fn trim_trailing_comma(json: &mut String) {
    while matches!(json.chars().last(), Some(ch) if ch.is_whitespace()) {
        json.pop();
    }
    if json.ends_with(',') {
        json.pop();
    }
}

pub fn repair_json(raw: &str) -> String {
    let mut repaired = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;

    while let Some(ch) = chars.next() {
        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            continue;
        }

        match ch {
            '"' => {
                repaired.push(ch);
                in_string = false;
            }
            '\\' => match chars.peek().copied() {
                Some('"') | Some('\\') | Some('/') | Some('b') | Some('f') | Some('n')
                | Some('r') | Some('t') => {
                    repaired.push('\\');
                    repaired.push(chars.next().unwrap());
                }
                Some('u') => {
                    let mut clone = chars.clone();
                    let _ = clone.next();
                    let digits: String = clone.by_ref().take(4).collect();
                    if digits.len() == 4 && digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
                        repaired.push('\\');
                        repaired.push(chars.next().unwrap());
                        for _ in 0..4 {
                            repaired.push(chars.next().unwrap());
                        }
                    } else {
                        repaired.push_str("\\\\");
                    }
                }
                Some(_) | None => repaired.push_str("\\\\"),
            },
            '\u{08}' => repaired.push_str("\\b"),
            '\u{0c}' => repaired.push_str("\\f"),
            '\n' => repaired.push_str("\\n"),
            '\r' => repaired.push_str("\\r"),
            '\t' => repaired.push_str("\\t"),
            control if control.is_control() => {
                repaired.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => repaired.push(other),
        }
    }

    repaired
}

#[cfg(test)]
mod tests {
    use super::parse_streaming_json;

    #[test]
    fn parses_partial_object_for_streaming_tool_args() {
        assert_eq!(
            parse_streaming_json(r#"{"path":"/tmp/a""#),
            serde_json::json!({ "path": "/tmp/a" })
        );
        assert_eq!(
            parse_streaming_json(r#"{"path":"/tmp/a"#),
            serde_json::json!({ "path": "/tmp/a" })
        );
    }
}
