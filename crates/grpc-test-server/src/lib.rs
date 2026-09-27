//! A small tonic service used by Swarmo's gRPC tests and for manual QA.
//! Binds loopback only; nothing here touches the network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use tonic::{Request, Response, Status};

pub mod pb {
    tonic::include_proto!("swarmo.testing");
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/testing_descriptor.bin"));
}

use pb::test_service_server::{TestService, TestServiceServer};
use pb::*;

#[derive(Default)]
pub struct Service {
    calls: AtomicU64,
}

#[tonic::async_trait]
impl TestService for Service {
    async fn echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoReply>, Status> {
        // Reflect back the ascii metadata so tests can prove it arrived.
        let mut metadata = HashMap::new();
        for key in request.metadata().keys() {
            if let tonic::metadata::KeyRef::Ascii(k) = key {
                if let Some(v) = request.metadata().get(k.as_str()) {
                    metadata.insert(
                        k.as_str().to_string(),
                        v.to_str().unwrap_or("<non-ascii>").to_string(),
                    );
                }
            }
        }

        let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        let r = request.into_inner();

        Ok(Response::new(EchoReply {
            message: r.message,
            number: r.number,
            flag: r.flag,
            nested: r.nested,
            at: r.at,
            tags: r.tags,
            metadata,
            call_count: n,
        }))
    }

    async fn delay(&self, request: Request<DelayRequest>) -> Result<Response<DelayReply>, Status> {
        let ms = request.into_inner().ms.min(60_000);
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(Response::new(DelayReply { delayed_ms: ms }))
    }

    async fn fail(&self, request: Request<FailRequest>) -> Result<Response<FailReply>, Status> {
        let r = request.into_inner();
        if r.code == 0 {
            return Ok(Response::new(FailReply {
                never: "actually succeeded".into(),
            }));
        }
        let message = if r.message.is_empty() {
            format!("requested failure with code {}", r.code)
        } else {
            r.message
        };
        Err(Status::new(code_from_i32(r.code), message))
    }

    async fn login(&self, request: Request<LoginRequest>) -> Result<Response<LoginReply>, Status> {
        let user = request.into_inner().user;
        if user.trim().is_empty() {
            return Err(Status::invalid_argument("user is required"));
        }
        Ok(Response::new(LoginReply {
            token: "grpc_tok_123".into(),
            expires_in: 3600,
        }))
    }

    type StreamNumbersStream = tokio_stream::wrappers::ReceiverStream<Result<StreamReply, Status>>;

    async fn stream_numbers(
        &self,
        request: Request<StreamRequest>,
    ) -> Result<Response<Self::StreamNumbersStream>, Status> {
        let count = request.into_inner().count.clamp(0, 1000);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            for i in 0..count {
                if tx.send(Ok(StreamReply { value: i })).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn sum(
        &self,
        request: Request<tonic::Streaming<SumRequest>>,
    ) -> Result<Response<SumReply>, Status> {
        let mut inbound = request.into_inner();
        let (mut total, mut count) = (0i64, 0i32);
        while let Some(m) = inbound.message().await? {
            total += i64::from(m.value);
            count += 1;
        }
        Ok(Response::new(SumReply { total, count }))
    }

    type ChatStream = tokio_stream::wrappers::ReceiverStream<Result<ChatMessage, Status>>;

    async fn chat(
        &self,
        request: Request<tonic::Streaming<ChatMessage>>,
    ) -> Result<Response<Self::ChatStream>, Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            while let Ok(Some(m)) = inbound.message().await {
                let reply = ChatMessage {
                    text: m.text.to_uppercase(),
                };
                if tx.send(Ok(reply)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

fn code_from_i32(code: i32) -> tonic::Code {
    match code {
        1 => tonic::Code::Cancelled,
        2 => tonic::Code::Unknown,
        3 => tonic::Code::InvalidArgument,
        4 => tonic::Code::DeadlineExceeded,
        5 => tonic::Code::NotFound,
        6 => tonic::Code::AlreadyExists,
        7 => tonic::Code::PermissionDenied,
        8 => tonic::Code::ResourceExhausted,
        9 => tonic::Code::FailedPrecondition,
        10 => tonic::Code::Aborted,
        11 => tonic::Code::OutOfRange,
        12 => tonic::Code::Unimplemented,
        13 => tonic::Code::Internal,
        14 => tonic::Code::Unavailable,
        15 => tonic::Code::DataLoss,
        16 => tonic::Code::Unauthenticated,
        _ => tonic::Code::Unknown,
    }
}

/// Rejects any request that does not carry `authorization: Bearer <token>`,
/// including reflection requests — which is exactly how a real authenticated
/// deployment behaves, and what makes the "reflection must send auth" case
/// testable.
#[derive(Clone)]
pub struct RequireAuth<S> {
    inner: S,
    token: String,
}

impl<S, B> tower::Service<http::Request<B>> for RequireAuth<S>
where
    S: tower::Service<http::Request<B>, Response = http::Response<tonic::body::Body>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Error>>
                + Send,
        >,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let expected = format!("Bearer {}", self.token);
        let ok = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == expected);

        // Clone-before-call: the readiness we polled belongs to `self.inner`.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            if !ok {
                return Ok(Status::unauthenticated("missing or wrong bearer token").into_http());
            }
            inner.call(req).await
        })
    }
}

/// How much of the reflection service to expose, so the fallback path is
/// testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reflection {
    /// Serve both v1 and v1alpha (what most servers do today).
    Both,
    /// Serve only v1alpha, exercising the client's fallback.
    V1AlphaOnly,
    /// Serve no reflection at all.
    Disabled,
}

/// Bind on an ephemeral loopback port and serve until the handle is dropped.
pub async fn spawn() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    spawn_with(Reflection::Both).await
}

/// Like [`spawn`], but every request — reflection included — must present
/// `authorization: Bearer <token>`.
pub async fn spawn_requiring_auth(token: &str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let token = token.to_string();
    let layer = tower::layer::layer_fn(move |inner| RequireAuth {
        inner,
        token: token.clone(),
    });

    let v1 = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(pb::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    let handle = tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .layer(layer)
            .add_service(TestServiceServer::new(Service::default()))
            .add_service(v1)
            .serve_with_incoming(incoming)
            .await;
    });
    (addr, handle)
}

pub async fn spawn_with(reflection: Reflection) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let svc = TestServiceServer::new(Service::default());
    let mut router = tonic::transport::Server::builder().add_service(svc);

    match reflection {
        Reflection::Disabled => {}
        Reflection::V1AlphaOnly => {
            let r = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(pb::FILE_DESCRIPTOR_SET)
                .build_v1alpha()
                .unwrap();
            router = router.add_service(r);
        }
        Reflection::Both => {
            let v1 = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(pb::FILE_DESCRIPTOR_SET)
                .build_v1()
                .unwrap();
            let v1alpha = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(pb::FILE_DESCRIPTOR_SET)
                .build_v1alpha()
                .unwrap();
            router = router.add_service(v1).add_service(v1alpha);
        }
    }

    let handle = tokio::spawn(async move {
        let _ = router.serve_with_incoming(incoming).await;
    });
    (addr, handle)
}

/// The `.proto` source, so tests and the demo workspace can write it to disk.
pub const TESTING_PROTO: &str = include_str!("../protos/testing.proto");

/// Path to a directory containing testing.proto, for file-based descriptor tests.
pub fn proto_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("protos")
}

pub fn proto_file() -> std::path::PathBuf {
    proto_dir().join("testing.proto")
}
