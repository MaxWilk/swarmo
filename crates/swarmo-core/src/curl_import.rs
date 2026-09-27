//! Turn a `curl` command line into a request.
//!
//! The common case is a command copied out of a browser's network panel, which
//! is why this lives in Rust rather than the UI: tokenizing shell quoting
//! correctly is fiddly enough to want real tests around it, and doing it here
//! means the CLI can reuse it later.
//!
//! Unsupported flags are reported as warnings rather than refused. A command
//! that is 90% importable should import, with the missing 10% named.

use std::iter::Peekable;
use std::str::Chars;

use crate::model::{Auth, Body, KeyValue, MultipartKind, MultipartPart, RequestDef};

/// A request recovered from a command line, plus anything that was dropped.
#[derive(Debug, Clone)]
pub struct ImportedCurl {
    pub def: RequestDef,
    pub warnings: Vec<String>,
}

/// Split a command line into words the way a POSIX shell would.
///
/// Handles single quotes (literal), double quotes (with backslash escapes),
/// `$'…'` (with Bash's ANSI-C escapes decoded), and backslash-newline
/// continuations.
pub fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                // A backslash before a newline continues the line; the pair
                // disappears rather than joining the words either side.
                Some('\n') => {
                    chars.next();
                }
                Some('\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(_) => {
                    cur.push(chars.next().unwrap());
                    has_token = true;
                }
                None => {}
            },
            '\'' => {
                has_token = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                has_token = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            // Inside double quotes a backslash only escapes a
                            // few characters; anything else keeps both.
                            match chars.peek() {
                                Some(&n @ ('"' | '\\' | '$' | '`')) => {
                                    cur.push(n);
                                    chars.next();
                                }
                                Some('\n') => {
                                    chars.next();
                                }
                                _ => cur.push('\\'),
                            }
                        }
                        _ => cur.push(c),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                // $'…' is Bash's ANSI-C quoting. Browsers emit it for anything
                // with a quote or an awkward character — Chrome writes `!` as
                // ! — so every escape Bash knows is decoded. \x and octal
                // escapes are bytes, not characters, hence the byte buffer.
                chars.next();
                has_token = true;
                let mut bytes = Vec::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\'' => break,
                        '\\' => ansi_c_escape(&mut chars, &mut bytes),
                        _ => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
                    }
                }
                cur.push_str(&String::from_utf8_lossy(&bytes));
            }
            c if c.is_whitespace() => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            _ => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// Decode the escape after a backslash inside `$'…'`, the way Bash does.
fn ansi_c_escape(chars: &mut Peekable<Chars<'_>>, out: &mut Vec<u8>) {
    let Some(c) = chars.next() else {
        out.push(b'\\');
        return;
    };
    let byte = match c {
        'n' => b'\n',
        'r' => b'\r',
        't' => b'\t',
        'a' => 0x07,
        'b' => 0x08,
        'e' | 'E' => 0x1b,
        'f' => 0x0c,
        'v' => 0x0b,
        '\\' | '\'' | '"' | '?' => c as u8,
        // One to three octal digits, the first already read.
        '0'..='7' => {
            let (rest, n) = read_digits(chars, 8, 2);
            let value = (c as u32 - '0' as u32) * 8u32.pow(n as u32) + rest;
            (value & 0xff) as u8
        }
        'x' | 'u' | 'U' => {
            let max = match c {
                'x' => 2,
                'u' => 4,
                _ => 8,
            };
            let (value, n) = read_digits(chars, 16, max);
            if n == 0 {
                // No digits: Bash keeps the escape as written.
                out.push(b'\\');
                out.push(c as u8);
            } else if c == 'x' {
                out.push(value as u8);
            } else {
                let ch = char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER);
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
            return;
        }
        // An escape Bash does not know keeps its backslash.
        other => {
            out.push(b'\\');
            out.extend_from_slice(other.encode_utf8(&mut [0; 4]).as_bytes());
            return;
        }
    };
    out.push(byte);
}

/// Read up to `max` digits in `radix`, returning the value and how many were read.
fn read_digits(chars: &mut Peekable<Chars<'_>>, radix: u32, max: usize) -> (u32, usize) {
    let mut value = 0u32;
    let mut n = 0;
    while n < max {
        match chars.peek().and_then(|c| c.to_digit(radix)) {
            Some(d) => {
                value = value * radix + d;
                chars.next();
                n += 1;
            }
            None => break,
        }
    }
    (value, n)
}

/// Decode one side of an `application/x-www-form-urlencoded` pair: `+` is a
/// space and `%XX` a byte. Text that does not decode to UTF-8 is kept as
/// written rather than mangled.
pub(crate) fn form_decode(s: &str) -> String {
    let spaced = s.replace('+', " ");
    percent_encoding::percent_decode_str(&spaced)
        .decode_utf8()
        .map(|d| d.into_owned())
        .unwrap_or(spaced)
}

/// Percent-encode a value for use in a query string or form body.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn header_lookup<'a>(headers: &'a [KeyValue], name: &str) -> Option<&'a KeyValue> {
    headers.iter().find(|h| h.key.eq_ignore_ascii_case(name))
}

/// A readable name for the request, taken from the URL's last path segment.
///
/// The host is deliberately not used: a request called "api.test" says which
/// server it hit but nothing about what it does.
fn name_from_url(url: &str) -> String {
    let without_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let path = without_scheme.split(['?', '#']).next().unwrap_or("");
    let after_host = match path.split_once('/') {
        Some((_, p)) => p,
        None => "",
    };
    match after_host.trim_end_matches('/').rsplit('/').next() {
        Some(seg) if !seg.is_empty() => seg.to_string(),
        _ => "Imported request".to_string(),
    }
}

/// Parse a `curl` command line into a request.
pub fn parse_curl(input: &str) -> Result<ImportedCurl, String> {
    let mut tokens = tokenize(input);
    if tokens.is_empty() {
        return Err("There is no command here to import.".to_string());
    }
    // `curl.exe` is how Windows users invoke it — PowerShell aliases bare
    // `curl` to Invoke-WebRequest — and a full path is common in copied
    // commands too.
    let first = tokens[0].trim();
    let is_curl = std::path::Path::new(first)
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.eq_ignore_ascii_case("curl") || f.eq_ignore_ascii_case("curl.exe"));
    let start = usize::from(is_curl);
    if start == 0 && !tokens[0].starts_with('-') && !tokens[0].contains("://") {
        return Err(format!(
            "This does not look like a curl command; it starts with \"{}\".",
            tokens[0]
        ));
    }

    let mut warnings = Vec::new();
    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<KeyValue> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut form_parts: Vec<MultipartPart> = Vec::new();
    let mut auth = Auth::Inherit;
    let mut verify_tls = true;
    let mut data_as_query = false;
    let mut json = false;

    let mut i = start;
    while i < tokens.len() {
        // curl lets short options share a word: `-sSL` is three flags and
        // `-XPOST` is -X with its value attached. Split them apart in place,
        // so the match below only ever sees one flag at a time.
        if let Some(split) = split_short_cluster(&tokens[i]) {
            tokens.splice(i..=i, split);
        }
        let tok = tokens[i].clone();
        // --flag=value is the same as --flag value.
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (tok.clone(), None),
        };
        let take = |i: &mut usize| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            *i += 1;
            tokens.get(*i).cloned()
        };

        match flag.as_str() {
            "-X" | "--request" => method = take(&mut i),
            "--url" => url = take(&mut i),
            "-H" | "--header" => {
                if let Some(h) = take(&mut i) {
                    match h.split_once(':') {
                        Some((k, v)) => headers.push(KeyValue {
                            key: k.trim().to_string(),
                            value: v.trim().to_string(),
                            enabled: true,
                            description: None,
                        }),
                        None => warnings.push(format!("Ignored a header without a colon: {h}")),
                    }
                }
            }
            // `--data-raw` exists precisely so a leading `@` is sent
            // literally rather than read as a filename.
            "--data-raw" => {
                if let Some(d) = take(&mut i) {
                    data.push(d);
                }
            }
            // --json is --data-binary plus JSON Content-Type and Accept
            // headers, which are added after the loop unless already set.
            "-d" | "--data" | "--data-binary" | "--data-ascii" | "--json" => {
                json |= flag == "--json";
                if let Some(d) = take(&mut i) {
                    match d.strip_prefix('@') {
                        Some(path) => warnings.push(format!(
                            "The body was read from the file {path}, which was not imported."
                        )),
                        None => data.push(d),
                    }
                }
            }
            "--data-urlencode" => {
                if let Some(d) = take(&mut i) {
                    // curl's own rule, keyed on whichever of `=` and `@` comes
                    // first: name=value encodes only the value, a leading `=`
                    // means there is no name, and `@` reads a file. A bare
                    // value encodes whole.
                    match d.find(['=', '@']).map(|at| d.split_at(at)) {
                        Some((_, rest)) if rest.starts_with('@') => warnings.push(format!(
                            "The body was read from the file {}, which was not imported.",
                            &rest[1..]
                        )),
                        Some(("", rest)) => data.push(url_encode(&rest[1..])),
                        Some((k, rest)) => data.push(format!("{k}={}", url_encode(&rest[1..]))),
                        None => data.push(url_encode(&d)),
                    }
                }
            }
            "-F" | "--form" => {
                if let Some(f) = take(&mut i) {
                    match f.split_once('=') {
                        Some((k, v)) if v.starts_with('@') || v.starts_with('<') => {
                            warnings.push(format!(
                                "The form field \"{k}\" uploaded a file, which was not imported."
                            ));
                        }
                        Some((k, v)) => {
                            // `;type=` after the value is the part's own
                            // Content-Type, not part of the value.
                            let (value, content_type) = match v.split_once(";type=") {
                                Some((val, ct)) => {
                                    let ct = ct.split(';').next().unwrap_or_default().trim();
                                    (val, Some(ct.to_string()))
                                }
                                None => (v, None),
                            };
                            form_parts.push(MultipartPart {
                                key: k.to_string(),
                                kind: MultipartKind::Text,
                                value: value.to_string(),
                                enabled: true,
                                content_type,
                            });
                        }
                        None => warnings.push(format!("Ignored a form field without a value: {f}")),
                    }
                }
            }
            "-u" | "--user" => {
                if let Some(cred) = take(&mut i) {
                    let (user, pass) = cred.split_once(':').unwrap_or((cred.as_str(), ""));
                    auth = Auth::Basic {
                        username: user.to_string(),
                        password: pass.to_string(),
                    };
                }
            }
            "-b" | "--cookie" => {
                if let Some(c) = take(&mut i) {
                    headers.push(KeyValue {
                        key: "Cookie".to_string(),
                        value: c,
                        enabled: true,
                        description: None,
                    });
                }
            }
            "-G" | "--get" => data_as_query = true,
            "-k" | "--insecure" => verify_tls = false,
            // Transport and output options that do not describe the request.
            "--compressed" | "-s" | "--silent" | "-L" | "--location" | "-i" | "--include"
            | "-v" | "--verbose" | "-S" | "--show-error" | "--http1.1" | "--http2" | "-f"
            | "--fail" | "-#" | "--progress-bar" => {}
            "-o" | "--output" | "-A" | "--user-agent" | "-e" | "--referer" | "-m"
            | "--max-time" | "--connect-timeout" | "--retry" | "--proxy" | "-x" => {
                // These take a value; consume it so it is not read as the URL.
                let value = take(&mut i);
                if flag == "-A" || flag == "--user-agent" {
                    if let Some(ua) = value {
                        headers.push(KeyValue {
                            key: "User-Agent".to_string(),
                            value: ua,
                            enabled: true,
                            description: None,
                        });
                    }
                } else if flag == "-e" || flag == "--referer" {
                    if let Some(r) = value {
                        headers.push(KeyValue {
                            key: "Referer".to_string(),
                            value: r,
                            enabled: true,
                            description: None,
                        });
                    }
                } else {
                    warnings.push(format!("Ignored {flag}, which Swarmo sets per request."));
                }
            }
            // Unsupported, but its value must still be consumed so it is not
            // read as the URL.
            other if takes_value(other) => {
                take(&mut i);
                warnings.push(format!("Ignored the unsupported option {other}."));
            }
            other if other.starts_with('-') && other.len() > 1 => {
                warnings.push(format!("Ignored the unsupported option {other}."));
            }
            _ => {
                if url.is_none() {
                    url = Some(tok);
                } else {
                    warnings.push(format!("Ignored the extra argument \"{tok}\"."));
                }
            }
        }
        i += 1;
    }

    let Some(mut url) = url else {
        return Err("No URL was found in that command.".to_string());
    };

    let joined = data.join("&");

    // -G moves the data into the query string instead of the body.
    if data_as_query && !joined.is_empty() {
        let sep = if url.contains('?') { '&' } else { '?' };
        url = format!("{url}{sep}{joined}");
        data.clear();
    }
    let joined = if data_as_query { String::new() } else { joined };

    if json {
        for key in ["Content-Type", "Accept"] {
            if header_lookup(&headers, key).is_none() {
                headers.push(KeyValue {
                    key: key.to_string(),
                    value: "application/json".to_string(),
                    enabled: true,
                    description: None,
                });
            }
        }
    }

    let content_type = header_lookup(&headers, "content-type").map(|h| h.value.clone());

    let body = if !form_parts.is_empty() {
        Body::Multipart { parts: form_parts }
    } else if joined.is_empty() {
        Body::None
    } else {
        let ct = content_type
            .clone()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ct.contains("json") || looks_like_json(&joined) {
            Body::Json { text: joined }
        } else if ct.contains("x-www-form-urlencoded") {
            // The command line holds the body already encoded, and a form
            // body is encoded again at send time, so fields are stored
            // decoded — otherwise `%40` would go out as `%2540`.
            Body::Form {
                fields: joined
                    .split('&')
                    .filter(|p| !p.is_empty())
                    .map(|pair| {
                        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                        KeyValue {
                            key: form_decode(k),
                            value: form_decode(v),
                            enabled: true,
                            description: None,
                        }
                    })
                    .collect(),
            }
        } else {
            // With no Content-Type, curl labels -d data as a form but sends
            // the bytes verbatim. Text with that type does the same; splitting
            // it into fields would not, since a body with no `=` or a literal
            // space changes on the way back out.
            Body::Text {
                text: joined,
                content_type: content_type
                    .or_else(|| Some("application/x-www-form-urlencoded".to_string())),
            }
        }
    };

    // Explicit -X wins; otherwise a body implies POST, as curl itself does.
    let method = method.unwrap_or_else(|| {
        if matches!(body, Body::None) {
            "GET".to_string()
        } else {
            "POST".to_string()
        }
    });

    let mut def = RequestDef::new(name_from_url(&url));
    def.method = method.to_uppercase();
    def.url = url;
    def.headers = headers;
    def.auth = auth;
    def.body = body;
    def.settings.verify_tls = verify_tls;

    Ok(ImportedCurl { def, warnings })
}

fn looks_like_json(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

/// curl's short options that take a value.
const SHORT_WITH_VALUE: &str = "AbcCdDeEFHKmoPQrtTuUwxXyYz";

/// Long options that take a value and that the match in [`parse_curl`] does
/// not otherwise handle.
const LONG_WITH_VALUE: &[&str] = &[
    "--abstract-unix-socket",
    "--alt-svc",
    "--aws-sigv4",
    "--cacert",
    "--capath",
    "--cert",
    "--cert-type",
    "--ciphers",
    "--config",
    "--connect-to",
    "--continue-at",
    "--cookie-jar",
    "--crlfile",
    "--doh-url",
    "--dns-servers",
    "--dump-header",
    "--etag-compare",
    "--etag-save",
    "--expect100-timeout",
    "--form-string",
    "--ftp-port",
    "--happy-eyeballs-timeout-ms",
    "--hsts",
    "--interface",
    "--keepalive-time",
    "--key",
    "--key-type",
    "--limit-rate",
    "--local-port",
    "--login-options",
    "--max-filesize",
    "--max-redirs",
    "--netrc-file",
    "--noproxy",
    "--oauth2-bearer",
    "--output-dir",
    "--pass",
    "--pinnedpubkey",
    "--preproxy",
    "--proto",
    "--proto-default",
    "--proto-redir",
    "--proxy-cacert",
    "--proxy-cert",
    "--proxy-header",
    "--proxy-key",
    "--proxy-user",
    "--quote",
    "--range",
    "--request-target",
    "--resolve",
    "--retry-delay",
    "--retry-max-time",
    "--socks4",
    "--socks4a",
    "--socks5",
    "--socks5-hostname",
    "--speed-limit",
    "--speed-time",
    "--stderr",
    "--telnet-option",
    "--time-cond",
    "--tls-max",
    "--tls13-ciphers",
    "--trace",
    "--trace-ascii",
    "--unix-socket",
    "--upload-file",
    "--url-query",
    "--variable",
    "--write-out",
];

fn takes_value(flag: &str) -> bool {
    match flag.strip_prefix('-') {
        Some(short) if short.len() == 1 => SHORT_WITH_VALUE.contains(short),
        _ => LONG_WITH_VALUE.contains(&flag),
    }
}

/// Split a word of clustered short options into one word per flag, with an
/// attached value as a word of its own: `-sSL` becomes `-s -S -L`, and
/// `-kXPOST` becomes `-k -X POST`. `None` when there is nothing to split.
fn split_short_cluster(tok: &str) -> Option<Vec<String>> {
    let body = tok.strip_prefix('-')?;
    if body.starts_with('-') || body.chars().count() < 2 {
        return None;
    }
    let mut out = Vec::new();
    for (at, c) in body.char_indices() {
        out.push(format!("-{c}"));
        if SHORT_WITH_VALUE.contains(c) {
            let value = &body[at + c.len_utf8()..];
            if !value.is_empty() {
                out.push(value.to_string());
            }
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ImportedCurl {
        parse_curl(s).expect("should parse")
    }

    #[test]
    fn quoting_survives_the_trip() {
        assert_eq!(tokenize("curl 'a b' c"), vec!["curl", "a b", "c"]);
        assert_eq!(tokenize(r#"curl "a b""#), vec!["curl", "a b"]);
        // An escaped quote inside double quotes belongs to the value.
        assert_eq!(
            tokenize(r#"curl "say \"hi\"""#),
            vec!["curl", r#"say "hi""#]
        );
        // Empty quoted strings are still arguments.
        assert_eq!(tokenize("curl '' x"), vec!["curl", "", "x"]);
    }

    #[test]
    fn a_line_continuation_joins_the_command() {
        let out = tokenize("curl 'https://a.test/' \\\n  -H 'accept: text/plain'");
        assert_eq!(
            out,
            vec!["curl", "https://a.test/", "-H", "accept: text/plain"]
        );
    }

    #[test]
    fn ansi_c_quoting_is_understood() {
        // Browsers emit $'…' for headers containing a quote.
        let out = tokenize(r#"curl -H $'x: it\'s'"#);
        assert_eq!(out, vec!["curl", "-H", "x: it's"]);
    }

    #[test]
    fn a_bare_url_is_a_get() {
        let r = parse("curl https://api.test/users");
        assert_eq!(r.def.method, "GET");
        assert_eq!(r.def.url, "https://api.test/users");
        assert!(matches!(r.def.body, Body::None));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn a_body_implies_post_but_an_explicit_method_wins() {
        assert_eq!(parse(r#"curl https://a.test/ -d 'x=1'"#).def.method, "POST");
        assert_eq!(
            parse(r#"curl -X PUT https://a.test/ -d 'x=1'"#).def.method,
            "PUT"
        );
        // Lowercase in, uppercase out.
        assert_eq!(parse("curl -X patch https://a.test/").def.method, "PATCH");
    }

    #[test]
    fn headers_are_split_on_the_first_colon_only() {
        let r =
            parse(r#"curl https://a.test/ -H 'accept: application/json' -H 'x-url: http://x/y'"#);
        assert_eq!(r.def.headers.len(), 2);
        assert_eq!(r.def.headers[1].key, "x-url");
        // A value containing a colon must not be truncated at it.
        assert_eq!(r.def.headers[1].value, "http://x/y");
    }

    #[test]
    fn a_json_body_is_recognised_by_its_content_type_or_its_shape() {
        let r = parse(r#"curl https://a.test/ -H 'content-type: application/json' -d '{"a":1}'"#);
        assert!(matches!(r.def.body, Body::Json { .. }));
        // Even with no content-type, a body that starts with a brace is JSON.
        let r = parse(r#"curl https://a.test/ -d '{"a":1}'"#);
        assert!(matches!(r.def.body, Body::Json { .. }));
    }

    #[test]
    fn a_form_body_becomes_fields() {
        let r = parse(
            r#"curl https://a.test/ -H 'content-type: application/x-www-form-urlencoded' -d 'a=1&b=2'"#,
        );
        match r.def.body {
            Body::Form { fields } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].key, "a");
                assert_eq!(fields[1].value, "2");
            }
            other => panic!("expected a form body, got {other:?}"),
        }
    }

    #[test]
    fn repeated_data_flags_are_joined_the_way_curl_joins_them() {
        let r = parse(r#"curl https://a.test/ -d 'a=1' -d 'b=2'"#);
        match r.def.body {
            Body::Text { text, .. } | Body::Json { text } => assert_eq!(text, "a=1&b=2"),
            Body::Form { fields } => assert_eq!(fields.len(), 2),
            other => panic!("unexpected body {other:?}"),
        }
    }

    #[test]
    fn data_urlencode_encodes_only_the_value() {
        let r = parse(r#"curl https://a.test/ --data-urlencode 'q=a b&c'"#);
        match r.def.body {
            Body::Text { text, .. } => assert_eq!(text, "q=a+b%26c"),
            other => panic!("unexpected body {other:?}"),
        }
    }

    #[test]
    fn dash_g_moves_the_data_into_the_query() {
        let r = parse(r#"curl -G https://a.test/search -d 'q=rust' -d 'page=2'"#);
        assert_eq!(r.def.url, "https://a.test/search?q=rust&page=2");
        assert!(matches!(r.def.body, Body::None));
        // No body, so it stays a GET.
        assert_eq!(r.def.method, "GET");
    }

    #[test]
    fn dash_g_respects_an_existing_query_string() {
        let r = parse(r#"curl -G 'https://a.test/s?a=1' -d 'b=2'"#);
        assert_eq!(r.def.url, "https://a.test/s?a=1&b=2");
    }

    #[test]
    fn credentials_become_basic_auth() {
        let r = parse("curl -u alice:secret https://a.test/");
        match r.def.auth {
            Auth::Basic { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret");
            }
            other => panic!("expected basic auth, got {other:?}"),
        }
    }

    #[test]
    fn insecure_turns_off_verification() {
        assert!(!parse("curl -k https://a.test/").def.settings.verify_tls);
        assert!(parse("curl https://a.test/").def.settings.verify_tls);
    }

    #[test]
    fn a_cookie_becomes_a_header() {
        let r = parse("curl -b 'a=1; b=2' https://a.test/");
        assert_eq!(
            header_lookup(&r.def.headers, "cookie").unwrap().value,
            "a=1; b=2"
        );
    }

    #[test]
    fn flags_written_with_an_equals_sign_work_too() {
        let r = parse("curl --request=DELETE --url=https://a.test/x");
        assert_eq!(r.def.method, "DELETE");
        assert_eq!(r.def.url, "https://a.test/x");
    }

    #[test]
    fn value_taking_flags_do_not_swallow_the_url() {
        // --max-time takes a value; without consuming it, "30" would be read
        // as the URL and the real URL discarded.
        let r = parse("curl --max-time 30 https://a.test/x");
        assert_eq!(r.def.url, "https://a.test/x");
    }

    #[test]
    fn unsupported_options_are_warned_about_rather_than_refused() {
        let r = parse("curl --cert-status https://a.test/");
        assert_eq!(r.def.url, "https://a.test/");
        assert!(
            r.warnings.iter().any(|w| w.contains("--cert-status")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn a_file_upload_is_named_rather_than_silently_dropped() {
        let r = parse("curl https://a.test/ -F 'doc=@/tmp/x.pdf' -F 'note=hi'");
        assert!(
            r.warnings.iter().any(|w| w.contains("doc")),
            "{:?}",
            r.warnings
        );
        match r.def.body {
            Body::Multipart { parts } => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].key, "note");
            }
            other => panic!("expected multipart, got {other:?}"),
        }
    }

    #[test]
    fn a_body_read_from_a_file_is_named_rather_than_silently_dropped() {
        let r = parse("curl https://a.test/ -d @payload.json");
        assert!(matches!(r.def.body, Body::None));
        assert!(r.warnings.iter().any(|w| w.contains("payload.json")));
    }

    #[test]
    fn a_command_copied_from_chrome_imports() {
        // Chrome's "Copy as cURL" shape, continuations and all.
        let cmd = r#"curl 'https://api.test/v1/items?page=2' \
  -H 'accept: application/json' \
  -H 'accept-language: en-GB,en;q=0.9' \
  -H 'content-type: application/json' \
  -H $'sec-ch-ua: "Chromium";v="120", "Not?A_Brand";v="8"' \
  --data-raw '{"name":"it'\''s here"}' \
  --compressed"#;
        let r = parse(cmd);
        assert_eq!(r.def.url, "https://api.test/v1/items?page=2");
        assert_eq!(r.def.method, "POST");
        assert_eq!(r.def.headers.len(), 4);
        assert_eq!(
            header_lookup(&r.def.headers, "sec-ch-ua").unwrap().value,
            r#""Chromium";v="120", "Not?A_Brand";v="8""#
        );
        match &r.def.body {
            Body::Json { text } => assert_eq!(text, r#"{"name":"it's here"}"#),
            other => panic!("expected JSON, got {other:?}"),
        }
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn output_from_the_apps_own_copy_as_curl_reimports() {
        // Kept in step with ui/src/lib/curl.ts by hand: that generator is TS,
        // so this fixture is the contract between the two.
        let generated = "curl -X POST 'https://api.test/v1/items' \\\n  \
                         -H 'content-type: application/json' \\\n  \
                         -H 'authorization: Bearer abc.def' \\\n  \
                         --data-raw '{\"a\":1,\"b\":\"it'\\''s\"}'";
        let r = parse(generated);
        assert_eq!(r.def.method, "POST");
        assert_eq!(r.def.url, "https://api.test/v1/items");
        assert_eq!(r.def.headers.len(), 2);
        match &r.def.body {
            Body::Json { text } => assert_eq!(text, r#"{"a":1,"b":"it's"}"#),
            other => panic!("expected JSON, got {other:?}"),
        }
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_command_with_no_url_is_refused_with_a_reason() {
        let err = parse_curl("curl -X POST").unwrap_err();
        assert!(err.contains("No URL"), "{err}");
    }

    #[test]
    fn curl_exe_and_a_full_path_are_recognised() {
        // PowerShell aliases bare `curl`, so Windows users type curl.exe.
        let a = parse("curl.exe https://x.test/a");
        assert_eq!(a.def.url, "https://x.test/a");
        let b = parse("C:/tools/curl.exe https://x.test/b");
        assert_eq!(b.def.url, "https://x.test/b");
    }

    #[test]
    fn data_raw_keeps_a_leading_at_sign_literal() {
        // That is the whole reason --data-raw exists.
        let c = parse("curl https://x.test/ --data-raw '@handle'");
        match &c.def.body {
            Body::Text { text, .. } | Body::Json { text } => assert_eq!(text, "@handle"),
            other => panic!("unexpected body {other:?}"),
        }
        assert!(
            c.warnings.iter().all(|w| !w.contains("read from the file")),
            "{:?}",
            c.warnings
        );
    }

    #[test]
    fn something_that_is_not_curl_is_refused() {
        assert!(parse_curl("wget https://a.test/").is_err());
        assert!(parse_curl("").is_err());
    }

    #[test]
    fn every_ansi_c_escape_is_decoded() {
        // Chrome writes `!` as !; dropping the backslash sent "u0021".
        let r = parse(r#"curl --data-raw $'{"msg":"hi!"}' https://x.test/"#);
        match &r.def.body {
            Body::Json { text } => assert_eq!(text, r#"{"msg":"hi!"}"#),
            other => panic!("expected JSON, got {other:?}"),
        }
        let out = tokenize(r#"$'\x41\101\U0001F600é\a\b\e\f\v\\\'\"\t'"#);
        assert_eq!(out, vec!["AA\u{1F600}é\x07\x08\x1b\x0c\x0b\\'\"\t"]);
        // \x and octal escapes are bytes, so a UTF-8 sequence survives.
        assert_eq!(tokenize(r#"$'\xe2\x82\xac'"#), vec!["€"]);
        // An escape Bash does not know keeps its backslash, as Bash does.
        assert_eq!(tokenize(r#"$'\q\x'"#), vec![r"\q\x"]);
    }

    #[test]
    fn an_encoded_form_body_is_not_encoded_twice() {
        let r = parse(
            r#"curl https://x.test/ -H 'content-type: application/x-www-form-urlencoded' --data-raw 'email=a%40b.com&q=hello+world'"#,
        );
        match r.def.body {
            Body::Form { fields } => {
                assert_eq!(fields[0].value, "a@b.com");
                assert_eq!(fields[1].value, "hello world");
            }
            other => panic!("expected a form body, got {other:?}"),
        }
    }

    #[test]
    fn data_with_no_content_type_is_sent_as_a_form_the_way_curl_sends_it() {
        // curl labels -d data as a form, and sends the bytes as written.
        let r = parse("curl https://x.test/ -d 'a=1&b=x%20y'");
        match r.def.body {
            Body::Text { text, content_type } => {
                assert_eq!(text, "a=1&b=x%20y");
                assert_eq!(
                    content_type.as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
            }
            other => panic!("expected a text body, got {other:?}"),
        }
        // An explicit type still wins.
        let r = parse("curl https://x.test/ -H 'Content-Type: text/csv' -d 'a,b'");
        match r.def.body {
            Body::Text { content_type, .. } => {
                assert_eq!(content_type.as_deref(), Some("text/csv"))
            }
            other => panic!("expected a text body, got {other:?}"),
        }
    }

    #[test]
    fn short_flags_may_carry_their_value_attached() {
        let r = parse("curl -XPOST -H'x-a: 1' -d'k=v' -ualice:pw https://x.test/");
        assert_eq!(r.def.method, "POST");
        assert_eq!(r.def.url, "https://x.test/");
        assert_eq!(header_lookup(&r.def.headers, "x-a").unwrap().value, "1");
        assert!(matches!(r.def.auth, Auth::Basic { ref username, .. } if username == "alice"));
        match &r.def.body {
            Body::Text { text, .. } => assert_eq!(text, "k=v"),
            other => panic!("unexpected body {other:?}"),
        }
        // Clustered boolean flags, one of them ending in a valued one.
        let r = parse("curl -sSLk -XPUT https://x.test/");
        assert_eq!(r.def.method, "PUT");
        assert!(!r.def.settings.verify_tls);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn unsupported_options_with_values_do_not_become_the_url() {
        for flag in [
            "-m 5",
            "-w '%{http_code}'",
            "-T up.bin",
            "--cacert ca.pem",
            "--resolve a.test:443:127.0.0.1",
            "-D headers.txt",
            "-o out.json",
            "--connect-timeout 3",
            "-A agent/1",
        ] {
            let r = parse(&format!("curl {flag} https://x.test/ok"));
            assert_eq!(r.def.url, "https://x.test/ok", "{flag}");
            assert!(
                r.warnings.iter().all(|w| !w.contains("extra argument")),
                "{flag}: {:?}",
                r.warnings
            );
        }
        let r = parse("curl --cacert ca.pem https://x.test/");
        assert!(r.warnings.iter().any(|w| w.contains("--cacert")));
    }

    #[test]
    fn json_sets_the_body_and_both_headers() {
        let r = parse(r#"curl --json '{"a":1}' https://x.test/"#);
        assert_eq!(r.def.method, "POST");
        assert!(matches!(r.def.body, Body::Json { ref text } if text == r#"{"a":1}"#));
        assert_eq!(
            header_lookup(&r.def.headers, "content-type").unwrap().value,
            "application/json"
        );
        assert_eq!(
            header_lookup(&r.def.headers, "accept").unwrap().value,
            "application/json"
        );
        // A header given explicitly is not doubled or overridden.
        let r = parse(r#"curl --json '{}' -H 'Accept: text/plain' https://x.test/"#);
        let accepts: Vec<_> = r
            .def
            .headers
            .iter()
            .filter(|h| h.key.eq_ignore_ascii_case("accept"))
            .collect();
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0].value, "text/plain");
    }

    #[test]
    fn data_urlencode_with_a_leading_equals_has_no_name() {
        let r = parse("curl https://x.test/ --data-urlencode '=a b'");
        match r.def.body {
            Body::Text { text, .. } => assert_eq!(text, "a+b"),
            other => panic!("unexpected body {other:?}"),
        }
        let r = parse("curl https://x.test/ --data-urlencode 'q@query.txt'");
        assert!(r.warnings.iter().any(|w| w.contains("query.txt")));
    }

    #[test]
    fn a_form_part_type_becomes_its_content_type() {
        let r = parse(r#"curl https://x.test/ -F 'meta={"a":1};type=application/json'"#);
        match r.def.body {
            Body::Multipart { parts } => {
                assert_eq!(parts[0].value, r#"{"a":1}"#);
                assert_eq!(parts[0].content_type.as_deref(), Some("application/json"));
            }
            other => panic!("expected multipart, got {other:?}"),
        }
    }

    #[test]
    fn the_request_is_named_after_its_path() {
        assert_eq!(parse("curl https://a.test/v1/items").def.name, "items");
        assert_eq!(parse("curl https://a.test/").def.name, "Imported request");
    }
}
