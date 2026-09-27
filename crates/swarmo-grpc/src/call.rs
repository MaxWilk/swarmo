//! Making a unary call and packaging the result.

use std::time::{Duration, Instant};

use base64::Engine as _;
use prost_reflect::{DynamicMessage, SerializeOptions};
use serde::{Deserialize, Serialize};
use swarmo_core::model_grpc::ResolvedGrpcRequest;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::{Request, Status};

use crate::channel::ChannelPool;
use crate::codec::DynamicCodec;
use crate::descriptors::DescriptorSource;
use crate::error::{GrpcError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcResult {
    /// gRPC status code, 0..=16. 0 is OK.
    pub code: u16,
    /// The canonical name, e.g. "OK" or "NOT_FOUND".
    pub code_name: String,
    /// The status message. Empty when the call succeeded.
    pub status_message: String,
    /// Pretty-printed protobuf-JSON of the response ("" on a non-OK status).
    pub response_json: String,
    /// The same JSON, compact — what scripts and captures see.
    pub response_raw_json: String,
    /// Initial metadata.
    pub headers: Vec<(String, String)>,
    /// Trailing metadata.
    pub trailers: Vec<(String, String)>,
    pub duration_ms: f64,
    pub response_bytes: u64,
    /// Encoded protobuf size of the request message, so throughput tests can
    /// report what was actually sent rather than the JSON it was typed as.
    #[serde(default)]
    pub request_bytes: u64,
    /// "unary", "server_streaming", "client_streaming" or "bidi".
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Every received message as compact JSON, in arrival order. Empty for a
    /// unary call, whose single reply is `response_raw_json`. For a stream,
    /// `response_raw_json` is this list as one JSON array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
    /// Messages received, for a stream. One for a unary reply.
    #[serde(default)]
    pub message_count: u64,
    /// Time to the first message of a stream, when there was one. The figure
    /// a streaming call is usually judged by: the total duration also counts
    /// however long the server chose to keep talking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message_ms: Option<f64>,
}

fn default_kind() -> String {
    "unary".to_string()
}

/// The streaming shape of a method, from its descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Unary,
    ServerStreaming,
    ClientStreaming,
    Bidi,
}

impl CallKind {
    pub fn of(method: &prost_reflect::MethodDescriptor) -> Self {
        match (method.is_client_streaming(), method.is_server_streaming()) {
            (false, false) => CallKind::Unary,
            (false, true) => CallKind::ServerStreaming,
            (true, false) => CallKind::ClientStreaming,
            (true, true) => CallKind::Bidi,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CallKind::Unary => "unary",
            CallKind::ServerStreaming => "server_streaming",
            CallKind::ClientStreaming => "client_streaming",
            CallKind::Bidi => "bidi",
        }
    }

    fn sends_stream(self) -> bool {
        matches!(self, CallKind::ClientStreaming | CallKind::Bidi)
    }
}

impl GrpcResult {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// The canonical name for a gRPC status code.
pub fn code_name(code: u16) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN",
    }
}

/// Perform one unary call.
///
/// A non-OK gRPC status returns `Ok(GrpcResult)` with that code, exactly as an
/// HTTP 500 returns a successful `ExecResult`. Only problems that stop the call
/// from being made or decoded produce `Err`.
pub async fn call_unary(
    channels: &ChannelPool,
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
    slot: usize,
) -> Result<GrpcResult> {
    call_with(channels, descriptors, req, slot, None).await
}

/// Make the call the method's shape demands — unary, server-, client- or
/// bidirectional streaming — from one resolved request.
///
/// The name says "unary" for history's sake; every entry point now dispatches
/// on the descriptor, so a step in a load test or a request in the client can
/// point at a streaming method and simply work.
pub async fn call(
    channels: &ChannelPool,
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
    slot: usize,
) -> Result<GrpcResult> {
    call_with(channels, descriptors, req, slot, None).await
}

/// Parse a request message once, so a load test can reuse it.
///
/// Turning JSON into a protobuf message is not free — a machine-learning
/// tensor can be megabytes of JSON numbers — and a step whose message has no
/// `{{variables}}` produces the identical message every iteration. Parsing it
/// at planning time and cloning it keeps the load generator measuring the
/// server rather than its own JSON parser.
pub fn prepare_message(
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
) -> Result<DynamicMessage> {
    let method = descriptors.method(&req.service, &req.method)?;
    if method.is_client_streaming() {
        // The request side is a *list* of messages; there is no single one
        // to prepare. Callers parse per iteration instead.
        return Err(GrpcError::invalid(
            "a client-streaming method sends a list of messages, not one",
        ));
    }
    parse_message(&method.input(), &req.message_json)
}

/// Parse the request side of a streaming call: a JSON array of messages, or
/// a single object standing in for a list of one.
fn parse_messages(
    input_type: &prost_reflect::MessageDescriptor,
    message_json: &str,
) -> Result<Vec<DynamicMessage>> {
    let text = message_json.trim();
    if !text.starts_with('[') {
        return Ok(vec![parse_message(input_type, text)?]);
    }
    let values: Vec<serde_json::Value> =
        serde_json::from_str(text).map_err(|e| GrpcError::BadMessage {
            message_type: input_type.full_name().to_string(),
            detail: format!("expected a JSON array of messages: {e}"),
        })?;
    values
        .iter()
        .map(|v| {
            DynamicMessage::deserialize(input_type.clone(), v).map_err(|e| GrpcError::BadMessage {
                message_type: input_type.full_name().to_string(),
                detail: e.to_string(),
            })
        })
        .collect()
}

fn parse_message(
    input_type: &prost_reflect::MessageDescriptor,
    message_json: &str,
) -> Result<DynamicMessage> {
    // JSON -> protobuf, applying the proto3 JSON mapping (well-known types,
    // enums by name, 64-bit ints as strings, and so on).
    let text = if message_json.trim().is_empty() {
        "{}"
    } else {
        message_json.trim()
    };
    let mut de = serde_json::Deserializer::from_str(text);
    let msg = DynamicMessage::deserialize(input_type.clone(), &mut de).map_err(|e| {
        GrpcError::BadMessage {
            message_type: input_type.full_name().to_string(),
            detail: e.to_string(),
        }
    })?;
    de.end().map_err(|e| GrpcError::BadMessage {
        message_type: input_type.full_name().to_string(),
        detail: format!("trailing content after the JSON object: {e}"),
    })?;
    Ok(msg)
}

/// As [`call`], but reusing an already-parsed request message when one is
/// supplied (unary and server-streaming calls only; the request side of a
/// client stream is parsed per call).
pub async fn call_unary_with(
    channels: &ChannelPool,
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
    slot: usize,
    prepared: Option<&DynamicMessage>,
) -> Result<GrpcResult> {
    call_with(channels, descriptors, req, slot, prepared).await
}

pub async fn call_with(
    channels: &ChannelPool,
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
    slot: usize,
    prepared: Option<&DynamicMessage>,
) -> Result<GrpcResult> {
    let mut result = call_with_inner(channels, descriptors, req, slot, prepared).await?;
    // Every early return inside — a deadline, an unreachable host — builds
    // its result through `status_result`, which does not know the kind. Stamp
    // it here so a failed stream is still reported as a stream.
    if let Ok(method) = descriptors.method(&req.service, &req.method) {
        result.kind = CallKind::of(&method).name().to_string();
    }
    Ok(result)
}

async fn call_with_inner(
    channels: &ChannelPool,
    descriptors: &DescriptorSource,
    req: &ResolvedGrpcRequest,
    slot: usize,
    prepared: Option<&DynamicMessage>,
) -> Result<GrpcResult> {
    if req.service.trim().is_empty() || req.method.trim().is_empty() {
        return Err(GrpcError::invalid(
            "Pick a service and a method before calling.",
        ));
    }

    let method = descriptors.method(&req.service, &req.method)?;
    let kind = CallKind::of(&method);
    let input_type = method.input();
    let output_type = method.output();

    // The request side: one message, or a list of them for a client stream.
    let request_msgs: Vec<DynamicMessage> = if kind.sends_stream() {
        parse_messages(&input_type, &req.message_json)?
    } else {
        vec![match prepared {
            Some(m) => m.clone(),
            None => parse_message(&input_type, &req.message_json)?,
        }]
    };
    // Costs a length walk, not an encode; prost sums field sizes.
    let request_bytes: u64 = request_msgs
        .iter()
        .map(|m| prost::Message::encoded_len(m) as u64)
        .sum();

    let channel = channels
        .get(&req.address, req.settings.verify_tls, slot)
        .await?;

    // gRPC libraries cap received messages at 4 MB by default, which real
    // payloads exceed more often than people expect. Swarmo's default is more
    // generous and the request can raise or lower it.
    let mut grpc = tonic::client::Grpc::new(channel)
        .max_decoding_message_size(req.settings.max_response_bytes.max(1) as usize);
    let path = http::uri::PathAndQuery::from_maybe_shared(req.path())
        .map_err(|e| GrpcError::invalid(format!("Invalid method path {}: {e}", req.path())))?;

    let timeout = Duration::from_millis(req.settings.timeout_ms.max(1));
    let timeout_header = MetadataValue::try_from(grpc_timeout_value(timeout)).ok();

    let started = Instant::now();

    let deadline_exceeded = |elapsed: Duration| {
        normalize_deadline(
            status_result(
                &Status::deadline_exceeded(format!(
                    "the call exceeded its {}ms deadline",
                    timeout.as_millis()
                )),
                elapsed,
                request_bytes,
            ),
            timeout,
        )
    };

    let ready = tokio::time::timeout(timeout, grpc.ready()).await;
    match ready {
        // Not ready within the deadline is a connectivity problem, not a slow
        // method: an unreachable host looks exactly like this. It goes under
        // UNAVAILABLE with the other connection failures below.
        Err(_) => {
            return Ok(status_result(
                &Status::unavailable(format!(
                    "could not connect to {} within {}ms",
                    req.address,
                    timeout.as_millis()
                )),
                started.elapsed(),
                request_bytes,
            ))
        }
        Ok(Err(e)) => {
            // Connection problems arrive here; report them as UNAVAILABLE so
            // they land in the status histogram the way gRPC intends.
            return Ok(status_result(
                &Status::unavailable(format!("could not connect to {}: {e}", req.address)),
                started.elapsed(),
                request_bytes,
            ));
        }
        Ok(Ok(())) => {}
    }

    let codec = DynamicCodec::new(output_type.clone());
    // Whatever the readiness wait used comes off the call's budget: the
    // deadline is for the whole exchange, not for each hop of it.
    let remaining = timeout.saturating_sub(started.elapsed());

    // Serialize with proto3 JSON conventions.
    let opts = SerializeOptions::new()
        .stringify_64_bit_integers(true)
        .use_proto_field_name(false)
        .skip_default_fields(false);
    let to_json = |m: &DynamicMessage| -> Result<String> {
        let mut compact = Vec::new();
        let mut ser = serde_json::Serializer::new(&mut compact);
        m.serialize_with_options(&mut ser, &opts)
            .map_err(|e| GrpcError::BadResponse(e.to_string()))?;
        String::from_utf8(compact).map_err(|e| GrpcError::BadResponse(e.to_string()))
    };

    // Metadata goes on whichever request shape the method takes.
    let attach = |map: &mut MetadataMap| -> Result<()> {
        apply_metadata(map, &req.metadata)?;
        if let Some(v) = timeout_header.clone() {
            // Set the wire deadline directly rather than via
            // `Request::set_timeout`, which would also engage tonic's own
            // timeout layer; the client-side clock here owns the deadline.
            map.insert(MetadataKey::from_static("grpc-timeout"), v);
        }
        Ok(())
    };

    match kind {
        CallKind::Unary => {
            let mut request = Request::new(request_msgs.into_iter().next().expect("one message"));
            attach(request.metadata_mut())?;
            let call = grpc.unary(request, path, codec);
            let response = match tokio::time::timeout(remaining, call).await {
                Err(_) => return Ok(deadline_exceeded(started.elapsed())),
                Ok(r) => r,
            };
            let elapsed = started.elapsed();
            match response {
                Ok(resp) => {
                    // tonic folds trailers into the response metadata before it
                    // reaches us, so `headers` carries both and `trailers` stays
                    // empty rather than repeating the same pairs.
                    let headers = metadata_pairs(resp.metadata());
                    let message = resp.into_inner();
                    // Measured the same way as request_bytes — the protobuf wire
                    // size — so inbound and outbound throughput are comparable.
                    let response_bytes = prost::Message::encoded_len(&message) as u64;
                    let response_raw_json = to_json(&message)?;
                    let pretty = serde_json::from_str::<serde_json::Value>(&response_raw_json)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| response_raw_json.clone());
                    Ok(GrpcResult {
                        code: 0,
                        code_name: "OK".into(),
                        status_message: String::new(),
                        response_bytes,
                        response_json: pretty,
                        response_raw_json,
                        headers,
                        trailers: Vec::new(),
                        request_bytes,
                        duration_ms: elapsed.as_secs_f64() * 1000.0,
                        kind: kind.name().to_string(),
                        messages: Vec::new(),
                        message_count: 1,
                        first_message_ms: None,
                    })
                }
                Err(status) => Ok(normalize_deadline(
                    status_result(&status, elapsed, request_bytes),
                    timeout,
                )),
            }
        }

        CallKind::ClientStreaming => {
            let mut request = Request::new(futures::stream::iter(request_msgs));
            attach(request.metadata_mut())?;
            let call = grpc.client_streaming(request, path, codec);
            let response = match tokio::time::timeout(remaining, call).await {
                Err(_) => return Ok(deadline_exceeded(started.elapsed())),
                Ok(r) => r,
            };
            let elapsed = started.elapsed();
            match response {
                Ok(resp) => {
                    let headers = metadata_pairs(resp.metadata());
                    let message = resp.into_inner();
                    let response_bytes = prost::Message::encoded_len(&message) as u64;
                    let response_raw_json = to_json(&message)?;
                    let pretty = serde_json::from_str::<serde_json::Value>(&response_raw_json)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| response_raw_json.clone());
                    Ok(GrpcResult {
                        code: 0,
                        code_name: "OK".into(),
                        status_message: String::new(),
                        response_bytes,
                        response_json: pretty,
                        response_raw_json,
                        headers,
                        trailers: Vec::new(),
                        request_bytes,
                        duration_ms: elapsed.as_secs_f64() * 1000.0,
                        kind: kind.name().to_string(),
                        messages: Vec::new(),
                        message_count: 1,
                        first_message_ms: None,
                    })
                }
                Err(status) => Ok(normalize_deadline(
                    status_result(&status, elapsed, request_bytes),
                    timeout,
                )),
            }
        }

        CallKind::ServerStreaming | CallKind::Bidi => {
            // Open the stream, then drain it under what is left of the deadline.
            let opened = if kind == CallKind::Bidi {
                let mut request = Request::new(futures::stream::iter(request_msgs));
                attach(request.metadata_mut())?;
                let call = grpc.streaming(request, path, codec);
                match tokio::time::timeout(remaining, call).await {
                    Err(_) => return Ok(deadline_exceeded(started.elapsed())),
                    Ok(r) => r,
                }
            } else {
                let mut request =
                    Request::new(request_msgs.into_iter().next().expect("one message"));
                attach(request.metadata_mut())?;
                let call = grpc.server_streaming(request, path, codec);
                match tokio::time::timeout(remaining, call).await {
                    Err(_) => return Ok(deadline_exceeded(started.elapsed())),
                    Ok(r) => r,
                }
            };

            let resp = match opened {
                Ok(r) => r,
                Err(status) => {
                    return Ok(normalize_deadline(
                        status_result(&status, started.elapsed(), request_bytes),
                        timeout,
                    ))
                }
            };
            let headers = metadata_pairs(resp.metadata());
            let mut inbound = resp.into_inner();

            let cap = req.settings.stream_max_messages.map(|n| n as usize);
            let mut messages: Vec<String> = Vec::new();
            let mut response_bytes: u64 = 0;
            let mut first_message_ms: Option<f64> = None;
            let mut status_err: Option<Status> = None;
            let mut timed_out = false;
            let mut ended = false;

            loop {
                if cap.is_some_and(|n| messages.len() >= n) {
                    // Enough: dropping `inbound` closes the stream. Reading an
                    // unbounded feed to its end would be reading forever.
                    break;
                }
                let left = timeout.saturating_sub(started.elapsed());
                if left.is_zero() {
                    timed_out = true;
                    break;
                }
                match tokio::time::timeout(left, inbound.message()).await {
                    Err(_) => {
                        timed_out = true;
                        break;
                    }
                    Ok(Ok(Some(m))) => {
                        if first_message_ms.is_none() {
                            first_message_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                        }
                        response_bytes += prost::Message::encoded_len(&m) as u64;
                        messages.push(to_json(&m)?);
                    }
                    Ok(Ok(None)) => {
                        // the server ended the stream
                        ended = true;
                        break;
                    }
                    Ok(Err(status)) => {
                        status_err = Some(status);
                        break;
                    }
                }
            }
            // A stream that ended cleanly has its trailers waiting; they are
            // already read, so this does not touch the network again.
            let trailers = if ended {
                match inbound.trailers().await {
                    Ok(Some(t)) => metadata_pairs(&t),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let elapsed = started.elapsed();
            let message_count = messages.len() as u64;

            // A stream is a list; as JSON it is one array. Pretty for the
            // pane, compact for scripts and captures.
            let response_raw_json = format!("[{}]", messages.join(","));
            let pretty = serde_json::from_str::<serde_json::Value>(&response_raw_json)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| response_raw_json.clone());

            let mut result = if timed_out {
                deadline_exceeded(elapsed)
            } else if let Some(status) = status_err {
                let mut r =
                    normalize_deadline(status_result(&status, elapsed, request_bytes), timeout);
                // A status that ends a stream arrives in the trailers, after
                // the initial headers; keep its metadata there rather than
                // letting the headers below overwrite it.
                r.trailers = std::mem::take(&mut r.headers);
                r
            } else {
                GrpcResult {
                    code: 0,
                    code_name: "OK".into(),
                    status_message: String::new(),
                    response_bytes: 0,
                    response_json: String::new(),
                    response_raw_json: String::new(),
                    headers: Vec::new(),
                    trailers,
                    request_bytes,
                    duration_ms: elapsed.as_secs_f64() * 1000.0,
                    kind: kind.name().to_string(),
                    messages: Vec::new(),
                    message_count: 0,
                    first_message_ms: None,
                }
            };
            // Whatever ended the stream, what arrived before that is kept:
            // a feed that failed after forty messages still delivered forty.
            result.headers = headers;
            result.response_bytes = response_bytes;
            result.response_json = pretty;
            result.response_raw_json = response_raw_json;
            result.kind = kind.name().to_string();
            result.messages = messages;
            result.message_count = message_count;
            result.first_message_ms = first_message_ms;
            Ok(result)
        }
    }
}

/// tonic reports a lapsed `grpc-timeout` as CANCELLED, but the canonical gRPC
/// code for a deadline is DEADLINE_EXCEEDED, and that is what other clients
/// show. Relabel it so status histograms and thresholds stay comparable across
/// tools; a genuine server-side cancellation arrives well before the deadline
/// and is left alone.
fn normalize_deadline(mut result: GrpcResult, timeout: Duration) -> GrpcResult {
    const CANCELLED: u16 = 1;
    const DEADLINE_EXCEEDED: u16 = 4;

    let deadline_ms = timeout.as_secs_f64() * 1000.0;
    if result.code == CANCELLED && result.duration_ms >= deadline_ms * 0.95 {
        result.code = DEADLINE_EXCEEDED;
        result.code_name = code_name(DEADLINE_EXCEEDED).to_string();
        if result.status_message.is_empty() || result.status_message == "Timeout expired" {
            result.status_message = format!("the call exceeded its {deadline_ms:.0}ms deadline");
        }
    }
    result
}

fn status_result(status: &Status, elapsed: Duration, request_bytes: u64) -> GrpcResult {
    let code = status.code() as i32 as u16;
    GrpcResult {
        code,
        code_name: code_name(code).to_string(),
        status_message: status.message().to_string(),
        response_json: String::new(),
        response_raw_json: String::new(),
        headers: metadata_pairs(status.metadata()),
        trailers: Vec::new(),
        request_bytes,
        duration_ms: elapsed.as_secs_f64() * 1000.0,
        response_bytes: 0,
        kind: default_kind(),
        messages: Vec::new(),
        message_count: 0,
        first_message_ms: None,
    }
}

/// The `grpc-timeout` value for a deadline.
///
/// The spec allows at most eight digits, and servers other than tonic's
/// reject anything longer as malformed, failing the call outright. A deadline
/// too long for milliseconds is sent in the coarsest unit that fits.
fn grpc_timeout_value(timeout: Duration) -> String {
    const MAX: u128 = 99_999_999;
    let ms = timeout.as_millis().max(1);
    if ms <= MAX {
        return format!("{ms}m");
    }
    let secs = u128::from(timeout.as_secs());
    if secs <= MAX {
        format!("{secs}S")
    } else if secs / 60 <= MAX {
        format!("{}M", secs / 60)
    } else {
        format!("{}H", (secs / 3600).min(MAX))
    }
}

/// Base64 for `-bin` values. The gRPC spec has senders omit the padding and
/// receivers accept either form, so values copied from elsewhere may lack it.
const BIN_BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// Copy metadata onto the outgoing request. Keys ending in `-bin` carry
/// base64-encoded binary values, per the gRPC wire spec. A key given twice is
/// sent twice: metadata, like HTTP headers, may repeat.
pub(crate) fn apply_metadata(map: &mut MetadataMap, pairs: &[(String, String)]) -> Result<()> {
    for (key, value) in pairs {
        let key = key.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        if key.starts_with("grpc-") {
            // Reserved by the protocol; silently sending them breaks calls.
            return Err(GrpcError::invalid(format!(
                "\"{key}\" is reserved by gRPC and cannot be set as metadata. \
                 Use the timeout setting instead of grpc-timeout."
            )));
        }

        if let Some(stripped) = key.strip_suffix("-bin") {
            let _ = stripped;
            let bytes = BIN_BASE64.decode(value.trim()).map_err(|_| {
                GrpcError::invalid(format!(
                    "Metadata \"{key}\" ends in -bin, so its value must be base64."
                ))
            })?;
            let k = MetadataKey::from_bytes(key.as_bytes())
                .map_err(|_| GrpcError::invalid(format!("Invalid metadata key \"{key}\".")))?;
            map.append_bin(k, MetadataValue::from_bytes(&bytes));
        } else {
            let k: MetadataKey<tonic::metadata::Ascii> = key
                .parse()
                .map_err(|_| GrpcError::invalid(format!("Invalid metadata key \"{key}\".")))?;
            let v: MetadataValue<tonic::metadata::Ascii> = value.parse().map_err(|_| {
                GrpcError::invalid(format!(
                    "Metadata \"{key}\" has a value that cannot be sent as text \
                     (control characters and newlines are not allowed). \
                     Rename the key to end in -bin and base64-encode the value."
                ))
            })?;
            map.append(k, v);
        }
    }
    Ok(())
}

fn metadata_pairs(map: &MetadataMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in map.keys() {
        match key {
            // A key may carry several values; every one is reported.
            tonic::metadata::KeyRef::Ascii(k) => {
                for v in map.get_all(k.as_str()) {
                    out.push((
                        k.as_str().to_string(),
                        v.to_str().unwrap_or("<non-ascii>").to_string(),
                    ));
                }
            }
            tonic::metadata::KeyRef::Binary(k) => {
                for v in map.get_all_bin(k.as_str()) {
                    let encoded = v
                        .to_bytes()
                        .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
                        .unwrap_or_else(|_| "<invalid>".to_string());
                    out.push((k.as_str().to_string(), encoded));
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_names_agree_with_core() {
        // This table names codes coming off the wire; swarmo-core names them
        // for reporting. Two tables, one meaning — so they must not drift.
        for code in 0..=16u16 {
            assert_eq!(
                code_name(code),
                swarmo_core::model::grpc_code_name(code),
                "code {code} is named differently in swarmo-grpc and swarmo-core"
            );
        }
    }

    #[test]
    fn code_names_cover_the_range() {
        assert_eq!(code_name(0), "OK");
        assert_eq!(code_name(5), "NOT_FOUND");
        assert_eq!(code_name(14), "UNAVAILABLE");
        assert_eq!(code_name(99), "UNKNOWN");
    }

    #[test]
    fn ascii_metadata_is_applied() {
        let mut map = MetadataMap::new();
        apply_metadata(&mut map, &[("authorization".into(), "Bearer tok".into())]).unwrap();
        assert_eq!(map.get("authorization").unwrap(), "Bearer tok");
    }

    #[test]
    fn binary_metadata_must_be_base64() {
        let mut map = MetadataMap::new();
        let ok = apply_metadata(&mut map, &[("trace-bin".into(), "aGk=".into())]);
        assert!(ok.is_ok());
        assert!(map.get_bin("trace-bin").is_some());

        let mut map = MetadataMap::new();
        let err =
            apply_metadata(&mut map, &[("trace-bin".into(), "not base64!".into())]).unwrap_err();
        assert!(err.to_string().contains("base64"), "{err}");
    }

    #[test]
    fn reserved_keys_are_refused_with_a_hint() {
        let mut map = MetadataMap::new();
        let err = apply_metadata(&mut map, &[("grpc-timeout".into(), "1S".into())]).unwrap_err();
        assert!(err.to_string().contains("timeout setting"), "{err}");
    }

    #[test]
    fn unsendable_values_explain_the_bin_convention() {
        let mut map = MetadataMap::new();
        // A newline cannot appear in a text metadata value.
        let err =
            apply_metadata(&mut map, &[("x-note".into(), "line1\nline2".into())]).unwrap_err();
        assert!(err.to_string().contains("-bin"), "{err}");
    }

    #[test]
    fn accented_text_is_accepted_as_is() {
        // Latin-1 range bytes are legal in header values, so this must not be
        // rejected: pushing users to -bin here would be wrong.
        let mut map = MetadataMap::new();
        apply_metadata(&mut map, &[("x-note".into(), "café".into())]).unwrap();
        assert!(map.get("x-note").is_some());
    }

    fn cancelled_after(ms: f64) -> GrpcResult {
        GrpcResult {
            code: 1,
            code_name: "CANCELLED".into(),
            status_message: "Timeout expired".into(),
            response_json: String::new(),
            response_raw_json: String::new(),
            headers: vec![],
            trailers: vec![],
            request_bytes: 0,
            duration_ms: ms,
            response_bytes: 0,
            kind: "unary".into(),
            messages: vec![],
            message_count: 0,
            first_message_ms: None,
        }
    }

    #[test]
    fn a_lapsed_deadline_is_relabelled() {
        let r = normalize_deadline(cancelled_after(305.0), Duration::from_millis(300));
        assert_eq!(r.code, 4);
        assert_eq!(r.code_name, "DEADLINE_EXCEEDED");
        assert!(
            r.status_message.contains("deadline"),
            "{}",
            r.status_message
        );
    }

    #[test]
    fn an_early_cancellation_is_left_alone() {
        let r = normalize_deadline(cancelled_after(20.0), Duration::from_millis(300));
        assert_eq!(r.code, 1);
        assert_eq!(r.code_name, "CANCELLED");
    }

    #[test]
    fn other_codes_are_never_touched() {
        let mut base = cancelled_after(999.0);
        base.code = 5;
        base.code_name = "NOT_FOUND".into();
        let r = normalize_deadline(base, Duration::from_millis(300));
        assert_eq!(r.code, 5);
    }

    #[test]
    fn unpadded_binary_metadata_is_accepted() {
        let mut map = MetadataMap::new();
        apply_metadata(&mut map, &[("trace-bin".into(), "aGk".into())]).unwrap();
        assert_eq!(
            map.get_bin("trace-bin")
                .unwrap()
                .to_bytes()
                .unwrap()
                .as_ref(),
            b"hi"
        );
    }

    #[test]
    fn a_repeated_key_sends_every_value() {
        let mut map = MetadataMap::new();
        apply_metadata(
            &mut map,
            &[
                ("x-tag".into(), "a".into()),
                ("x-tag".into(), "b".into()),
                ("t-bin".into(), "AQ==".into()),
                ("t-bin".into(), "Ag==".into()),
            ],
        )
        .unwrap();
        let tags: Vec<&str> = map
            .get_all("x-tag")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(tags, ["a", "b"]);
        assert_eq!(map.get_all_bin("t-bin").iter().count(), 2);
    }

    #[test]
    fn grpc_timeout_never_exceeds_eight_digits() {
        assert_eq!(grpc_timeout_value(Duration::from_millis(0)), "1m");
        assert_eq!(grpc_timeout_value(Duration::from_millis(30_000)), "30000m");
        assert_eq!(
            grpc_timeout_value(Duration::from_millis(99_999_999)),
            "99999999m"
        );
        assert_eq!(
            grpc_timeout_value(Duration::from_millis(100_000_000)),
            "100000S"
        );
        for d in [
            Duration::from_secs(100_000_000),
            Duration::from_secs(u64::MAX / 2),
            Duration::from_millis(u64::MAX),
        ] {
            let v = grpc_timeout_value(d);
            assert!(v.len() <= 9, "{v} is longer than 8 digits plus a unit");
        }
    }

    #[test]
    fn empty_keys_are_skipped() {
        let mut map = MetadataMap::new();
        apply_metadata(&mut map, &[("  ".into(), "v".into())]).unwrap();
        assert_eq!(map.len(), 0);
    }
}
