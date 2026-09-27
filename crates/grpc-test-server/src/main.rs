#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50051);

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("could not bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    println!("swarmo gRPC test server listening on http://{addr}");
    println!("service: swarmo.testing.TestService (reflection enabled)");

    let svc = grpc_test_server::pb::test_service_server::TestServiceServer::new(
        grpc_test_server::Service::default(),
    );
    let v1 = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(grpc_test_server::pb::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();
    let v1alpha = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(grpc_test_server::pb::FILE_DESCRIPTOR_SET)
        .build_v1alpha()
        .unwrap();

    tonic::transport::Server::builder()
        .add_service(svc)
        .add_service(v1)
        .add_service(v1alpha)
        .serve_with_incoming(incoming)
        .await
        .unwrap();
}
