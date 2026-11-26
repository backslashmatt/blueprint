//! HTTPS for the update check (feature `net`, DR 0019): ureq with rustls,
//! certificates checked by the platform's verifier, HTTPS only on every
//! redirect, bounded in time and size. Requests go to github.com, which
//! redirects downloads to githubusercontent.com; the system's proxy
//! settings (`HTTPS_PROXY`) are honoured. Nothing is sent but the request,
//! with `User-Agent: Blueprint/<version>` and `Accept: */*`.

use std::sync::Arc;
use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::Fetch;

/// An HTTPS client for [`crate::check`] and [`crate::download`].
pub struct Https {
    agent: ureq::Agent,
}

impl Https {
    /// A client that identifies itself only as `Blueprint/<version>`.
    #[must_use]
    pub fn new(version: &str) -> Https {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .https_only(true)
            .max_redirects(5)
            // No limit on the whole request: an installer on a slow link takes as long
            // as it takes. Each phase is bounded instead, and a stalled body fails.
            .timeout_resolve(Some(Duration::from_secs(30)))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .user_agent(format!("Blueprint/{version}"))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Https { agent }
    }
}

impl Fetch for Https {
    fn get(&self, url: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
        let response = self.agent.get(url).call().map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            code => return Err(format!("{url}: HTTP {code}")),
        }
        response
            .into_body()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}
