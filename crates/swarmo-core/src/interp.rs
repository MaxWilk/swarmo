//! `{{variable}}` interpolation with a layered scope. See docs/formats.md.
//!
//! Resolution order (first hit wins):
//!   1. runtime overrides (set by scripts / load-test captures)
//!   2. active environment (with secrets merged in)
//!
//! Unresolved variables are left as literal `{{name}}` text and reported so the
//! UI can underline them.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

fn var_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}").unwrap())
}

/// Layered variable lookup. Later-added layers take priority.
#[derive(Debug, Clone, Default)]
pub struct VarScope {
    /// Lowest priority first.
    layers: Vec<HashMap<String, String>>,
}

impl VarScope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a layer with higher priority than everything already present.
    pub fn push_layer(&mut self, map: HashMap<String, String>) {
        self.layers.push(map);
    }

    /// Ensure a top-most mutable layer exists and return it.
    fn top_mut(&mut self) -> &mut HashMap<String, String> {
        if self.layers.is_empty() {
            self.layers.push(HashMap::new());
        }
        self.layers.last_mut().unwrap()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.top_mut().insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        for layer in self.layers.iter().rev() {
            if let Some(v) = layer.get(key) {
                return Some(v.as_str());
            }
        }
        None
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Flatten all layers into a single map (highest priority wins).
    pub fn flatten(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for layer in &self.layers {
            for (k, v) in layer {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// A child scope that inherits everything here plus its own mutable top layer.
    pub fn child(&self) -> VarScope {
        let mut c = self.clone();
        c.layers.push(HashMap::new());
        c
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub name: String,
}

/// Interpolate `{{vars}}`. Nested/recursive references are resolved up to
/// `MAX_DEPTH` passes so `{{baseUrl}}` can itself contain `{{host}}`.
const MAX_DEPTH: usize = 5;

pub fn interpolate(text: &str, scope: &VarScope) -> (String, Vec<Unresolved>) {
    let mut unresolved: Vec<Unresolved> = Vec::new();
    let mut current = text.to_string();

    for _ in 0..MAX_DEPTH {
        if !current.contains("{{") {
            break;
        }
        let mut changed = false;
        let next = var_re()
            .replace_all(&current, |caps: &regex::Captures| {
                let name = &caps[1];
                match scope.get(name) {
                    Some(v) => {
                        changed = true;
                        v.to_string()
                    }
                    None => caps[0].to_string(),
                }
            })
            .into_owned();
        current = next;
        if !changed {
            break;
        }
    }

    // Generators run after variables, so a variable whose *value* is a
    // generator token still expands, and generator arguments see resolved text.
    let (current, bad) = crate::generators::expand(&current);
    for token in bad {
        if !unresolved.iter().any(|u| u.name == token) {
            unresolved.push(Unresolved { name: token });
        }
    }

    // Whatever `{{...}}` remains is unresolved.
    for caps in var_re().captures_iter(&current) {
        let name = caps[1].to_string();
        if !unresolved.iter().any(|u| u.name == name) {
            unresolved.push(Unresolved { name });
        }
    }

    (current, unresolved)
}

/// Interpolate without collecting diagnostics.
pub fn interpolate_str(text: &str, scope: &VarScope) -> String {
    interpolate(text, scope).0
}

/// List every `{{var}}` referenced in `text`, in order of first appearance.
pub fn referenced_vars(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for caps in var_re().captures_iter(text) {
        let name = caps[1].to_string();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope_of(pairs: &[(&str, &str)]) -> VarScope {
        let mut s = VarScope::new();
        s.push_layer(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        s
    }

    #[test]
    fn resolves_simple() {
        let s = scope_of(&[("baseUrl", "http://x")]);
        let (out, un) = interpolate("{{baseUrl}}/orders", &s);
        assert_eq!(out, "http://x/orders");
        assert!(un.is_empty());
    }

    #[test]
    fn tolerates_whitespace() {
        let s = scope_of(&[("a", "1")]);
        assert_eq!(interpolate_str("{{ a }}", &s), "1");
    }

    #[test]
    fn reports_unresolved_and_keeps_literal() {
        let s = scope_of(&[]);
        let (out, un) = interpolate("{{missing}}/x", &s);
        assert_eq!(out, "{{missing}}/x");
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].name, "missing");
    }

    #[test]
    fn nested_resolution() {
        let s = scope_of(&[("host", "example.com"), ("baseUrl", "https://{{host}}")]);
        assert_eq!(
            interpolate_str("{{baseUrl}}/a", &s),
            "https://example.com/a"
        );
    }

    #[test]
    fn cycle_terminates() {
        let s = scope_of(&[("a", "{{b}}"), ("b", "{{a}}")]);
        let (_out, _un) = interpolate("{{a}}", &s);
        // Just must not hang or panic.
    }

    #[test]
    fn layer_priority() {
        let mut s = scope_of(&[("k", "env")]);
        s.push_layer([("k".to_string(), "runtime".to_string())].into());
        assert_eq!(interpolate_str("{{k}}", &s), "runtime");
    }

    #[test]
    fn referenced_vars_lists_in_order() {
        assert_eq!(
            referenced_vars("{{a}}/{{b}}/{{a}}"),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
