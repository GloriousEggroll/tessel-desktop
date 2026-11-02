//! Bounded HTTPS GET (urllib.request.urlopen equivalent) over the system
//! OpenSSL via native-tls. Bodies are read up to `limit + 1` bytes so callers
//! can reject oversized responses exactly like `response.read(limit + 1)`.

use std::io::Read;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum HttpError {
    Status(u16),
    Transport,
}

fn connector() -> Option<Arc<native_tls::TlsConnector>> {
    static CONNECTOR: OnceLock<Option<Arc<native_tls::TlsConnector>>> = OnceLock::new();
    CONNECTOR.get_or_init(|| native_tls::TlsConnector::new().ok().map(Arc::new)).clone()
}

/// GET `url`; `http_only_redirects` rejects a final URL that is not http(s).
pub fn get(url: &str, user_agent: &str, accept: &str, timeout: Duration, limit: usize,
           http_only_redirects: bool) -> Result<Vec<u8>, HttpError> {
    // urllib honours http(s)_proxy/no_proxy from the environment; so does this agent.
    let mut builder = ureq::AgentBuilder::new().timeout(timeout).user_agent(user_agent).try_proxy_from_env(true);
    if let Some(connector) = connector() {
        builder = builder.tls_connector(connector);
    }
    let agent = builder.build();
    let response = match agent.get(url).set("Accept", accept).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => return Err(HttpError::Status(code)),
        Err(_) => return Err(HttpError::Transport),
    };
    if http_only_redirects && !(response.get_url().starts_with("http://") || response.get_url().starts_with("https://")) {
        return Err(HttpError::Transport);
    }
    let mut data = Vec::new();
    response.into_reader().take(limit as u64 + 1).read_to_end(&mut data).map_err(|_| HttpError::Transport)?;
    Ok(data)
}
