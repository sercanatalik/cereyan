//! Parameter validation against the flow's JSON schema subset produced by the
//! Python authoring layer. Engines coerce fully; the server rejects obvious
//! mismatches with a 422 naming the parameter.

use serde_json::{Map, Value};

pub fn validate_parameters(schema: &Value, params: &Map<String, Value>) -> Result<(), String> {
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for r in required {
            if let Some(name) = r.as_str() {
                if !params.contains_key(name) {
                    return Err(format!("parameter {name:?} is required"));
                }
            }
        }
    }
    for (name, value) in params {
        match props.get(name) {
            None => return Err(format!("parameter {name:?} is not declared by the flow")),
            Some(prop) => check(name, prop, value)?,
        }
    }
    Ok(())
}

fn check(name: &str, prop: &Value, value: &Value) -> Result<(), String> {
    if let Some(any_of) = prop.get("anyOf").and_then(|a| a.as_array()) {
        if any_of.iter().any(|p| check(name, p, value).is_ok()) {
            return Ok(());
        }
        return Err(format!(
            "parameter {name:?} does not match any allowed type"
        ));
    }
    if let Some(choices) = prop.get("enum").and_then(|e| e.as_array()) {
        if choices.iter().any(|c| c == value)
            || value
                .as_str()
                .map(|s| choices.iter().any(|c| c.to_string().trim_matches('"') == s))
                .unwrap_or(false)
        {
            return Ok(());
        }
        return Err(format!("parameter {name:?} must be one of {choices:?}"));
    }
    let Some(kind) = prop.get("type").and_then(|t| t.as_str()) else {
        return Ok(());
    };
    let ok = match kind {
        "string" => value.is_string(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value
                    .as_str()
                    .map(|s| s.trim().parse::<i64>().is_ok())
                    .unwrap_or(false)
        }
        "number" => {
            value.is_number()
                || value
                    .as_str()
                    .map(|s| s.trim().parse::<f64>().is_ok())
                    .unwrap_or(false)
        }
        "boolean" => {
            value.is_boolean()
                || value
                    .as_str()
                    .map(|s| {
                        matches!(
                            s.to_ascii_lowercase().as_str(),
                            "true" | "false" | "1" | "0" | "yes" | "no"
                        )
                    })
                    .unwrap_or(false)
        }
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("parameter {name:?} expects {kind}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_and_types() {
        let schema = json!({"type":"object","properties":{"day":{"type":"string","format":"date"},"n":{"type":"integer","default":1}},"required":["day"]});
        let ok = json!({"day":"2026-09-06","n":"3"});
        assert!(validate_parameters(&schema, ok.as_object().unwrap()).is_ok());
        let missing = json!({"n":1});
        assert!(validate_parameters(&schema, missing.as_object().unwrap())
            .unwrap_err()
            .contains("day"));
        let bad = json!({"day":"2026-09-06","n":"abc"});
        assert!(validate_parameters(&schema, bad.as_object().unwrap())
            .unwrap_err()
            .contains("n"));
        let unknown = json!({"day":"x","zzz":1});
        assert!(validate_parameters(&schema, unknown.as_object().unwrap())
            .unwrap_err()
            .contains("zzz"));
    }
}
