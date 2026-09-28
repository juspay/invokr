use std::collections::HashMap;

pub fn resolve(
    value: &serde_json::Value,
    input: &HashMap<String, serde_json::Value>,
    config: &HashMap<String, serde_json::Value>,
    secrets: &HashMap<String, String>,
    execution: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::String(s) => resolve_string(s, input, config, secrets, execution),
        serde_json::Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (k, v) in map {
                let resolved_key = match resolve_string(k, input, config, secrets, execution)? {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                result.insert(resolved_key, resolve(v, input, config, secrets, execution)?);
            }
            Ok(serde_json::Value::Object(result))
        }
        serde_json::Value::Array(arr) => {
            let resolved: Result<Vec<_>, _> = arr
                .iter()
                .map(|v| resolve(v, input, config, secrets, execution))
                .collect();
            Ok(serde_json::Value::Array(resolved?))
        }
        other => Ok(other.clone()),
    }
}

fn resolve_string(
    s: &str,
    input: &HashMap<String, serde_json::Value>,
    config: &HashMap<String, serde_json::Value>,
    secrets: &HashMap<String, String>,
    execution: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    // Check if the entire string is a single template variable
    if let Some(var) = is_single_template(s) {
        return resolve_variable(var, input, config, secrets, execution);
    }

    // Otherwise do string interpolation
    let mut result = s.to_string();
    let mut start = 0;
    while let Some(open) = result[start..].find("{{") {
        let open = start + open;
        if let Some(close) = result[open..].find("}}") {
            let close = open + close + 2;
            let var = result[open + 2..close - 2].trim();
            let resolved = resolve_variable(var, input, config, secrets, execution)?;
            let replacement = match &resolved {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result.replace_range(open..close, &replacement);
            start = open + replacement.len();
        } else {
            break;
        }
    }
    Ok(serde_json::Value::String(result))
}

fn is_single_template(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        let inner = trimmed[2..trimmed.len() - 2].trim();
        // Make sure there's no other {{ in the inner part
        if !inner.contains("{{") {
            return Some(inner);
        }
    }
    None
}

fn resolve_variable(
    var: &str,
    input: &HashMap<String, serde_json::Value>,
    config: &HashMap<String, serde_json::Value>,
    secrets: &HashMap<String, String>,
    execution: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    if let Some(key) = var.strip_prefix("input.") {
        input
            .get(key)
            .cloned()
            .ok_or_else(|| format!("Unresolved template variable: {}", var))
    } else if let Some(key) = var.strip_prefix("config.") {
        config
            .get(key)
            .cloned()
            .ok_or_else(|| format!("Unresolved template variable: {}", var))
    } else if let Some(key) = var.strip_prefix("secret.") {
        secrets
            .get(key)
            .map(|v| serde_json::Value::String(v.clone()))
            .ok_or_else(|| format!("Unresolved template variable: {}", var))
    } else if let Some(key) = var.strip_prefix("execution.") {
        execution
            .get(key)
            .cloned()
            .ok_or_else(|| format!("Unresolved template variable: {}", var))
    } else {
        Err(format!("Unknown template namespace in: {}", var))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_resolution() {
        let mut input = HashMap::new();
        input.insert("user_id".into(), serde_json::json!("u_abc"));

        let mut config = HashMap::new();
        config.insert(
            "api_base_url".into(),
            serde_json::json!("https://api.example.com"),
        );

        let mut secrets = HashMap::new();
        secrets.insert("api_key".into(), "sk-123".into());

        let execution = HashMap::new();

        let template = serde_json::json!({
            "url": "{{config.api_base_url}}/users/{{input.user_id}}",
            "headers": {
                "Authorization": "Bearer {{secret.api_key}}"
            },
            "body": {
                "user_id": "{{input.user_id}}"
            }
        });

        let result = resolve(&template, &input, &config, &secrets, &execution).unwrap();
        assert_eq!(
            result["url"],
            serde_json::json!("https://api.example.com/users/u_abc")
        );
        assert_eq!(
            result["headers"]["Authorization"],
            serde_json::json!("Bearer sk-123")
        );
        assert_eq!(result["body"]["user_id"], serde_json::json!("u_abc"));
    }

    #[test]
    fn test_execution_idempotency_key() {
        let input = HashMap::new();
        let config = HashMap::new();
        let secrets = HashMap::new();

        let mut execution = HashMap::new();
        execution.insert(
            "idempotency_key".into(),
            serde_json::json!("my-idem-key-42"),
        );

        let template = serde_json::json!({
            "headers": {
                "Idempotency-Key": "{{execution.idempotency_key}}"
            }
        });

        let result = resolve(&template, &input, &config, &secrets, &execution).unwrap();
        assert_eq!(
            result["headers"]["Idempotency-Key"],
            serde_json::json!("my-idem-key-42")
        );
    }

    #[test]
    fn test_execution_interpolation() {
        let input = HashMap::new();
        let config = HashMap::new();
        let secrets = HashMap::new();

        let mut execution = HashMap::new();
        execution.insert("idempotency_key".into(), serde_json::json!("abc"));

        let template = serde_json::json!("key={{execution.idempotency_key}}-suffix");

        let result = resolve(&template, &input, &config, &secrets, &execution).unwrap();
        assert_eq!(result, serde_json::json!("key=abc-suffix"));
    }

    #[test]
    fn test_execution_missing_key() {
        let input = HashMap::new();
        let config = HashMap::new();
        let secrets = HashMap::new();
        let execution = HashMap::new();

        let template = serde_json::json!("{{execution.missing}}");
        assert!(resolve(&template, &input, &config, &secrets, &execution).is_err());
    }

    #[test]
    fn test_missing_variable() {
        let input = HashMap::new();
        let config = HashMap::new();
        let secrets = HashMap::new();
        let execution = HashMap::new();

        let template = serde_json::json!("{{input.missing}}");
        assert!(resolve(&template, &input, &config, &secrets, &execution).is_err());
    }

    #[test]
    fn callback_url_template_resolves() {
        let mut execution = std::collections::HashMap::new();
        execution.insert("execution_id".into(), serde_json::json!("exec_abc"));
        execution.insert("org_id".into(), serde_json::json!("org_1"));
        execution.insert("workspace_id".into(), serde_json::json!("ws_1"));
        execution.insert(
            "callback_url_success".into(),
            serde_json::json!(
                "https://invokr.example/v1/callbacks/org_1/ws_1/executions/exec_abc/complete"
            ),
        );
        let template = serde_json::json!({
            "on_success": "{{execution.callback_url_success}}",
            "org": "{{execution.org_id}}",
        });
        let out = resolve(
            &template,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &execution,
        )
        .unwrap();
        assert_eq!(
            out["on_success"].as_str().unwrap(),
            "https://invokr.example/v1/callbacks/org_1/ws_1/executions/exec_abc/complete"
        );
        assert_eq!(out["org"].as_str().unwrap(), "org_1");
    }
}
