//! Random value generators for request templates.
//!
//! These extend `{{variable}}` interpolation with `{{$generator(args)}}` tokens,
//! so one request body can stand in for a whole population of them:
//!
//! ```text
//! { "userId": "{{$uuid}}", "spend": {{$float(0,100,2)}}, "tier": "{{$pick(free,pro)}}" }
//! ```
//!
//! They are evaluated wherever interpolation runs — URL, headers, query params,
//! body, gRPC metadata and message — and, crucially, *per iteration*, so a load
//! test sends different values every time rather than replaying one request.
//!
//! Output can be piped through transforms: `{{$pick(a,b) | base64}}`. That one
//! matters for gRPC, where a `DT_STRING` tensor's value must be base64.

use std::sync::OnceLock;

use base64::Engine as _;
use rand::Rng;
use regex::Regex;

fn generator_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `[^{}]` keeps a generator from swallowing an adjacent `{{var}}`;
    // quoted stretches may hold anything, so `fmt('creative-{}')` still works.
    RE.get_or_init(|| Regex::new(r#"\{\{\s*\$((?:'[^']*'|"[^"]*"|[^{}])*?)\s*\}\}"#).unwrap())
}

/// Expand every `{{$...}}` token, returning the text and any tokens that could
/// not be understood (left in place so the failure is visible rather than
/// silently producing an empty value).
pub fn expand(text: &str) -> (String, Vec<String>) {
    if !text.contains("{{") {
        return (text.to_string(), Vec::new());
    }

    let mut bad: Vec<String> = Vec::new();
    let out = generator_re()
        .replace_all(text, |caps: &regex::Captures| match evaluate(&caps[1]) {
            Ok(v) => v,
            Err(_) => {
                let token = caps[0].to_string();
                if !bad.contains(&token) {
                    bad.push(token.clone());
                }
                token
            }
        })
        .into_owned();

    (out, bad)
}

/// True when the text contains a generator, and so must be re-evaluated on
/// every iteration rather than computed once.
pub fn has_generators(text: &str) -> bool {
    text.contains("{{") && generator_re().is_match(text)
}

/// Most values one `repeat` may produce.
///
/// A generous ceiling rather than a limit anyone should meet: it exists so a
/// mistyped count cannot hang the load generator building one enormous string
/// per iteration.
pub const MAX_REPEAT: usize = 1_000_000;

thread_local! {
    /// The current index within a `repeat`, so `$seq` can read it. Zero when
    /// no repeat is running.
    static REPEAT_INDEX: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Debug)]
pub struct BadGenerator(pub String);

fn evaluate(spec: &str) -> Result<String, BadGenerator> {
    let mut parts = split_pipes(spec).into_iter();
    let head = parts.next().unwrap_or_default();

    let mut value = generate(head.trim())?;
    for t in parts {
        value = transform(t.trim(), &value)?;
    }
    Ok(value)
}

/// Split a spec on `|`, but only at the top level: the pipe in
/// `repeat(3, $seq | fmt('x-{}'))` belongs to the inner spec, not this one.
fn split_pipes(spec: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in spec.char_indices() {
        match c {
            '"' | '\'' => match quote {
                Some(open) if open == c => quote = None,
                Some(_) => {}
                None => quote = Some(c),
            },
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => depth = depth.saturating_sub(1),
            '|' if quote.is_none() && depth == 0 => {
                out.push(&spec[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&spec[start..]);
    out
}

fn generate(spec: &str) -> Result<String, BadGenerator> {
    let (name, args) = match spec.find('(') {
        Some(open) => {
            let close = spec
                .rfind(')')
                .ok_or_else(|| BadGenerator(spec.to_string()))?;
            if close < open {
                return Err(BadGenerator(spec.to_string()));
            }
            (spec[..open].trim(), split_args(&spec[open + 1..close]))
        }
        None => (spec.trim(), Vec::new()),
    };

    let mut rng = rand::thread_rng();

    match name {
        "int" | "randomInt" => {
            let (a, b) = two_numbers(&args, spec)?;
            let (lo, hi) = ordered(a, b);
            Ok(rng
                .gen_range(lo.round() as i64..=hi.round() as i64)
                .to_string())
        }
        "float" | "randomFloat" => {
            let (a, b) = two_numbers(&args, spec)?;
            let (lo, hi) = ordered(a, b);
            let decimals = match args.get(2) {
                Some(d) => d
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| BadGenerator(spec.to_string()))?
                    .min(15),
                None => 6,
            };
            let v = if (hi - lo).abs() < f64::EPSILON {
                lo
            } else {
                rng.gen_range(lo..hi)
            };
            Ok(format!("{v:.decimals$}"))
        }
        "pick" | "oneOf" | "choice" => {
            if args.is_empty() {
                return Err(BadGenerator(spec.to_string()));
            }
            Ok(args[rng.gen_range(0..args.len())].clone())
        }
        "string" | "randomString" => {
            let n = match args.first() {
                Some(a) => a
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| BadGenerator(spec.to_string()))?
                    .min(1_000_000),
                None => 8,
            };
            let charset = args.get(1).map(|s| s.trim()).unwrap_or("alnum");
            let pool: &[u8] = match charset {
                "alnum" => b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
                "alpha" => b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
                "lower" => b"abcdefghijklmnopqrstuvwxyz",
                "digits" => b"0123456789",
                "hex" => b"0123456789abcdef",
                // Lowercase alphanumeric — the usual alphabet for generated
                // ids. `alnum` mixes case, which will not match an id scheme
                // that does not. Pipe through `upper` for the other case.
                "base36" | "loweralnum" => b"0123456789abcdefghijklmnopqrstuvwxyz",
                _ => return Err(BadGenerator(spec.to_string())),
            };
            Ok((0..n)
                .map(|_| pool[rng.gen_range(0..pool.len())] as char)
                .collect())
        }
        // `repeat(527, $float(0,1))` — a whole array's worth of values, each
        // generated independently. Written for tensors: a model input of a few
        // hundred floats is otherwise a few hundred copies of the same token.
        // Like repeat, but every value must differ. Random ids collide more
        // often than intuition says — a thousand draws from ten million hit a
        // duplicate one run in forty — and a cache-comparison test that sends
        // the same id twice is quietly measuring less than it claims.
        "repeatUnique" | "uniqueRepeat" => {
            let count = args
                .first()
                .and_then(|a| a.trim().parse::<usize>().ok())
                .ok_or_else(|| BadGenerator(spec.to_string()))?;
            if count > MAX_REPEAT {
                return Err(BadGenerator(spec.to_string()));
            }
            let inner = args
                .get(1)
                .map(|a| a.trim().trim_start_matches('$'))
                .filter(|a| !a.is_empty())
                .ok_or_else(|| BadGenerator(spec.to_string()))?;
            let sep = args.get(2).map(|s| s.as_str()).unwrap_or(",");

            let mut seen = std::collections::HashSet::with_capacity(count);
            let mut out = String::new();
            let outer = REPEAT_INDEX.with(|cell| cell.get());
            let result = (|| {
                // Bounded retries: a domain smaller than the count must fail
                // loudly rather than loop forever.
                let mut attempts = 0usize;
                let max_attempts = count.saturating_mul(20).max(64);
                while seen.len() < count {
                    attempts += 1;
                    if attempts > max_attempts {
                        return Err(BadGenerator(spec.to_string()));
                    }
                    REPEAT_INDEX.with(|i| i.set(seen.len() as u64));
                    let value = evaluate(inner)?;
                    if seen.insert(value.clone()) {
                        if seen.len() > 1 {
                            out.push_str(sep);
                        }
                        out.push_str(&value);
                    }
                }
                Ok(())
            })();
            REPEAT_INDEX.with(|cell| cell.set(outer));
            result.map(|()| out)
        }
        "repeat" | "times" => {
            let count = args
                .first()
                .and_then(|a| a.trim().parse::<usize>().ok())
                .ok_or_else(|| BadGenerator(spec.to_string()))?;
            if count > MAX_REPEAT {
                return Err(BadGenerator(spec.to_string()));
            }
            let inner = args
                .get(1)
                .map(|a| a.trim().trim_start_matches('$'))
                .filter(|a| !a.is_empty())
                .ok_or_else(|| BadGenerator(spec.to_string()))?;
            // A separator is occasionally useful, but a JSON array is the
            // reason this exists, so a comma is the default.
            let sep = args.get(2).map(|s| s.as_str()).unwrap_or(",");

            let mut out = String::new();
            let outer = REPEAT_INDEX.with(|cell| cell.get());
            let result = (|| {
                for i in 0..count {
                    if i > 0 {
                        out.push_str(sep);
                    }
                    REPEAT_INDEX.with(|cell| cell.set(i as u64));
                    out.push_str(&evaluate(inner)?);
                }
                Ok(())
            })();
            // Restored on every exit so a `$seq` after this repeat — or an
            // enclosing repeat — does not read this loop's last index.
            REPEAT_INDEX.with(|cell| cell.set(outer));
            result.map(|()| out)
        }
        // The position within the surrounding repeat: `$repeat(1000, $seq |
        // fmt('creative-{}'))` numbers its values 0..999, which is the only
        // way to *guarantee* uniqueness rather than gamble on it.
        "seq" | "index" => Ok(REPEAT_INDEX.with(|i| i.get()).to_string()),
        "uuid" => Ok(uuid::Uuid::new_v4().to_string()),
        "bool" => Ok(rng.gen::<bool>().to_string()),
        "epoch" => Ok(now_secs().to_string()),
        "epochMs" => Ok(now_millis().to_string()),
        "now" | "timestamp" => {
            // RFC 3339, which is also the proto3 JSON form of Timestamp.
            let offset: i64 = match args.first() {
                Some(a) => a
                    .trim()
                    .parse()
                    .map_err(|_| BadGenerator(spec.to_string()))?,
                None => 0,
            };
            // RFC 3339 has four-digit years, so anything outside 0000–9999 is
            // refused, as is an offset too large to add at all.
            const YEAR_0: i64 = -62_167_219_200; // 0000-01-01T00:00:00Z
            const YEAR_10000: i64 = 253_402_300_800; // 10000-01-01T00:00:00Z
            (now_secs() as i64)
                .checked_add(offset)
                .filter(|t| (YEAR_0..YEAR_10000).contains(t))
                .map(rfc3339)
                .ok_or_else(|| BadGenerator(spec.to_string()))
        }
        _ => Err(BadGenerator(spec.to_string())),
    }
}

fn transform(name: &str, value: &str) -> Result<String, BadGenerator> {
    // fmt('creative-{}') wraps the value in a fixed shape — the difference
    // between a bare random number and an id a real system would recognise.
    if let Some(args) = name
        .strip_prefix("fmt(")
        .or_else(|| name.strip_prefix("format("))
    {
        let raw = args
            .strip_suffix(')')
            .map(str::trim)
            .ok_or_else(|| BadGenerator(name.to_string()))?;
        // Only one matching outer pair comes off. Trimming every quote from
        // both ends would eat quotes the template means to emit, and it
        // fails silently: the value comes out unquoted and the JSON it was
        // building is invalid with nothing to point at.
        let wrapped = raw.len() >= 2
            && ((raw.starts_with('\'') && raw.ends_with('\''))
                || (raw.starts_with('"') && raw.ends_with('"')));
        let template = if wrapped { &raw[1..raw.len() - 1] } else { raw };
        if !template.contains("{}") {
            return Err(BadGenerator(name.to_string()));
        }
        return Ok(template.replacen("{}", value, 1));
    }
    // pad(6) left-pads with zeros: `$seq | pad(6) | fmt('creative_{}')` gives
    // creative_000000 style ids, matching systems that key on fixed-width
    // numbers. A value already at or past the width passes through unchanged.
    if let Some(args) = name.strip_prefix("pad(") {
        let width = args
            .strip_suffix(')')
            .and_then(|w| w.trim().parse::<usize>().ok())
            .filter(|w| (1..=64).contains(w))
            .ok_or_else(|| BadGenerator(name.to_string()))?;
        let short = width.saturating_sub(value.chars().count());
        return Ok(format!("{}{}", "0".repeat(short), value));
    }
    match name {
        "base64" | "b64" => Ok(base64::engine::general_purpose::STANDARD.encode(value.as_bytes())),
        // The other meaning of "base64": the numbers packed as raw IEEE-754
        // little-endian bytes and *then* encoded — what an embeddings API
        // means when it asks for a base64 vector. Plain `base64` above
        // encodes the digits as text, which decodes back to "0.64,0.81,..."
        // and is not a vector at all.
        "b64f32" | "base64f32" => pack_floats(value, Width::F32),
        "b64f64" | "base64f64" => pack_floats(value, Width::F64),
        // The same idea at 128-bit integer width, for a UInt128[] behind a
        // base64 converter.
        "b64u128" | "base64u128" => pack_u128(value),
        "upper" => Ok(value.to_uppercase()),
        "lower" => Ok(value.to_lowercase()),
        "trim" => Ok(value.trim().to_string()),
        // A JSON string, quotes and escaping included. This is what makes a
        // repeat usable as a JSON array of strings: `[{{$repeat(3, $uuid |
        // quote)}}]` is valid JSON where the bare list is not. The escaping
        // matters as much as the quotes — a value containing a quote or a
        // backslash would otherwise build a body that fails to parse.
        "quote" | "json" => Ok(serde_json::Value::String(value.to_string()).to_string()),
        other => Err(BadGenerator(other.to_string())),
    }
}

/// Which IEEE-754 width a numeric list is packed at.
#[derive(Clone, Copy)]
enum Width {
    F32,
    F64,
}

/// Pack a list of numbers as little-endian IEEE-754 bytes, then base64.
///
/// Little-endian and f32 by default because that is what the ecosystem
/// settled on: x86/ARM are little-endian, and embeddings travel as f32 —
/// 512 dimensions in 2 KB rather than 4 KB.
///
/// The separator is whatever `repeat` used, so commas and whitespace both
/// split. A token that is not a number is an error rather than a silent
/// zero: a vector quietly full of zeros still gets a 200 back, and a
/// benchmark measuring that is measuring nothing.
fn pack_floats(value: &str, width: Width) -> Result<String, BadGenerator> {
    let mut bytes: Vec<u8> = Vec::new();
    for token in value.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        let n: f64 = t
            .parse()
            .map_err(|_| BadGenerator(format!("b64f: \"{t}\" is not a number")))?;
        match width {
            Width::F32 => bytes.extend_from_slice(&(n as f32).to_le_bytes()),
            Width::F64 => bytes.extend_from_slice(&n.to_le_bytes()),
        }
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Pack a list of unsigned integers as little-endian 16-byte values, then
/// base64 — the `UInt128[]` counterpart of [`pack_floats`].
///
/// Little-endian for the same reason: a .NET converter reading these with
/// `MemoryMarshal.Cast<byte, UInt128>` gets native order, and native order is
/// little-endian everywhere this runs.
fn pack_u128(value: &str) -> Result<String, BadGenerator> {
    let mut bytes: Vec<u8> = Vec::new();
    for token in value.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        // Unsigned and exact: a value that does not fit, or is negative, is
        // refused rather than wrapped into a key that silently means
        // something else.
        let n: u128 = t
            .parse()
            .map_err(|_| BadGenerator(format!("b64u128: \"{t}\" is not an unsigned integer")))?;
        bytes.extend_from_slice(&n.to_le_bytes());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Split on commas, honouring quotes.
///
/// Quoting is what lets a choice contain a comma, meaningful spaces, or nothing
/// at all: `pick('a,b', ' c ', '')` is three values. Unquoted arguments are
/// trimmed, since `pick(a, b)` should not depend on how it was spaced.
///
/// **Single and double quotes both work, and inside a JSON body you want single
/// ones** — a double quote there would close the surrounding JSON string and
/// leave you with a parse error rather than a working template.
fn split_args(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() && !raw.contains('"') && !raw.contains('\'') {
        return Vec::new(); // `foo()` takes no arguments
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut was_quoted = false;
    // Depth of nested calls, so `$repeat(3, $float(0,1))` splits into two
    // arguments rather than four.
    let mut depth = 0usize;

    for c in raw.chars() {
        match c {
            '"' | '\'' => match quote {
                // Only the matching quote closes, so 'a"b' keeps its inner quote.
                Some(open) if open == c => {
                    quote = None;
                    if depth > 0 {
                        current.push(c);
                    }
                }
                Some(_) => current.push(c),
                // Inside a nested call the quotes belong to that call's own
                // arguments: `$pick('a,b','c')` has to reach pick with them
                // intact, or it splits into three choices. They are still
                // tracked, so a comma or paren inside them splits nothing here.
                None if depth > 0 => {
                    quote = Some(c);
                    current.push(c);
                }
                None => {
                    // Space between the comma and the quote is formatting, not
                    // part of the value. Without this, `pick('a', 'b')` — which
                    // is how anyone would write it — yields " b", and a
                    // categorical field silently gets a value no model has
                    // ever seen.
                    if current.trim().is_empty() {
                        current.clear();
                    }
                    quote = Some(c);
                    was_quoted = true;
                }
            },
            '(' if quote.is_none() => {
                depth += 1;
                current.push(c);
            }
            ')' if quote.is_none() => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if quote.is_none() && depth == 0 => {
                out.push(finish_arg(&current, was_quoted));
                current.clear();
                was_quoted = false;
            }
            // Likewise the space after a closing quote, as in `pick( 'x' )`.
            _ if quote.is_none() && was_quoted && c.is_whitespace() => {}
            _ => current.push(c),
        }
    }
    out.push(finish_arg(&current, was_quoted));
    out
}

fn finish_arg(raw: &str, was_quoted: bool) -> String {
    if was_quoted {
        raw.to_string()
    } else {
        raw.trim().to_string()
    }
}

fn two_numbers(args: &[String], spec: &str) -> Result<(f64, f64), BadGenerator> {
    if args.len() < 2 {
        return Err(BadGenerator(spec.to_string()));
    }
    let a = args[0]
        .trim()
        .parse::<f64>()
        .map_err(|_| BadGenerator(spec.to_string()))?;
    let b = args[1]
        .trim()
        .parse::<f64>()
        .map_err(|_| BadGenerator(spec.to_string()))?;
    // "nan" and "inf" parse as f64 but panic inside rand's range sampling;
    // so does a span too wide to represent. Refuse them as bad arguments.
    if !a.is_finite() || !b.is_finite() || !(b - a).is_finite() {
        return Err(BadGenerator(spec.to_string()));
    }
    Ok((a, b))
}

fn ordered(a: f64, b: f64) -> (f64, f64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Minimal civil-time conversion, so this crate stays dependency-light.
fn rfc3339(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );

    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mth <= 2 { y + 1 } else { y };

    format!("{year:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> String {
        let (out, bad) = expand(text);
        assert!(bad.is_empty(), "unexpected bad tokens in {text}: {bad:?}");
        out
    }

    #[test]
    fn int_stays_within_range() {
        for _ in 0..200 {
            let v: i64 = one("{{$int(1,10)}}").parse().unwrap();
            assert!((1..=10).contains(&v), "{v}");
        }
    }

    #[test]
    fn int_bounds_may_be_given_either_way_round() {
        for _ in 0..50 {
            let v: i64 = one("{{$int(10,1)}}").parse().unwrap();
            assert!((1..=10).contains(&v));
        }
    }

    #[test]
    fn a_single_point_range_is_that_point() {
        assert_eq!(one("{{$int(7,7)}}"), "7");
        assert_eq!(one("{{$float(2,2,1)}}"), "2.0");
    }

    #[test]
    fn float_honours_the_decimal_count() {
        let v = one("{{$float(0,1,3)}}");
        assert_eq!(v.split('.').nth(1).unwrap().len(), 3, "{v}");
        let f: f64 = v.parse().unwrap();
        assert!((0.0..=1.0).contains(&f));
    }

    #[test]
    fn pick_chooses_from_the_list_and_eventually_varies() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..300 {
            let v = one("{{$pick(alpha,beta,gamma)}}");
            assert!(["alpha", "beta", "gamma"].contains(&v.as_str()), "{v}");
            seen.insert(v);
        }
        assert_eq!(seen.len(), 3, "all three should appear across 300 draws");
    }

    #[test]
    fn quoted_choices_may_contain_commas_and_spaces() {
        for _ in 0..50 {
            let v = one(r#"{{$pick("a,b"," c ")}}"#);
            assert!(v == "a,b" || v == " c ", "{v:?}");
        }
    }

    #[test]
    fn single_quotes_work_too_so_templates_can_live_inside_json() {
        // A double quote here would terminate the surrounding JSON string, so
        // single quotes are the usable form in a request body.
        for _ in 0..50 {
            let v = one("{{$pick('a,b',' c ')}}");
            assert!(v == "a,b" || v == " c ", "{v:?}");
        }
    }

    #[test]
    fn an_empty_choice_can_be_expressed() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..300 {
            seen.insert(one("{{$pick('BD','IN','')}}"));
        }
        assert!(seen.contains(""), "an empty option should be selectable");
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn the_other_quote_character_survives_inside_a_quoted_choice() {
        assert_eq!(one(r#"{{$pick('say "hi"')}}"#), r#"say "hi""#);
        assert_eq!(one("{{$pick(\"it's\")}}"), "it's");
    }

    #[test]
    fn an_empty_choice_is_still_base64_safe() {
        // base64 of nothing is nothing, which is a valid JSON string value.
        for _ in 0..50 {
            let v = one("{{$pick('','BD') | base64}}");
            assert!(v.is_empty() || v == "QkQ=", "{v:?}");
        }
    }

    #[test]
    fn string_length_and_charsets() {
        assert_eq!(one("{{$string(12)}}").len(), 12);
        assert_eq!(one("{{$string}}").len(), 8, "default length");
        assert!(one("{{$string(20,digits)}}")
            .chars()
            .all(|c| c.is_ascii_digit()));
        assert!(one("{{$string(20,hex)}}")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(one("{{$string(20,lower)}}")
            .chars()
            .all(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn uuid_and_bool_and_epoch() {
        assert_eq!(one("{{$uuid}}").len(), 36);
        assert!(["true", "false"].contains(&one("{{$bool}}").as_str()));
        assert!(one("{{$epoch}}").parse::<u64>().unwrap() > 1_600_000_000);
        assert!(one("{{$epochMs}}").parse::<u64>().unwrap() > 1_600_000_000_000);
    }

    #[test]
    fn now_is_rfc3339_and_offsets_apply() {
        let t = one("{{$now}}");
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z') && t.contains('T'), "{t}");
        // A known instant, so the date maths is actually checked.
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn an_offset_past_the_four_digit_years_is_refused_not_a_panic() {
        // The bounds the check uses really are the ends of RFC 3339's range.
        assert_eq!(rfc3339(-62_167_219_200), "0000-01-01T00:00:00Z");
        assert_eq!(rfc3339(253_402_300_799), "9999-12-31T23:59:59Z");
        for bad in [
            "{{$now(9223372036854775807)}}",
            "{{$now(-9223372036854775808)}}",
            "{{$now(300000000000)}}",
            "{{$now(-100000000000)}}",
        ] {
            let (out, errs) = expand(bad);
            assert_eq!(&out, bad, "left in place");
            assert_eq!(errs.len(), 1, "{bad} should be reported");
        }
    }

    #[test]
    fn quotes_inside_a_nested_call_reach_that_call() {
        // Stripping them at the outer level turned two choices into three.
        for _ in 0..50 {
            let (out, bad) = expand("{{$repeat(4, $pick('a,b','c'), '|')}}");
            assert!(bad.is_empty(), "{bad:?}");
            let parts: Vec<&str> = out.split('|').collect();
            assert_eq!(parts.len(), 4, "{out}");
            assert!(parts.iter().all(|p| *p == "a,b" || *p == "c"), "{out}");
        }
        // A quoted paren inside the nested call does not end it early.
        assert_eq!(one("{{$repeat(2, $pick(')'), '')}}"), "))");
    }

    #[test]
    fn transforms_pipe_onto_the_value() {
        assert_eq!(one("{{$pick(bd) | base64}}"), "YmQ=");
        assert_eq!(one("{{$pick(bd) | upper}}"), "BD");
        assert_eq!(one("{{$pick(BD) | lower}}"), "bd");
        // Chained.
        assert_eq!(one("{{$pick(bd) | upper | base64}}"), "QkQ=");
    }

    #[test]
    fn several_tokens_in_one_string_are_all_expanded_independently() {
        let out = one("{{$int(5,5)}}-{{$int(9,9)}}-{{$pick(x)}}");
        assert_eq!(out, "5-9-x");
    }

    #[test]
    fn unknown_generators_are_reported_and_left_in_place() {
        let (out, bad) = expand("{{$nope(1)}}");
        assert_eq!(out, "{{$nope(1)}}", "the token stays visible");
        assert_eq!(bad, vec!["{{$nope(1)}}".to_string()]);
    }

    #[test]
    fn spacing_around_a_quoted_choice_is_not_part_of_it() {
        // Writing the list with spaces after the commas is the natural way to
        // write it, and used to produce values with a leading space — which a
        // model treats as a category it has never seen, silently.
        for spec in [
            "{{$pick('only')}}",
            "{{$pick( 'only' )}}",
            "{{$pick(  'only'  )}}",
        ] {
            let (out, unresolved) = expand(spec);
            assert!(unresolved.is_empty(), "{spec}: {unresolved:?}");
            assert_eq!(out, "only", "{spec}");
        }

        // Every choice in a spaced-out list, not just the first.
        for _ in 0..50 {
            let (out, _) = expand("{{$pick('alpha', 'beta', 'gamma')}}");
            assert!(
                ["alpha", "beta", "gamma"].contains(&out.as_str()),
                "got {out:?}"
            );
        }
    }

    #[test]
    fn spacing_inside_a_quoted_choice_is_kept() {
        // Only the space *outside* the quotes is formatting; a value that
        // genuinely contains spaces must survive intact.
        let (out, _) = expand("{{$pick('New York')}}");
        assert_eq!(out, "New York");
        let (padded, _) = expand("{{$pick(' padded ')}}");
        assert_eq!(padded, " padded ");
    }

    #[test]
    fn repeat_produces_a_whole_array_of_values() {
        let (out, unresolved) = expand("[{{$repeat(527, $float(0,1,4))}}]");
        assert!(unresolved.is_empty(), "{unresolved:?}");

        // It has to be valid JSON, since that is the only reason it exists.
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON array");
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 527);
        assert!(arr.iter().all(|n| {
            let f = n.as_f64().unwrap();
            (0.0..=1.0).contains(&f)
        }));

        // Independently generated, not one value copied 527 times.
        let distinct: std::collections::HashSet<String> =
            arr.iter().map(|n| n.to_string()).collect();
        assert!(
            distinct.len() > 400,
            "only {} distinct values",
            distinct.len()
        );
    }

    #[test]
    fn repeat_composes_with_any_generator() {
        let (out, _) = expand("{{$repeat(5, $pick('a','b'))}}");
        let parts: Vec<&str> = out.split(',').collect();
        assert_eq!(parts.len(), 5);
        assert!(parts.iter().all(|p| *p == "a" || *p == "b"), "{out}");

        let (ints, _) = expand("{{$repeat(4, $int(10,20))}}");
        for p in ints.split(',') {
            let n: i64 = p.parse().expect("an integer");
            assert!((10..=20).contains(&n));
        }

        // A generator that takes no arguments works too.
        let (ids, _) = expand("{{$repeat(3, $uuid)}}");
        assert_eq!(ids.split(',').count(), 3);
    }

    #[test]
    fn repeat_accepts_its_own_separator() {
        let (out, _) = expand("{{$repeat(3, $int(1,1), ' ')}}");
        assert_eq!(out, "1 1 1");
    }

    #[test]
    fn a_nested_call_does_not_confuse_argument_splitting() {
        // `$float(0,1,4)` contains commas; without paren tracking they would
        // be read as further arguments to repeat.
        let (out, unresolved) = expand("{{$repeat(2, $float(0,1,4))}}");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(out.split(',').count(), 2, "{out}");
    }

    #[test]
    fn fmt_wraps_the_value_in_a_template() {
        assert_eq!(one("{{$int(42,42) | fmt('creative-{}')}}"), "creative-42");
        assert_eq!(one("{{$int(7,7) | format('id_{}')}}"), "id_7");
        // Chains with other transforms.
        assert_eq!(one("{{$pick(bd) | upper | fmt('[{}]')}}"), "[BD]");
    }

    #[test]
    fn non_finite_bounds_are_refused_rather_than_panicking() {
        // "nan" and "inf" parse as f64; rand's range sampling panics on them.
        for bad in [
            "{{$int(nan,5)}}",
            "{{$int(1,inf)}}",
            "{{$float(0,inf)}}",
            "{{$float(-1e308,1e308)}}",
        ] {
            let (out, errs) = expand(bad);
            assert_eq!(&out, bad, "left in place");
            assert!(!errs.is_empty(), "{bad} should be reported");
        }
    }

    #[test]
    fn a_repeat_does_not_leak_its_index_into_what_follows() {
        // The doc promises `$seq` is zero outside a repeat; the loop used to
        // leave its last index behind.
        let (out, _) = expand("{{$repeat(3, $seq)}}|{{$seq}}");
        assert_eq!(out, "0,1,2|0");
    }

    #[test]
    fn base64_of_floats_packs_the_bytes_not_the_digits() {
        use base64::Engine as _;

        // Plain base64 encodes the text, which is a different thing entirely
        // and is what an embeddings endpoint rejects.
        let text = one("{{$repeat(2, $float(1,1,1)) | base64}}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap();
        assert_eq!(String::from_utf8(raw).unwrap(), "1.0,1.0");

        // b64f32 packs little-endian float32: two values, eight bytes.
        let packed = one("{{$repeat(2, $float(1,1,4)) | b64f32}}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(packed)
            .unwrap();
        assert_eq!(raw.len(), 8);
        assert_eq!(&raw[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&raw[4..8], &1.0f32.to_le_bytes());

        // f64 is the same list at twice the width.
        let packed = one("{{$repeat(2, $float(1,1,4)) | b64f64}}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(packed)
            .unwrap();
        assert_eq!(raw.len(), 16);
        assert_eq!(&raw[0..8], &1.0f64.to_le_bytes());
    }

    #[test]
    fn a_realistic_embedding_packs_to_the_expected_size() {
        use base64::Engine as _;
        let out = one("{{$repeat(512, $float(0,1,4)) | b64f32}}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(out)
            .unwrap();
        assert_eq!(raw.len(), 512 * 4, "512 float32 values");
    }

    #[test]
    fn packing_something_that_is_not_a_number_is_refused() {
        // Silently packing zeros would still get a 200 back, and the run
        // would be measuring nothing.
        let (out, bad) = expand("{{$repeat(3, $pick(a,b)) | b64f32}}");
        assert_eq!(out, "{{$repeat(3, $pick(a,b)) | b64f32}}");
        assert!(!bad.is_empty());
    }

    #[test]
    fn base36_is_single_case_alphanumeric() {
        let v = one("{{$string(24, base36)}}");
        assert_eq!(v.len(), 24);
        assert!(
            v.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{v}"
        );
        // And uppercase is the same alphabet piped through `upper`.
        let u = one("{{$string(12, base36) | upper}}");
        assert!(
            u.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "{u}"
        );
    }

    #[test]
    fn an_alphanumeric_id_list_is_a_valid_json_array() {
        // The shape for string ids behind a prefix.
        let (out, bad) =
            expand("[{{$repeatUnique(50, $string(6,base36) | fmt('creative_{}') | quote)}}]");
        assert!(bad.is_empty(), "{bad:?}");
        let ids: Vec<String> = serde_json::from_str(&out).expect("valid JSON array");
        assert_eq!(ids.len(), 50);
        assert!(ids
            .iter()
            .all(|i| i.starts_with("creative_") && i.len() == 15));
        let distinct: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(distinct.len(), 50);
    }

    #[test]
    fn u128_keys_pack_sixteen_little_endian_bytes_each() {
        use base64::Engine as _;

        let out = one("{{$repeat(3, $seq) | b64u128}}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(out)
            .unwrap();
        assert_eq!(raw.len(), 48, "three 16-byte keys");
        assert_eq!(&raw[0..16], &0u128.to_le_bytes());
        assert_eq!(&raw[16..32], &1u128.to_le_bytes());
        assert_eq!(&raw[32..48], &2u128.to_le_bytes());
    }

    #[test]
    fn a_key_may_use_the_whole_128_bit_range() {
        use base64::Engine as _;
        let big = u128::MAX.to_string();
        let (out, bad) = expand(&format!("{{{{$pick({big}) | b64u128}}}}"));
        assert!(bad.is_empty(), "{bad:?}");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(out)
            .unwrap();
        assert_eq!(raw, u128::MAX.to_le_bytes());
    }

    #[test]
    fn a_negative_or_non_numeric_key_is_refused() {
        // Wrapping a negative into an unsigned key would send a valid-looking
        // number that means something entirely different.
        for bad_token in ["{{$pick(-1) | b64u128}}", "{{$pick(creative_1) | b64u128}}"] {
            let (out, errs) = expand(bad_token);
            assert_eq!(&out, bad_token);
            assert!(!errs.is_empty(), "{bad_token} should be refused");
        }
    }

    #[test]
    fn pad_left_pads_with_zeros() {
        assert_eq!(one("{{$int(7,7) | pad(6)}}"), "000007");
        // Already wide enough: untouched.
        assert_eq!(one("{{$int(1234567,1234567) | pad(6)}}"), "1234567");
        // The exact shape the padding exists for.
        let (out, unresolved) = expand("{{$repeat(3, $seq | pad(6) | fmt('creative_{}'), ' ')}}");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(out, "creative_000000 creative_000001 creative_000002");
    }

    #[test]
    fn pad_rejects_a_missing_or_absurd_width() {
        for bad in [
            "{{$int(1,1) | pad()}}",
            "{{$int(1,1) | pad(0)}}",
            "{{$int(1,1) | pad(999)}}",
        ] {
            let (out, errs) = expand(bad);
            assert_eq!(&out, bad);
            assert!(!errs.is_empty(), "{bad} should be refused");
        }
    }

    #[test]
    fn quote_makes_a_json_string() {
        assert_eq!(one("{{$pick(bd) | quote}}"), r#""bd""#);
        // Escaping, not merely wrapping: a value holding a quote must not be
        // allowed to end the string early and break the body.
        assert_eq!(one(r#"{{$pick('a"b') | quote}}"#), r#""a\"b""#);
    }

    #[test]
    fn a_quoted_repeat_is_a_valid_json_array() {
        // The shape this exists for: a list of ids a JSON parser accepts.
        let (out, unresolved) =
            expand("[{{$repeat(4, $seq | pad(6) | fmt('creative_{}') | quote)}}]");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        let parsed: Vec<String> = serde_json::from_str(&out).expect("valid JSON array");
        assert_eq!(
            parsed,
            [
                "creative_000000",
                "creative_000001",
                "creative_000002",
                "creative_000003"
            ]
        );
    }

    #[test]
    fn an_unquoted_repeat_is_not_valid_json() {
        // The failure this replaces, pinned so the difference stays visible:
        // bare values are what a server rejects with "invalid start of a value".
        let (out, _) = expand("[{{$repeat(3, $seq | fmt('creative_{}'))}}]");
        assert_eq!(out, "[creative_0,creative_1,creative_2]");
        assert!(serde_json::from_str::<Vec<String>>(&out).is_err());
    }

    #[test]
    fn repeat_unique_quoted_is_also_valid_json() {
        let (out, unresolved) =
            expand("[{{$repeatUnique(50, $int(1,9999) | pad(6) | fmt('creative_{}') | quote)}}]");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        let parsed: Vec<String> = serde_json::from_str(&out).expect("valid JSON array");
        assert_eq!(parsed.len(), 50);
        let distinct: std::collections::HashSet<&String> = parsed.iter().collect();
        assert_eq!(distinct.len(), 50, "all distinct");
        assert!(parsed.iter().all(|v| v.starts_with("creative_")));
    }

    #[test]
    fn a_fmt_template_keeps_quotes_of_its_own() {
        // Only the outer pair is stripped, so a template can emit quotes.
        assert_eq!(one(r#"{{$seq | fmt('"id_{}"')}}"#), r#""id_0""#);
        // A plainly quoted template is unwrapped exactly once.
        assert_eq!(one("{{$seq | fmt('id_{}')}}"), "id_0");
        assert_eq!(one(r#"{{$seq | fmt("id_{}")}}"#), "id_0");

        // The template has to be quoted: an unquoted `{}` is indistinguishable
        // from the `{{ }}` that delimits the token itself, so the whole thing
        // is not recognised as a token and survives as literal text. Pinned
        // here because it fails visibly rather than silently, which is the
        // behaviour worth keeping.
        let (out, bad) = expand("{{$seq | fmt(id_{})}}");
        assert_eq!(out, "{{$seq | fmt(id_{})}}");
        assert!(bad.is_empty(), "not even matched as a token");
    }

    #[test]
    fn fmt_without_a_placeholder_is_an_error() {
        let (out, bad) = expand("{{$int(1,1) | fmt('creative')}}");
        assert_eq!(out, "{{$int(1,1) | fmt('creative')}}");
        assert!(!bad.is_empty());
    }

    #[test]
    fn seq_inside_repeat_counts_up_from_zero() {
        let (out, unresolved) = expand("{{$repeat(4, $seq, '-')}}");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(out, "0-1-2-3");
    }

    #[test]
    fn the_unique_id_list_shape_the_whole_feature_exists_for() {
        // "1000 unique values in format creative-xxxxxxx" — sequential form:
        // guaranteed unique by construction.
        let (out, unresolved) =
            expand("[\"{{$repeat(1000, $seq | fmt('creative-{}'), '\",\"')}}\"]");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        let parsed: Vec<String> = serde_json::from_str(&out).expect("valid JSON array");
        assert_eq!(parsed.len(), 1000);
        let distinct: std::collections::HashSet<&String> = parsed.iter().collect();
        assert_eq!(distinct.len(), 1000, "all values unique");
        assert!(
            parsed.iter().all(|v| v.starts_with("creative-")),
            "{:?}",
            &parsed[..3]
        );
        assert_eq!(parsed[0], "creative-0");
        assert_eq!(parsed[999], "creative-999");
    }

    #[test]
    fn repeat_unique_dedups_random_values() {
        // Random form: 500 draws from a domain of 1000 would collide roughly
        // half the time per draw — without dedup this test fails immediately.
        let (out, unresolved) = expand("{{$repeatUnique(500, $int(0,999) | fmt('creative-{}'))}}");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        let parts: Vec<&str> = out.split(',').collect();
        assert_eq!(parts.len(), 500);
        let distinct: std::collections::HashSet<&&str> = parts.iter().collect();
        assert_eq!(distinct.len(), 500);
        assert!(parts.iter().all(|p| p.starts_with("creative-")));
    }

    #[test]
    fn repeat_unique_accepts_its_own_separator() {
        let (out, _) = expand("{{$repeatUnique(3, $seq, ' ')}}");
        assert_eq!(out, "0 1 2");
    }

    #[test]
    fn repeat_unique_refuses_a_domain_smaller_than_the_count() {
        // Ten unique values cannot come out of $int(1,3); this must error
        // rather than spin forever.
        let (out, bad) = expand("{{$repeatUnique(10, $int(1,3))}}");
        assert_eq!(out, "{{$repeatUnique(10, $int(1,3))}}");
        assert!(!bad.is_empty());
    }

    #[test]
    fn repeat_of_one_has_no_separator() {
        let (out, _) = expand("{{$repeat(1, $int(7,7))}}");
        assert_eq!(out, "7");
    }

    #[test]
    fn an_absurd_count_is_refused_rather_than_attempted() {
        // A mistyped count must not have the load generator build a gigabyte
        // string once per iteration.
        let (out, unresolved) = expand("{{$repeat(99999999, $int(0,1))}}");
        assert_eq!(unresolved.len(), 1, "the bad token should be reported");
        assert!(
            out.contains("{{"),
            "the token should be left in place: {out}"
        );
    }

    #[test]
    fn repeat_needs_both_a_count_and_something_to_repeat() {
        for bad in [
            "{{$repeat()}}",
            "{{$repeat(5)}}",
            "{{$repeat(abc, $int(0,1))}}",
            "{{$repeat(3, )}}",
        ] {
            let (_, unresolved) = expand(bad);
            assert_eq!(unresolved.len(), 1, "{bad} should not have resolved");
        }
    }

    #[test]
    fn malformed_arguments_are_reported() {
        for text in [
            "{{$int(a,b)}}",
            "{{$int(1)}}",
            "{{$float(1)}}",
            "{{$string(x)}}",
            "{{$pick()}}",
            "{{$string(4,klingon)}}",
            "{{$pick(a) | nope}}",
        ] {
            let (_, bad) = expand(text);
            assert_eq!(bad.len(), 1, "{text} should be reported");
        }
    }

    #[test]
    fn plain_variables_are_left_alone() {
        let (out, bad) = expand("{{baseUrl}}/x");
        assert_eq!(out, "{{baseUrl}}/x");
        assert!(bad.is_empty());
    }

    #[test]
    fn a_generator_beside_a_variable_does_not_swallow_it() {
        let (out, bad) = expand("{{baseUrl}}/{{$int(3,3)}}/{{other}}");
        assert_eq!(out, "{{baseUrl}}/3/{{other}}");
        assert!(bad.is_empty());
    }

    #[test]
    fn detection_matches_expansion() {
        assert!(has_generators("{{$uuid}}"));
        assert!(has_generators(r#"{"a": "{{$pick(x,y)}}"}"#));
        assert!(!has_generators("{{baseUrl}}"));
        assert!(!has_generators("no braces at all"));
    }

    #[test]
    fn whitespace_inside_the_braces_is_tolerated() {
        assert_eq!(one("{{ $int(4,4) }}"), "4");
        assert_eq!(one("{{$pick( a )}}"), "a");
    }
}
