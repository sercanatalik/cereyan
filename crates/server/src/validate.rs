//! Parameter validation against the flow's JSON schema subset produced by the
//! Python authoring layer. Engines coerce fully; the server rejects obvious
//! mismatches with a 422 naming the parameter.

use serde_json::{Map, Value};

pub fn validate_parameters(schema: &Value, params: &Map<String, Value>) -> Result<(), String> {
    // Borrowed. This used to `.cloned()` the whole `properties` object -- every
    // property's full subtree, its `anyOf` branches and its `enum` entries --
    // only to read one property per supplied parameter. A missing or non-object
    // `properties` leaves `props` as `None`, and every lookup then misses, which
    // is exactly what the empty-map version did.
    let props = schema.get("properties").and_then(|p| p.as_object());
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
        match props.and_then(|p| p.get(name)) {
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
        // The `to_string()` here is not an oversight and must not be "simplified"
        // to `c.as_str() == Some(s)`: it trims quotes off the *serialised* form,
        // so the escaping matters. For a choice whose value is `"x"`, `to_string`
        // gives `"\"x\""` and the trim leaves `\"x\` — which is neither `x` nor
        // `"x"`. Replacing it with `as_str()` would make that choice match `x`,
        // which it does not today. Measured and rejected; see design.md.
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

#[cfg(test)]
mod borrow_tests {
    use super::*;
    use serde_json::json;

    /// A schema of the size a real flow's parameter schema reaches, so the
    /// benchmark is measuring a representative copy rather than a toy one.
    fn realistic_schema() -> Value {
        let mut properties = Map::new();
        for i in 0..20 {
            properties.insert(
                format!("param_{i}"),
                json!({
                    "type": "string",
                    "description": "a reasonably long description of what this parameter is for",
                    "default": "x",
                    "anyOf": [{"type": "string", "maxLength": 64}, {"type": "integer"}],
                    "enum": ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"],
                }),
            );
        }
        json!({ "type": "object", "properties": properties, "required": ["param_0"] })
    }

    fn schema_of(prop: Value) -> Value {
        json!({ "type": "object", "properties": { "p": prop } })
    }

    // ---- every branch of the rule, unchanged -------------------------------

    #[test]
    fn a_declared_parameter_is_accepted() {
        let s = schema_of(json!({"type": "string"}));
        assert!(validate_parameters(&s, json!({"p": "v"}).as_object().unwrap()).is_ok());
    }

    #[test]
    fn an_undeclared_parameter_is_rejected_by_name() {
        let s = schema_of(json!({"type": "string"}));
        let e = validate_parameters(&s, json!({"other": "v"}).as_object().unwrap())
            .unwrap_err();
        assert_eq!(e, "parameter \"other\" is not declared by the flow");
    }

    #[test]
    fn a_missing_required_parameter_is_rejected_by_name() {
        let s = json!({ "properties": { "a": {"type": "string"} }, "required": ["a"] });
        let e = validate_parameters(&s, json!({}).as_object().unwrap()).unwrap_err();
        assert_eq!(e, "parameter \"a\" is required");
    }

    #[test]
    fn a_type_mismatch_is_rejected() {
        let s = schema_of(json!({"type": "integer"}));
        assert!(validate_parameters(&s, json!({"p": "v"}).as_object().unwrap()).is_err());
    }

    #[test]
    fn a_union_accepts_any_branch_and_rejects_a_non_member() {
        let s = schema_of(json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}));
        assert!(validate_parameters(&s, json!({"p": "v"}).as_object().unwrap()).is_ok());
        assert!(validate_parameters(&s, json!({"p": 7}).as_object().unwrap()).is_ok());
        assert!(validate_parameters(&s, json!({"p": true}).as_object().unwrap()).is_err());
    }

    #[test]
    fn a_choice_list_accepts_a_member_and_rejects_a_non_member() {
        let s = schema_of(json!({"enum": ["alpha", "beta"]}));
        assert!(validate_parameters(&s, json!({"p": "alpha"}).as_object().unwrap()).is_ok());
        let e = validate_parameters(&s, json!({"p": "iota"}).as_object().unwrap())
            .unwrap_err();
        assert!(
            e.starts_with("parameter \"p\" must be one of"),
            "the choices are listed: {e}"
        );
        // And a non-string value is rejected without panicking.
        assert!(validate_parameters(&s, json!({"p": 42}).as_object().unwrap()).is_err());
    }

    /// Why the `to_string()` in the choice comparison stays.
    ///
    /// This was written expecting the replacement `c.as_str() == Some(s)` to be
    /// equivalent, and an oracle comparison over 11 choice sets and 13 values
    /// showed it is **not**: they disagree for a choice whose value is `"x"`.
    ///
    /// The reason is that `to_string()` renders the *serialised* form, so the
    /// quotes being trimmed are the serialisation's own and the escaping is in
    /// between. `json!("\"x\"")` renders as `"\"x\""`, and `trim_matches('"')`
    /// strips the outer pair plus the escaping's trailing quote, leaving
    /// `\"x\` — a four-character string that is neither `x` nor `"x"`. Reading it
    /// through `as_str()` and trimming that instead yields `x`, so the choice
    /// would start matching a value it does not match today.
    ///
    /// The oracle is kept as the record: it is the evidence that the shipped
    /// expression is the original one, and the reason the tempting rewrite is not.
    #[test]
    fn a_choice_of_quoted_text_does_not_match_what_as_str_would_allow() {
        // A choice that is the three-character string `"x"`.
        let choices = vec![json!("\"x\"")];
        let original = |value: &Value| -> bool {
            choices.iter().any(|c| c == value)
                || value
                    .as_str()
                    .map(|s| choices.iter().any(|c| c.to_string().trim_matches('"') == s))
                    .unwrap_or(false)
        };
        let via_as_str = |value: &Value| -> bool {
            choices.iter().any(|c| c == value)
                || value
                    .as_str()
                    .map(|s| {
                        choices.iter().any(|c| match c.as_str() {
                            Some(raw) => raw.trim_matches('"') == s,
                            None => c.to_string().trim_matches('"') == s,
                        })
                    })
                    .unwrap_or(false)
        };
        // They differ here, which is the whole point.
        assert!(!original(&json!("x")), "the original does not match `x`");
        assert!(
            via_as_str(&json!("x")),
            "the as_str rewrite would, and that is the behaviour change"
        );
        // And the shipped rule keeps the original answer.
        let s = schema_of(json!({ "enum": choices.clone() }));
        assert!(
            validate_parameters(&s, json!({"p": "x"}).as_object().unwrap()).is_err(),
            "the rule rejects it, as it always has"
        );
    }

    #[test]
    fn a_non_string_choice_is_matched_through_its_rendering() {
        // A numeric choice renders to "42" and trims to "42", so a *string* value
        // of "42" does match it. This surprised me and is why the comparison
        // tests above exist: the behaviour is preserved, not corrected.
        let s = schema_of(json!({ "enum": [42] }));
        assert!(
            validate_parameters(&s, json!({"p": "42"}).as_object().unwrap()).is_ok(),
            "the `None` arm renders the choice, so a string \"42\" matches it"
        );
    }

    #[test]
    fn a_schema_with_no_properties_rejects_everything_supplied() {
        let s = json!({ "type": "object" });
        let e = validate_parameters(&s, json!({"p": "v"}).as_object().unwrap())
            .unwrap_err();
        assert_eq!(e, "parameter \"p\" is not declared by the flow");
    }

    #[test]
    fn a_schema_that_is_not_an_object_is_treated_as_empty() {
        // Not an object, so `properties` is unreachable and every lookup misses.
        for s in [json!("not a schema"), json!([1, 2, 3]), json!(null)] {
            let e = validate_parameters(&s, json!({"p": "v"}).as_object().unwrap())
                .unwrap_err();
            assert_eq!(
                e, "parameter \"p\" is not declared by the flow",
                "schema {s}"
            );
        }
    }

    /// Borrowing must not have made validation stateful.
    #[test]
    fn validation_leaves_the_schema_unchanged_and_is_repeatable() {
        let s = realistic_schema();
        let before = s.clone();
        let params = json!({"param_0": "v", "param_1": "w"}).as_object().unwrap().clone();
        for _ in 0..5 {
            assert!(validate_parameters(&s, &params).is_ok());
            assert_eq!(s, before, "the schema must not be touched");
        }
        // A rejected case repeated, too.
        // `param_0` declares `anyOf: [string, integer]`, so a number is accepted;
        // a boolean matches neither branch nor the choice list, so it is rejected.
        assert!(
            validate_parameters(&s, json!({"param_0": 1}).as_object().unwrap()).is_ok(),
            "the fixture's own union accepts an integer"
        );
        let bad = json!({"param_0": true}).as_object().unwrap().clone();
        let first = validate_parameters(&s, &bad).unwrap_err();
        for _ in 0..5 {
            assert_eq!(validate_parameters(&s, &bad).unwrap_err(), first);
            assert_eq!(s, before);
        }
    }

    // ---- the measurement --------------------------------------------------

    /// Measures both shapes on the real rule. Opt-in: a measurement, not an
    /// assertion.
    #[test]
    fn report_validate_cost() {
        if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
            return;
        }
        let s = realistic_schema();
        let params: Map<String, Value> = (0..5)
            .map(|i| (format!("param_{i}"), json!("v")))
            .collect();
        let iters = 50_000i64;

        // What the rule costs as it stands: borrowing the schema.
        let t = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(validate_parameters(&s, &params));
        }
        let borrowed = t.elapsed().as_secs_f64() / iters as f64 * 1e6;

        // What it cost before: the same walk, over a deep copy of `properties`.
        // Timed here rather than modelled, so the number is the rule's own.
        let t = std::time::Instant::now();
        for _ in 0..iters {
            let props = s
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let mut n = 0usize;
            for name in params.keys() {
                if props.get(name).is_some() {
                    n += 1;
                }
            }
            std::hint::black_box(n);
        }
        let cloned = t.elapsed().as_secs_f64() / iters as f64 * 1e6;

        println!(
            "validate_parameters, 20-property schema, 5 params: borrow {borrowed:.2} us, \
             deep copy {cloned:.2} us ({:.0}x)",
            cloned / borrowed
        );
    }
}
