//! Channel pooling.
//!
//! A `tonic::Channel` multiplexes many concurrent RPCs over one HTTP/2
//! connection, which is ideal for the client UI but can become the bottleneck
//! under load: a single connection shares one flow-control window and one TCP
//! congestion window. The load engine therefore asks for several channels per
//! address and assigns them to virtual users round-robin.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::error::{GrpcError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelKey {
    pub verify_tls: bool,
}

/// Lazily-created channels, keyed by address and TLS mode.
pub struct ChannelPool {
    channels: Mutex<HashMap<(String, ChannelKey, usize), Channel>>,
    /// How many channels to keep per address (1 for the client UI).
    width: usize,
}

impl Default for ChannelPool {
    fn default() -> Self {
        Self::new(1)
    }
}

impl ChannelPool {
    pub fn new(width: usize) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            width: width.clamp(1, 64),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Get a channel for this address. `slot` spreads load across the pool;
    /// pass the virtual-user id, or 0 from the client UI.
    pub async fn get(&self, address: &str, verify_tls: bool, slot: usize) -> Result<Channel> {
        let key = ChannelKey { verify_tls };
        let index = if self.width <= 1 {
            0
        } else {
            slot % self.width
        };

        {
            let guard = self.channels.lock().unwrap();
            if let Some(c) = guard.get(&(address.to_string(), key, index)) {
                return Ok(c.clone());
            }
        }

        let channel = connect(address, verify_tls).await?;

        let mut guard = self.channels.lock().unwrap();
        // Another task may have inserted while we were connecting; either is fine.
        let entry = guard
            .entry((address.to_string(), key, index))
            .or_insert(channel);
        Ok(entry.clone())
    }

    pub fn clear(&self) {
        self.channels.lock().unwrap().clear();
    }
}

async fn connect(address: &str, verify_tls: bool) -> Result<Channel> {
    let address = address.trim();
    if address.is_empty() {
        return Err(GrpcError::invalid("The address is empty."));
    }
    if !address.starts_with("http://") && !address.starts_with("https://") {
        return Err(GrpcError::invalid(format!(
            "\"{address}\" is not a gRPC address. Use http://host:port for plaintext or \
             https://host:port for TLS."
        )));
    }

    let is_tls = address.starts_with("https://");

    let mut endpoint = Endpoint::from_shared(address.to_string())
        .map_err(|e| GrpcError::invalid(format!("Invalid address \"{address}\": {e}")))?
        .tcp_nodelay(true)
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(20));

    if is_tls {
        endpoint = if verify_tls {
            endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|e| GrpcError::Transport(format!("TLS setup failed: {e}")))?
        } else {
            // Matches the HTTP side's "verify TLS" toggle. Only reachable when
            // the user explicitly turned verification off for this request.
            //
            // Deliberately *no* root certificates here: tonic rejects a custom
            // verifier combined with a root store, and roots would be
            // meaningless anyway when the verifier accepts everything.
            endpoint
                .tls_config_with_verifier(
                    ClientTlsConfig::new(),
                    std::sync::Arc::new(NoVerification::new()),
                )
                .map_err(|e| GrpcError::Transport(format!("TLS setup failed: {e}")))?
        };
    }

    // `connect_lazy` avoids a round trip here and surfaces connection problems
    // on the first call instead, where they can be reported per-request and
    // counted as a sample rather than failing the whole run.
    Ok(endpoint.connect_lazy())
}

/// A certificate verifier that accepts anything.
///
/// This exists solely to back the per-request "verify TLS certificates"
/// toggle, which the HTTP side already offers and which is routinely needed
/// against staging servers with self-signed certificates. It is never the
/// default: a request must explicitly opt out of verification to reach it.
#[derive(Debug)]
struct NoVerification {
    provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
}

impl NoVerification {
    fn new() -> Self {
        // Whichever provider rustls has installed; tonic installs ring.
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider()));
        Self { provider }
    }
}

impl rustls::client::danger::ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_addresses_without_a_scheme() {
        let pool = ChannelPool::default();
        let err = pool.get("127.0.0.1:50051", true, 0).await.unwrap_err();
        assert!(err.to_string().contains("http://"), "{err}");
    }

    #[tokio::test]
    async fn rejects_an_empty_address() {
        let pool = ChannelPool::default();
        assert!(pool.get("", true, 0).await.is_err());
    }

    #[tokio::test]
    async fn reuses_the_same_channel_for_one_slot() {
        let pool = ChannelPool::new(1);
        let a = pool.get("http://127.0.0.1:1", false, 0).await.unwrap();
        let b = pool.get("http://127.0.0.1:1", false, 7).await.unwrap();
        // Width 1 means every slot maps to the same entry.
        assert_eq!(pool.channels.lock().unwrap().len(), 1);
        drop((a, b));
    }

    #[tokio::test]
    async fn a_wider_pool_creates_distinct_channels() {
        let pool = ChannelPool::new(4);
        for slot in 0..8 {
            pool.get("http://127.0.0.1:1", false, slot).await.unwrap();
        }
        assert_eq!(pool.channels.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn tls_mode_keys_separately() {
        let pool = ChannelPool::new(1);
        pool.get("https://example.test", true, 0).await.unwrap();
        pool.get("https://example.test", false, 0).await.unwrap();
        assert_eq!(pool.channels.lock().unwrap().len(), 2);
    }
}
