//! Unique run keys: what makes two runs "the same" for admission.

use serde_json::{Map, Value};

/// Render a key template over parameters: `{day}` becomes the parameter's
/// JSON text (strings without quotes); a missing name renders empty. With no
/// template the key is every parameter as sorted JSON.
pub fn render_key(template: Option<&str>, parameters: &Map<String, Value>) -> String {
    match template.map(str::trim).filter(|t| !t.is_empty()) {
        None => serde_json::to_string(parameters).unwrap_or_default(),
        Some(t) => {
            let mut out = String::with_capacity(t.len());
            let mut rest = t;
            while let Some(start) = rest.find('{') {
                out.push_str(&rest[..start]);
                let after = &rest[start + 1..];
                match after.find('}') {
                    Some(end) => {
                        let name = after[..end].trim();
                        if let Some(v) = parameters.get(name) {
                            match v {
                                Value::String(s) => out.push_str(s),
                                other => out.push_str(&other.to_string()),
                            }
                        }
                        rest = &after[end + 1..];
                    }
                    None => {
                        out.push_str(&rest[start..]);
                        rest = "";
                    }
                }
            }
            out.push_str(rest);
            out
        }
    }
}

/// The stored key: the flow, the rendered key, and the period bucket (fixed
/// windows of `period` seconds, as Oban does) when a period is set.
pub fn unique_key(
    flow_id: i64,
    rendered: &str,
    period_secs: Option<f64>,
    now_micros: i64,
) -> String {
    let bucket = match period_secs.filter(|p| *p > 0.0) {
        Some(p) => (now_micros / (p * 1_000_000.0) as i64).to_string(),
        None => String::new(),
    };
    format!("flow:{flow_id}|{rendered}|{bucket}")
}

/// The key of a request-level idempotency key, valid in every state within its TTL.
pub fn idempotency_key(flow_id: i64, key: &str) -> String {
    format!("flow:{flow_id}|idem:{}|", key.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_templates_and_buckets() {
        let params: Map<String, Value> =
            serde_json::from_value(json!({"day": "2026-09-20", "n": 3})).unwrap();
        assert_eq!(render_key(Some("{day}/{n}"), &params), "2026-09-20/3");
        assert_eq!(render_key(Some("{missing}-x"), &params), "-x");
        assert_eq!(
            render_key(None, &params),
            "{\"day\":\"2026-09-20\",\"n\":3}"
        );
        assert_eq!(unique_key(7, "a", None, 123), "flow:7|a|");
        assert_eq!(
            unique_key(7, "a", Some(3600.0), 7_200_000_000),
            "flow:7|a|2"
        );
        assert_eq!(idempotency_key(7, " k "), "flow:7|idem:k|");
    }
}
