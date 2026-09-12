//! The one implementation that opens a socket.
//!
//! Nothing else in this crate touches the network, and no test constructs this
//! type — `spec/00-overview.md` §7.
//!
//! **TLS.** `rustls` with the **`ring`** provider, never `aws-lc-rs`, which
//! needs `cmake` (`PREREQUISITES.md` §5.1). The workspace pins the feature
//! flags; this module does the other half, which the flags cannot do:
//! `rustls` has no default provider when more than none is compiled in, so one
//! must be installed into the process exactly once, before the first
//! connection. Forgetting it is a runtime error on the first request, not a
//! build error — which is why [`ReqwestTransport::new`] does it rather than
//! leaving it to a caller to remember.

use crate::transport::{Headers, HttpRequest, HttpResponse, Method, Transport, TransportError};
use async_trait::async_trait;
use std::error::Error as _;
use std::sync::Once;
use std::time::Duration;

/// Install the `ring` crypto provider. Idempotent, and safe to call from
/// anywhere.
///
/// `install_default` returns `Err` if a provider is already installed, which
/// is a perfectly good outcome — another crate, or an earlier call, got there
/// first. The `Once` keeps it to a single attempt regardless.
pub fn install_crypto_provider() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            tracing::debug!("a rustls crypto provider was already installed");
        }
    });
}

/// How long a single request may take before it counts as `Offline`.
///
/// A round trip to GitHub is ~700ms (`spec/00-overview.md` §4.2). Thirty
/// seconds is far beyond anything healthy, and the point of the ceiling is
/// that a refresh cannot hang forever, not that it is tight.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, TransportError> {
        install_crypto_provider();

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // Explicit even though it is the only TLS backend compiled in:
            // the feature set is what keeps OpenSSL out, and saying so here
            // makes a future `default-features` slip fail loudly.
            //
            // Roots come from the system store via `rustls-native-certs`,
            // not a compiled-in bundle — a corporate TLS-inspecting proxy
            // otherwise fails in a way that looks like a network bug
            // (`PREREQUISITES.md` §5.1).
            .use_rustls_tls()
            .build()
            .map_err(|e| TransportError::Malformed(e.to_string()))?;

        Ok(Self { client })
    }

    pub fn from_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

fn method_of(m: Method) -> reqwest::Method {
    match m {
        Method::Get => reqwest::Method::GET,
        Method::Head => reqwest::Method::HEAD,
        Method::Post => reqwest::Method::POST,
        Method::Patch => reqwest::Method::PATCH,
        Method::Put => reqwest::Method::PUT,
        Method::Delete => reqwest::Method::DELETE,
    }
}

/// Classify a `reqwest` failure into the `Offline` / `Upstream` split.
///
/// The distinction is load-bearing: `Offline` with a warm cache is a banner,
/// while a request we could not even build is our own bug and never retried.
fn classify(e: &reqwest::Error) -> TransportError {
    let detail = e
        .source()
        .map_or_else(|| e.to_string(), std::string::ToString::to_string);

    if e.is_timeout() {
        TransportError::Timeout(detail)
    } else if e.is_connect() || e.is_request() {
        TransportError::Unreachable(detail)
    } else if e.is_body() || e.is_decode() {
        TransportError::Body(detail)
    } else if e.is_builder() {
        TransportError::Malformed(detail)
    } else {
        TransportError::Unreachable(detail)
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = self.client.request(method_of(request.method), &request.url);
        for (name, value) in request.headers.iter() {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(|e| classify(&e))?;

        let status = response.status().as_u16();
        let headers: Headers = response
            .headers()
            .iter()
            .filter_map(|(n, v)| v.to_str().ok().map(|v| (n.as_str(), v)))
            .collect();
        let body = response.bytes().await.map_err(|e| classify(&e))?.to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installing_the_provider_twice_is_fine() {
        // The idempotence matters: `omaghy tui` and `omaghy watch` both
        // construct a transport, and the second must not panic.
        install_crypto_provider();
        install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn methods_survive_the_round_trip() {
        for m in [
            Method::Get,
            Method::Head,
            Method::Post,
            Method::Patch,
            Method::Put,
            Method::Delete,
        ] {
            assert_eq!(method_of(m).as_str(), m.as_str());
        }
    }
}
