//! Inheritance merging (collection -> folder -> request) and finalization
//! (variable interpolation, auth header generation, query-string building).
//!
//! Two phases, deliberately separated so pre-request scripts can run in between:
//!
//!   merge_chain(ancestors, request) -> MergedRequest   (still contains {{vars}})
//!        ...pre-request scripts may mutate MergedRequest and set variables...
//!   finalize(merged, scope)         -> ResolvedRequest (ready to send)

use base64::Engine as _;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};

use crate::interp::{interpolate, Unresolved, VarScope};
use crate::model::{
    Auth, Body, ContainerDef, KeyValue, MultipartKind, RequestDef, RequestSettings,
};

// ---------------------------------------------------------------------------
// Merged (post-inheritance, pre-interpolation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedRequest {
    /// The request's stable id, carried through so anything recording a send
    /// can name which request it was rather than only where it lived.
    pub id: String,
    pub name: String,
    pub method: String,
    pub url: String,
    pub params: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub auth: Auth,
    pub body: Body,
    pub settings: RequestSettings,
    /// Outermost first: collection, then folders top-down, then the request.
    pub pre_scripts: Vec<String>,
    /// Innermost first: the request, then folders bottom-up, then collection.
    pub post_scripts: Vec<String>,
}

/// Merge a chain of containers (collection first, then nested folders in
/// top-down order) with the request itself.
pub fn merge_chain(ancestors: &[ContainerDef], req: &RequestDef) -> MergedRequest {
    // Headers: start from the outermost, let inner levels override by key.
    let mut headers: Vec<KeyValue> = Vec::new();
    for a in ancestors {
        upsert_headers(&mut headers, &a.headers);
    }
    upsert_headers(&mut headers, &req.headers);

    // Auth: nearest explicit (non-Inherit) declaration wins.
    let auth = if !matches!(req.auth, Auth::Inherit) {
        req.auth.clone()
    } else {
        ancestors
            .iter()
            .rev()
            .find(|a| !matches!(a.auth, Auth::Inherit))
            .map(|a| a.auth.clone())
            .unwrap_or(Auth::None)
    };

    let mut pre_scripts: Vec<String> = ancestors
        .iter()
        .filter(|a| !a.scripts.pre_request.trim().is_empty())
        .map(|a| a.scripts.pre_request.clone())
        .collect();
    if !req.scripts.pre_request.trim().is_empty() {
        pre_scripts.push(req.scripts.pre_request.clone());
    }

    let mut post_scripts: Vec<String> = Vec::new();
    if !req.scripts.post_response.trim().is_empty() {
        post_scripts.push(req.scripts.post_response.clone());
    }
    for a in ancestors.iter().rev() {
        if !a.scripts.post_response.trim().is_empty() {
            post_scripts.push(a.scripts.post_response.clone());
        }
    }

    MergedRequest {
        id: req.id.clone(),
        name: req.name.clone(),
        method: req.method.clone(),
        url: req.url.clone(),
        params: req.params.clone(),
        headers,
        auth,
        body: req.body.clone(),
        settings: req.settings.clone(),
        pre_scripts,
        post_scripts,
    }
}

/// Layer one level's headers over the outer levels'.
///
/// A key set here replaces every outer value for it, but repeats within this
/// level are all kept: some APIs mean something by a header sent twice.
fn upsert_headers(into: &mut Vec<KeyValue>, from: &[KeyValue]) {
    let here: Vec<&KeyValue> = from.iter().filter(|h| h.enabled).collect();
    into.retain(|e| !here.iter().any(|h| h.key.eq_ignore_ascii_case(&e.key)));
    into.extend(here.into_iter().cloned());
}

// ---------------------------------------------------------------------------
// Resolved (ready to execute)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPart {
    pub key: String,
    pub kind: MultipartKind,
    pub value: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolvedBody {
    None,
    #[serde(rename_all = "camelCase")]
    Bytes {
        content_type: Option<String>,
        /// Text is kept as a String; binary file bodies use `File`.
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Form {
        fields: Vec<(String, String)>,
    },
    #[serde(rename_all = "camelCase")]
    Multipart {
        parts: Vec<ResolvedPart>,
    },
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRequest {
    pub name: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: ResolvedBody,
    pub settings: RequestSettings,
    pub unresolved: Vec<String>,
}

/// Interpolate everything and produce a directly sendable request.
pub fn finalize(merged: &MergedRequest, scope: &VarScope) -> ResolvedRequest {
    let mut unresolved: Vec<String> = Vec::new();
    // Set inside the body match, pushed once `interp` has released its
    // borrow of `unresolved`.
    let mut graphql_note: Option<String> = None;
    let mut interp = |s: &str| -> String {
        let (out, un) = interpolate(s, scope);
        collect(&mut unresolved, un);
        out
    };

    let mut url = interp(&merged.url);

    // Query params.
    let pairs: Vec<(String, String)> = merged
        .params
        .iter()
        .filter(|p| p.enabled)
        .map(|p| (interp(&p.key), interp(&p.value)))
        .collect();
    if !pairs.is_empty() {
        let qs = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
            .collect::<Vec<_>>()
            .join("&");
        // Anything after '#' is a fragment and never sent, so the query has
        // to go in ahead of it.
        let fragment = url.find('#').map(|i| url.split_off(i));
        if url.contains('?') {
            if url.ends_with('?') || url.ends_with('&') {
                url.push_str(&qs);
            } else {
                url.push('&');
                url.push_str(&qs);
            }
        } else {
            url.push('?');
            url.push_str(&qs);
        }
        if let Some(fragment) = fragment {
            url.push_str(&fragment);
        }
    }

    let mut headers: Vec<(String, String)> = merged
        .headers
        .iter()
        .filter(|h| h.enabled)
        .map(|h| (interp(&h.key), interp(&h.value)))
        .collect();

    // Auth -> header.
    match &merged.auth {
        Auth::Inherit | Auth::None => {}
        Auth::Basic { username, password } => {
            let raw = format!("{}:{}", interp(username), interp(password));
            let b64 = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
            set_header(&mut headers, "Authorization", format!("Basic {b64}"));
        }
        Auth::Bearer { token } => {
            let t = interp(token);
            set_header(&mut headers, "Authorization", format!("Bearer {t}"));
        }
        Auth::ApiKeyHeader { header_name, value } => {
            let n = interp(header_name);
            let v = interp(value);
            if !n.trim().is_empty() {
                set_header(&mut headers, &n, v);
            }
        }
        // Nothing to apply here: running a command needs a process, which this
        // crate deliberately cannot do. The caller resolves the token first and
        // hands finalize an ApiKeyHeader carrying the result.
        Auth::CommandToken { .. } => {}
    }

    // Body.
    let body = match &merged.body {
        Body::None => ResolvedBody::None,
        Body::Json { text } => {
            ensure_content_type(&mut headers, "application/json");
            ResolvedBody::Bytes {
                content_type: Some("application/json".into()),
                text: interp(text),
            }
        }
        Body::Text { text, content_type } => {
            let ct = content_type
                .clone()
                .unwrap_or_else(|| "text/plain".to_string());
            ensure_content_type(&mut headers, &ct);
            ResolvedBody::Bytes {
                content_type: Some(ct),
                text: interp(text),
            }
        }
        Body::Form { fields } => {
            ensure_content_type(&mut headers, "application/x-www-form-urlencoded");
            ResolvedBody::Form {
                fields: fields
                    .iter()
                    .filter(|f| f.enabled)
                    .map(|f| (interp(&f.key), interp(&f.value)))
                    .collect(),
            }
        }
        Body::Multipart { parts } => ResolvedBody::Multipart {
            parts: parts
                .iter()
                .filter(|p| p.enabled)
                .map(|p| ResolvedPart {
                    key: interp(&p.key),
                    kind: p.kind,
                    value: interp(&p.value),
                    content_type: p.content_type.clone(),
                })
                .collect(),
        },
        Body::Graphql { query, variables } => {
            ensure_content_type(&mut headers, "application/json");
            let vars_txt = interp(variables);
            let vars_json: serde_json::Value = if vars_txt.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(&vars_txt) {
                    Ok(v) => v,
                    Err(_) => {
                        // Sending `{}` in place of a typo would make the
                        // server answer a different query than the one
                        // written, with nothing to say so. Flag it like an
                        // unresolved variable, and send the text as-is so
                        // the server's own error names the problem.
                        graphql_note = Some("graphql.variables (not valid JSON)".to_string());
                        serde_json::Value::String(vars_txt.clone())
                    }
                }
            };
            let payload = serde_json::json!({
                "query": interp(query),
                "variables": vars_json,
            });
            ResolvedBody::Bytes {
                content_type: Some("application/json".into()),
                text: payload.to_string(),
            }
        }
        Body::Binary { path } => ResolvedBody::File { path: interp(path) },
    };
    if let Some(note) = graphql_note {
        if !unresolved.contains(&note) {
            unresolved.push(note);
        }
    }

    ResolvedRequest {
        name: merged.name.clone(),
        method: merged.method.to_uppercase(),
        url,
        headers,
        body,
        settings: merged.settings.clone(),
        unresolved,
    }
}

fn collect(into: &mut Vec<String>, un: Vec<Unresolved>) {
    for u in un {
        if !into.contains(&u.name) {
            into.push(u.name);
        }
    }
}

/// What a query component must escape.
///
/// RFC 3986 calls `-`, `.`, `_` and `~` *unreserved*: they carry no syntactic
/// meaning in a URL and never need escaping. Encoding them anyway is legal but
/// wasteful and hostile to read — `creative_000001` going out as
/// `creative%5F000001` is three bytes longer, unrecognisable in a log or an
/// APM trace, and breaks any scheme that signs a canonical query string.
/// Everything genuinely delimiting (`&`, `=`, `?`, `#`, space, …) is still
/// escaped, so this is narrower without being laxer.
const QUERY_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, QUERY_ESCAPE).to_string()
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    match headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
    {
        Some(h) => h.1 = value,
        None => headers.push((name.to_string(), value)),
    }
}

fn ensure_content_type(headers: &mut Vec<(String, String)>, ct: &str) {
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("Content-Type".to_string(), ct.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Scripts;
    use std::collections::HashMap;

    fn scope(pairs: &[(&str, &str)]) -> VarScope {
        let mut s = VarScope::new();
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s.push_layer(m);
        s
    }

    #[test]
    fn child_header_overrides_parent() {
        let mut coll = ContainerDef::new("c");
        coll.headers = vec![KeyValue::new("X-A", "parent"), KeyValue::new("X-B", "keep")];
        let mut req = RequestDef::new("r");
        req.headers = vec![KeyValue::new("x-a", "child")];

        let merged = merge_chain(&[coll], &req);
        let a = merged
            .headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("x-a"))
            .unwrap();
        assert_eq!(a.value, "child");
        assert!(merged
            .headers
            .iter()
            .any(|h| h.key == "X-B" && h.value == "keep"));
    }

    #[test]
    fn nearest_explicit_auth_wins() {
        let mut coll = ContainerDef::new("c");
        coll.auth = Auth::Bearer {
            token: "coll".into(),
        };
        let mut folder = ContainerDef::new("f");
        folder.auth = Auth::Bearer {
            token: "folder".into(),
        };
        let req = RequestDef::new("r"); // Inherit

        let merged = merge_chain(&[coll, folder], &req);
        match merged.auth {
            Auth::Bearer { ref token } => assert_eq!(token, "folder"),
            _ => panic!("expected bearer"),
        }
    }

    #[test]
    fn explicit_none_stops_inheritance() {
        let mut coll = ContainerDef::new("c");
        coll.auth = Auth::Bearer {
            token: "coll".into(),
        };
        let mut req = RequestDef::new("r");
        req.auth = Auth::None;
        let merged = merge_chain(&[coll], &req);
        let r = finalize(&merged, &scope(&[]));
        assert!(!r
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization")));
    }

    #[test]
    fn script_order_is_outside_in_then_inside_out() {
        let mut coll = ContainerDef::new("c");
        coll.scripts = Scripts {
            pre_request: "COLL".into(),
            post_response: "COLL".into(),
        };
        let mut req = RequestDef::new("r");
        req.scripts = Scripts {
            pre_request: "REQ".into(),
            post_response: "REQ".into(),
        };
        let merged = merge_chain(&[coll], &req);
        assert_eq!(merged.pre_scripts, vec!["COLL", "REQ"]);
        assert_eq!(merged.post_scripts, vec!["REQ", "COLL"]);
    }

    #[test]
    fn builds_query_string() {
        let mut req = RequestDef::new("r");
        req.url = "http://x/y".into();
        req.params = vec![KeyValue::new("a", "1"), KeyValue::new("b", "hello world")];
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert_eq!(r.url, "http://x/y?a=1&b=hello%20world");
    }

    #[test]
    fn query_goes_ahead_of_a_fragment() {
        // Anything after '#' is never sent, params included.
        let mut req = RequestDef::new("r");
        req.url = "http://x/y#frag".into();
        req.params = vec![KeyValue::new("a", "1")];
        let merged = merge_chain(&[], &req);
        assert_eq!(finalize(&merged, &scope(&[])).url, "http://x/y?a=1#frag");
    }

    #[test]
    fn repeated_headers_on_one_level_are_all_kept() {
        let mut coll = ContainerDef::new("c");
        coll.headers = vec![KeyValue::new("X-A", "parent")];
        let mut req = RequestDef::new("r");
        req.headers = vec![KeyValue::new("X-A", "1"), KeyValue::new("x-a", "2")];

        let merged = merge_chain(&[coll], &req);
        let values: Vec<&str> = merged.headers.iter().map(|h| h.value.as_str()).collect();
        assert_eq!(values, ["1", "2"]);
    }

    #[test]
    fn unreserved_characters_are_not_escaped() {
        let mut req = RequestDef::new("r");
        req.url = "http://x/y".into();
        req.params = vec![
            KeyValue::new("id", "creative_000001"),
            KeyValue::new("range", "a-b.c~d"),
        ];
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert_eq!(r.url, "http://x/y?id=creative_000001&range=a-b.c~d");
    }

    #[test]
    fn delimiters_are_still_escaped() {
        // Narrower must not mean laxer: anything that would change how the
        // query parses still has to be escaped.
        let mut req = RequestDef::new("r");
        req.url = "http://x/y".into();
        req.params = vec![KeyValue::new("q", "a&b=c?d#e f/g+h,i")];
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert_eq!(r.url, "http://x/y?q=a%26b%3Dc%3Fd%23e%20f%2Fg%2Bh%2Ci");
    }

    #[test]
    fn invalid_graphql_variables_are_flagged_rather_than_sent_as_empty() {
        let mut req = RequestDef::new("r");
        req.url = "http://x/gql".into();
        req.body = Body::Graphql {
            query: "{ me }".into(),
            variables: "{\"id\": 1,}".into(), // trailing comma
        };
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert!(
            r.unresolved.iter().any(|u| u.contains("graphql.variables")),
            "{:?}",
            r.unresolved
        );
        // And the text goes through as written, so the server's own error
        // names the problem rather than answering a different query.
        match r.body {
            ResolvedBody::Bytes { text, .. } => {
                assert!(text.contains("\"id\\\": 1,}") || text.contains("1,}"))
            }
            other => panic!("unexpected body {other:?}"),
        }
    }

    #[test]
    fn appends_to_existing_query_string() {
        let mut req = RequestDef::new("r");
        req.url = "http://x/y?z=0".into();
        req.params = vec![KeyValue::new("a", "1")];
        let merged = merge_chain(&[], &req);
        assert_eq!(finalize(&merged, &scope(&[])).url, "http://x/y?z=0&a=1");
    }

    #[test]
    fn disabled_params_skipped() {
        let mut req = RequestDef::new("r");
        req.url = "http://x".into();
        let mut p = KeyValue::new("a", "1");
        p.enabled = false;
        req.params = vec![p];
        let merged = merge_chain(&[], &req);
        assert_eq!(finalize(&merged, &scope(&[])).url, "http://x");
    }

    #[test]
    fn basic_auth_header() {
        let mut req = RequestDef::new("r");
        req.auth = Auth::Basic {
            username: "u".into(),
            password: "p".into(),
        };
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        let h = r
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .unwrap();
        assert_eq!(h.1, "Basic dTpw");
    }

    #[test]
    fn bearer_interpolates() {
        let mut req = RequestDef::new("r");
        req.auth = Auth::Bearer {
            token: "{{apiKey}}".into(),
        };
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[("apiKey", "secret123")]));
        assert_eq!(
            r.headers
                .iter()
                .find(|(k, _)| k == "Authorization")
                .unwrap()
                .1,
            "Bearer secret123"
        );
    }

    #[test]
    fn graphql_body_becomes_json_payload() {
        let mut req = RequestDef::new("r");
        req.body = Body::Graphql {
            query: "{ me { id } }".into(),
            variables: "{\"a\":1}".into(),
        };
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        match r.body {
            ResolvedBody::Bytes { ref text, .. } => {
                let v: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(v["query"], "{ me { id } }");
                assert_eq!(v["variables"]["a"], 1);
            }
            _ => panic!("expected bytes body"),
        }
    }

    #[test]
    fn json_body_sets_content_type_but_respects_explicit() {
        let mut req = RequestDef::new("r");
        req.body = Body::Json { text: "{}".into() };
        req.headers = vec![KeyValue::new("Content-Type", "application/vnd.x+json")];
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert_eq!(
            r.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .unwrap()
                .1,
            "application/vnd.x+json"
        );
    }

    #[test]
    fn unresolved_vars_reported() {
        let mut req = RequestDef::new("r");
        req.url = "{{baseUrl}}/x".into();
        let merged = merge_chain(&[], &req);
        let r = finalize(&merged, &scope(&[]));
        assert_eq!(r.unresolved, vec!["baseUrl".to_string()]);
    }
}
