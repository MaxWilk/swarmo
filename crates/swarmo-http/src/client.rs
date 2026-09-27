//! A small cache of `reqwest::Client`s keyed by the settings that require a
//! distinct client (redirects, TLS verification, timeout, proxy).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::cookie::Jar;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientSettings {
    pub follow_redirects: bool,
    pub verify_tls: bool,
    pub timeout_ms: u64,
    pub proxy: Option<String>,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            follow_redirects: true,
            verify_tls: true,
            timeout_ms: 30_000,
            proxy: None,
        }
    }
}

/// Keeps up to `CAP` distinct clients alive, sharing one cookie jar so that
/// session cookies survive across differently-configured requests.
pub struct ClientPool {
    /// Replaced wholesale by [`Self::clear_cookies`]: a `reqwest::Jar` has no
    /// "remove everything" of its own, and every cached client holds a clone
    /// of the `Arc`, so emptying the client cache alone would rebuild each
    /// client around the same, still-populated jar.
    jar: Mutex<Arc<Jar>>,
    entries: Mutex<Vec<(ClientSettings, reqwest::Client)>>,
}

const CAP: usize = 8;

impl Default for ClientPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientPool {
    pub fn new() -> Self {
        Self {
            jar: Mutex::new(Arc::new(Jar::default())),
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn jar(&self) -> Arc<Jar> {
        self.jar.lock().unwrap().clone()
    }

    /// Forget every cookie: a fresh jar, and no client left holding the old
    /// one. Dropping the clients alone does not do this — see the field.
    pub fn clear_cookies(&self) {
        *self.jar.lock().unwrap() = Arc::new(Jar::default());
        self.entries.lock().unwrap().clear();
    }

    pub fn get(&self, settings: &ClientSettings) -> Result<reqwest::Client, reqwest::Error> {
        {
            let entries = self.entries.lock().unwrap();
            if let Some((_, c)) = entries.iter().find(|(s, _)| s == settings) {
                return Ok(c.clone());
            }
        }

        let client = build_client(settings, self.jar())?;

        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= CAP {
            entries.remove(0);
        }
        entries.push((settings.clone(), client.clone()));
        Ok(client)
    }

    /// Drop every cached client, which also drops their connection pools.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

pub fn build_client(
    settings: &ClientSettings,
    jar: Arc<Jar>,
) -> Result<reqwest::Client, reqwest::Error> {
    let redirect = if settings.follow_redirects {
        reqwest::redirect::Policy::limited(10)
    } else {
        reqwest::redirect::Policy::none()
    };

    let mut builder = reqwest::Client::builder()
        .redirect(redirect)
        .danger_accept_invalid_certs(!settings.verify_tls)
        .cookie_provider(jar)
        .tcp_nodelay(true)
        .user_agent(concat!("Swarmo/", env!("CARGO_PKG_VERSION")));

    if settings.timeout_ms > 0 {
        builder = builder.timeout(Duration::from_millis(settings.timeout_ms));
    }

    if let Some(proxy) = &settings.proxy {
        if !proxy.trim().is_empty() {
            // Silently falling back to a direct connection is both surprising
            // and potentially unsafe: a user who configured a proxy expects a
            // malformed value to stop the request, not bypass the proxy.
            builder = builder.proxy(reqwest::Proxy::all(proxy.trim())?);
        }
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_proxy_is_rejected_instead_of_bypassed() {
        let settings = ClientSettings {
            proxy: Some(":// not a proxy".into()),
            ..ClientSettings::default()
        };

        assert!(build_client(&settings, Arc::new(Jar::default())).is_err());
    }

    #[test]
    fn a_blank_proxy_is_the_same_as_no_proxy() {
        let settings = ClientSettings {
            proxy: Some("   ".into()),
            ..ClientSettings::default()
        };

        assert!(build_client(&settings, Arc::new(Jar::default())).is_ok());
    }
}
