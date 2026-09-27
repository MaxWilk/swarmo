//! The gRPC server-reflection client: point at an address, get its schema.
//!
//! The reflection service is defined by a handful of small messages, so they
//! are written out here with prost derives rather than pulling a `.proto`
//! through codegen. `ServerReflectionInfo` is bidirectional streaming, but the
//! way we use it is not conversational: every request we intend to send is
//! known up front, so we push them all and then drain the responses.

use prost::Message as _;
use prost_reflect::DescriptorPool;
use tonic::transport::Channel;
use tonic::{Code, Request};

use crate::channel::ChannelPool;
use crate::descriptors::DescriptorSource;
use crate::error::{GrpcError, Result};

// ---------------------------------------------------------------------------
// Wire types (grpc.reflection.v1 / v1alpha share these shapes)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServerReflectionRequest {
    #[prost(string, tag = "1")]
    pub host: String,
    #[prost(oneof = "MessageRequest", tags = "3, 4, 7")]
    pub message_request: Option<MessageRequest>,
}

#[derive(Clone, PartialEq, ::prost::Oneof)]
pub enum MessageRequest {
    #[prost(string, tag = "3")]
    FileByFilename(String),
    #[prost(string, tag = "4")]
    FileContainingSymbol(String),
    #[prost(string, tag = "7")]
    ListServices(String),
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServerReflectionResponse {
    #[prost(string, tag = "1")]
    pub valid_host: String,
    #[prost(oneof = "MessageResponse", tags = "4, 6, 7")]
    pub message_response: Option<MessageResponse>,
}

#[derive(Clone, PartialEq, ::prost::Oneof)]
pub enum MessageResponse {
    #[prost(message, tag = "4")]
    FileDescriptorResponse(FileDescriptorResponse),
    #[prost(message, tag = "6")]
    ListServicesResponse(ListServiceResponse),
    #[prost(message, tag = "7")]
    ErrorResponse(ErrorResponse),
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub file_descriptor_proto: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ListServiceResponse {
    #[prost(message, repeated, tag = "1")]
    pub service: Vec<ServiceResponse>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServiceResponse {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ErrorResponse {
    #[prost(int32, tag = "1")]
    pub error_code: i32,
    #[prost(string, tag = "2")]
    pub error_message: String,
}

const V1_PATH: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
const V1ALPHA_PATH: &str = "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

/// Fetch a schema from a server that has reflection enabled.
///
/// Tries the stable v1 service first and falls back to v1alpha, which many
/// servers still serve exclusively.
/// `metadata` is sent with every reflection request. Authenticated servers
/// reject unauthenticated reflection calls just as they would any other RPC, so
/// the request's own metadata (including whatever its auth setting produced)
/// has to come along or the schema fetch fails with a permission error.
pub async fn load_via_reflection(
    address: &str,
    verify_tls: bool,
    metadata: &[(String, String)],
) -> Result<DescriptorSource> {
    let pool = ChannelPool::new(1);
    let channel = pool.get(address, verify_tls, 0).await?;

    let v1 = fetch(&channel, V1_PATH, metadata).await;
    let (services, protos) = match v1 {
        Ok(v) => v,
        Err(e) if should_fall_back(&e) => {
            tracing::debug!("reflection v1 unavailable ({e}); trying v1alpha");
            fetch(&channel, V1ALPHA_PATH, metadata)
                .await
                .map_err(|alpha_err| {
                    GrpcError::descriptor(format!(
                        "The server at {address} did not answer a reflection request. \
                     Enable the gRPC reflection service on it, or point this request at \
                     .proto files instead.\nv1: {e}\nv1alpha: {alpha_err}"
                    ))
                })?
        }
        Err(e) => return Err(e),
    };

    if services.is_empty() {
        return Err(GrpcError::descriptor(format!(
            "The server at {address} reports no services over reflection."
        )));
    }

    let mut pool = DescriptorPool::new();
    // This resolves inter-file dependencies itself, so the order the server
    // happened to send them in does not matter.
    pool.add_file_descriptor_protos(protos).map_err(|e| {
        GrpcError::descriptor(format!(
            "The schema from {address} could not be assembled: {e}"
        ))
    })?;

    Ok(DescriptorSource::from_pool(
        pool,
        format!("reflection at {address}"),
    ))
}

/// Servers are not required to send a file's imports alongside it, so keep
/// asking for whatever is still missing until the set closes over its own
/// dependencies. Without this, anything importing a well-known type (a
/// `Timestamp` field, say) fails to assemble.
async fn collect_with_dependencies(
    channel: &Channel,
    path: &'static str,
    metadata: &[(String, String)],
    initial: Vec<Vec<u8>>,
) -> Result<Vec<prost_types::FileDescriptorProto>> {
    use std::collections::{HashMap, HashSet};

    const MAX_ROUNDS: usize = 12;

    let mut have: HashMap<String, prost_types::FileDescriptorProto> = HashMap::new();
    let mut pending = initial;
    // Filenames we already asked for, so a server that ignores a request
    // cannot put us in a loop.
    let mut requested: HashSet<String> = HashSet::new();

    for round in 0..MAX_ROUNDS {
        for bytes in pending.drain(..) {
            let fd = prost_types::FileDescriptorProto::decode(bytes.as_slice()).map_err(|e| {
                GrpcError::descriptor(format!("The server sent a descriptor we cannot read: {e}"))
            })?;
            let name = fd.name.clone().unwrap_or_default();
            have.entry(name).or_insert(fd);
        }

        let missing: Vec<String> = have
            .values()
            .flat_map(|fd| fd.dependency.iter().cloned())
            .filter(|dep| !have.contains_key(dep) && !requested.contains(dep))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if missing.is_empty() {
            let mut out: Vec<_> = have.into_values().collect();
            // Stable order keeps cache keys and error messages reproducible.
            out.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(out);
        }

        if round + 1 == MAX_ROUNDS {
            return Err(GrpcError::descriptor(format!(
                "The server's schema still references files it did not supply: {}",
                missing.join(", ")
            )));
        }

        for m in &missing {
            requested.insert(m.clone());
        }
        let requests: Vec<ServerReflectionRequest> = missing
            .iter()
            .map(|name| ServerReflectionRequest {
                host: String::new(),
                message_request: Some(MessageRequest::FileByFilename(name.clone())),
            })
            .collect();

        let responses = round_trip(channel, path, requests, metadata).await?;
        for r in responses {
            match r.message_response {
                Some(MessageResponse::FileDescriptorResponse(fd)) => {
                    pending.extend(fd.file_descriptor_proto)
                }
                Some(MessageResponse::ErrorResponse(err)) => {
                    return Err(GrpcError::descriptor(format!(
                        "The server could not supply an imported file: {} ({})",
                        err.error_message, err.error_code
                    )))
                }
                _ => {}
            }
        }
    }

    Err(GrpcError::descriptor(
        "The server's schema could not be resolved.",
    ))
}

/// True when the failure means "this reflection version is not served here".
fn should_fall_back(e: &GrpcError) -> bool {
    match e {
        GrpcError::Transport(msg) => {
            let m = msg.to_ascii_lowercase();
            m.contains("unimplemented") || m.contains("not found") || m.contains("unknown service")
        }
        _ => false,
    }
}

/// One reflection exchange: list the services, ask for the file containing
/// each, then close over the imports those files reference.
async fn fetch(
    channel: &Channel,
    path: &'static str,
    metadata: &[(String, String)],
) -> Result<(Vec<String>, Vec<prost_types::FileDescriptorProto>)> {
    let services = list_services(channel, path, metadata).await?;

    let requests: Vec<ServerReflectionRequest> = services
        .iter()
        .map(|name| ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::FileContainingSymbol(name.clone())),
        })
        .collect();

    let responses = round_trip(channel, path, requests, metadata).await?;

    let mut files = Vec::new();
    for r in responses {
        match r.message_response {
            Some(MessageResponse::FileDescriptorResponse(fd)) => {
                files.extend(fd.file_descriptor_proto)
            }
            Some(MessageResponse::ErrorResponse(err)) => {
                return Err(GrpcError::descriptor(format!(
                    "The server refused a reflection request: {} ({})",
                    err.error_message, err.error_code
                )))
            }
            _ => {}
        }
    }

    let protos = collect_with_dependencies(channel, path, metadata, files).await?;
    Ok((services, protos))
}

async fn list_services(
    channel: &Channel,
    path: &str,
    metadata: &[(String, String)],
) -> Result<Vec<String>> {
    let responses = round_trip(
        channel,
        path,
        vec![ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        }],
        metadata,
    )
    .await?;

    for r in responses {
        match r.message_response {
            Some(MessageResponse::ListServicesResponse(list)) => {
                return Ok(list
                    .service
                    .into_iter()
                    .map(|s| s.name)
                    .filter(|n| !n.starts_with("grpc.reflection."))
                    .collect())
            }
            Some(MessageResponse::ErrorResponse(err)) => {
                return Err(GrpcError::descriptor(format!(
                    "The server refused to list its services: {} ({})",
                    err.error_message, err.error_code
                )))
            }
            _ => {}
        }
    }
    Ok(Vec::new())
}

/// Send every request, then read responses until the server closes.
async fn round_trip(
    channel: &Channel,
    path: &str,
    requests: Vec<ServerReflectionRequest>,
    metadata: &[(String, String)],
) -> Result<Vec<ServerReflectionResponse>> {
    use futures::StreamExt;

    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let expected = requests.len();

    // A server that accepts the connection and then never answers the
    // reflection stream would otherwise hang Send and scenario planning for
    // good; only the TCP connect had a timeout.
    const REFLECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let timed_out = || {
        GrpcError::Transport(format!(
            "the server did not answer reflection within {}s",
            REFLECTION_TIMEOUT.as_secs()
        ))
    };

    let mut grpc = tonic::client::Grpc::new(channel.clone());
    tokio::time::timeout(REFLECTION_TIMEOUT, grpc.ready())
        .await
        .map_err(|_| timed_out())?
        .map_err(|e| GrpcError::Transport(format!("could not connect: {e}")))?;

    let path = http::uri::PathAndQuery::from_static(match path {
        V1_PATH => V1_PATH,
        _ => V1ALPHA_PATH,
    });

    let codec: tonic_prost::ProstCodec<ServerReflectionRequest, ServerReflectionResponse> =
        tonic_prost::ProstCodec::default();

    let stream = futures::stream::iter(requests);
    let mut request = Request::new(stream);
    crate::call::apply_metadata(request.metadata_mut(), metadata)?;

    let response = tokio::time::timeout(REFLECTION_TIMEOUT, grpc.streaming(request, path, codec))
        .await
        .map_err(|_| timed_out())?
        .map_err(|status| status_to_error(&status))?;

    let mut inbound = response.into_inner();
    let mut out = Vec::new();
    while let Some(next) = tokio::time::timeout(REFLECTION_TIMEOUT, inbound.next())
        .await
        .map_err(|_| timed_out())?
    {
        match next {
            Ok(msg) => {
                out.push(msg);
                // The server answers one response per request and then waits
                // for more input, so stop once we have them all rather than
                // blocking on a stream that will not end on its own.
                if out.len() >= expected {
                    break;
                }
            }
            Err(status) => return Err(status_to_error(&status)),
        }
    }
    Ok(out)
}

fn status_to_error(status: &tonic::Status) -> GrpcError {
    match status.code() {
        Code::Unimplemented => GrpcError::Transport(format!(
            "unimplemented: {} ({})",
            status.message(),
            status.code()
        )),
        _ => GrpcError::Transport(format!("{}: {}", status.code(), status.message())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_only_on_unimplemented() {
        assert!(should_fall_back(&GrpcError::Transport(
            "Unimplemented: unknown service".into()
        )));
        assert!(!should_fall_back(&GrpcError::Transport(
            "connection refused".into()
        )));
        assert!(!should_fall_back(&GrpcError::Timeout(10)));
    }

    #[test]
    fn wire_types_round_trip() {
        let req = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        };
        let bytes = req.encode_to_vec();
        let back = ServerReflectionRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(back, req);
    }
}
