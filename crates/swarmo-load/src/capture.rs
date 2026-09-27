//! The minimal JSONPath subset used by declarative scenario captures:
//! `$.a.b[0].c` — dot access and numeric indices only.

use serde_json::Value;

/// Evaluate a path against a JSON document, returning a scalar as a string.
pub fn json_path<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = doc;
    let trimmed = path.trim();
    let rest = trimmed
        .strip_prefix("$.")
        .or_else(|| trimmed.strip_prefix('$'))
        .unwrap_or(trimmed);

    for raw_seg in rest.split('.') {
        if raw_seg.is_empty() {
            continue;
        }
        // Split "items[0][1]" into the key "items" and the indices.
        // Anything outside the subset — `['x']`, `[-1]`, `[*]`, an unclosed
        // bracket — matches nothing, rather than quietly yielding the parent.
        let (key, indices) = split_indices(raw_seg)?;
        if !key.is_empty() {
            cur = cur.get(key)?;
        }
        for idx in indices {
            cur = cur.get(idx)?;
        }
    }
    Some(cur)
}

fn split_indices(seg: &str) -> Option<(&str, Vec<usize>)> {
    match seg.find('[') {
        None => Some((seg, Vec::new())),
        Some(pos) => {
            let key = &seg[..pos];
            let mut indices = Vec::new();
            let mut rest = &seg[pos..];
            while !rest.is_empty() {
                let inner = rest.strip_prefix('[')?;
                let close = inner.find(']')?;
                indices.push(inner[..close].trim().parse::<usize>().ok()?);
                rest = &inner[close + 1..];
            }
            Some((key, indices))
        }
    }
}

/// A captured value rendered for substitution into `{{vars}}`.
pub fn value_to_var(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Extract a value from a response body using a path expression.
pub fn capture_from_body(body: &str, path: &str) -> Option<String> {
    let doc: Value = serde_json::from_str(body).ok()?;
    json_path(&doc, path).map(value_to_var)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dot_access() {
        let d = json!({"a": {"b": {"c": "deep"}}});
        assert_eq!(value_to_var(json_path(&d, "$.a.b.c").unwrap()), "deep");
    }

    #[test]
    fn array_index() {
        let d = json!({"items": [{"id": 1}, {"id": 2}]});
        assert_eq!(value_to_var(json_path(&d, "$.items[1].id").unwrap()), "2");
    }

    #[test]
    fn nested_indices() {
        let d = json!({"m": [[10, 20], [30, 40]]});
        assert_eq!(value_to_var(json_path(&d, "$.m[1][0]").unwrap()), "30");
    }

    #[test]
    fn root_array() {
        let d = json!([{"t": "x"}]);
        assert_eq!(value_to_var(json_path(&d, "$[0].t").unwrap()), "x");
    }

    #[test]
    fn missing_path_is_none() {
        let d = json!({"a": 1});
        assert!(json_path(&d, "$.b.c").is_none());
        assert!(json_path(&d, "$.a[5]").is_none());
    }

    #[test]
    fn unsupported_brackets_match_nothing() {
        // Each of these used to stop parsing at the bracket and hand back the
        // parent value — capturing a whole object where one field was meant.
        let d = json!({"x": 1, "items": [1, 2]});
        for path in [
            "$['x']",
            "$.items[-1]",
            "$.items[*]",
            "$.items[1",
            "$.items[0]x",
        ] {
            assert!(json_path(&d, path).is_none(), "{path}");
        }
    }

    #[test]
    fn strings_are_unquoted_numbers_are_not() {
        let d = json!({"s": "hi", "n": 5, "b": true});
        assert_eq!(value_to_var(json_path(&d, "$.s").unwrap()), "hi");
        assert_eq!(value_to_var(json_path(&d, "$.n").unwrap()), "5");
        assert_eq!(value_to_var(json_path(&d, "$.b").unwrap()), "true");
    }

    #[test]
    fn works_without_the_dollar_prefix() {
        let d = json!({"token": "t"});
        assert_eq!(capture_from_body(&d.to_string(), "token").unwrap(), "t");
    }

    #[test]
    fn invalid_json_body() {
        assert!(capture_from_body("not json", "$.a").is_none());
    }
}
