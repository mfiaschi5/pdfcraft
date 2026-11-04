//! The AI assistant's OS services (M13.A7): the HTTPS transport that sends its requests, and the
//! store that keeps its API key.
//!
//! - [`HttpsTransport`] applies `pdfcraft_ai::transport::check_url` before connecting:
//!   - only the address it is given, over HTTPS (or plain HTTP to this computer);
//!   - no redirects, so the key and the document are never sent anywhere else;
//!   - the OS certificate verifier, and timeouts;
//!   - a 16 MiB cap on the decoded answer, and cancellation between chunks.
//! - [`Keychain`] keeps the key in the login keychain on macOS. Elsewhere there is no system store
//!   yet, and [`api_key`] falls back to `ANTHROPIC_API_KEY` from the environment, or a key kept for
//!   the session only ([`SessionKeys`]). The key is never written to a settings file.
//!
//! - [`ClaudeCode`] instead runs the user's own `claude` program, signed in with their Claude
//!   subscription, with its tools and customisations turned off (personal use; see
//!   `pdfcraft_ai::claude_code`).
//!
//! Nothing here runs unless the user turns the assistant on and asks a question: no background
//! traffic, no update checks, no telemetry.

use std::sync::{Mutex, PoisonError};

use pdfcraft_ai::transport::Secret;

/// Where a key is kept: the keychain item's service name.
pub const KEYCHAIN_SERVICE: &str = "PdfCraft AI assistant";
/// The environment variable read when no key is stored.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// Longest key accepted, from anywhere.
pub const MAX_KEY_BYTES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("this system has no keychain PdfCraft can use; the key is kept for this session only")]
    Unavailable,
    #[error("the key is empty, longer than {MAX_KEY_BYTES} bytes, or not plain text")]
    Invalid,
    #[error("the keychain refused: {0}")]
    Store(String),
}

/// Somewhere to keep API keys, one per provider (`account`: "anthropic").
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, account: &str, key: &Secret) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// A key is printable ASCII without spaces, as provider keys are, and short.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_BYTES && key.bytes().all(|b| b.is_ascii_graphic())
}

/// The key to use for `account`: the stored one, else the environment variable `env` reads (the
/// caller passes `std::env::var` in the app, a fake in tests). Either is checked like a typed key.
pub fn api_key(store: &dyn SecretStore, account: &str, env: impl Fn(&str) -> Option<String>) -> Option<Secret> {
    if let Ok(Some(key)) = store.get(account)
        && valid_key(key.expose())
    {
        return Some(key);
    }
    env(API_KEY_ENV).map(|k| k.trim().to_owned()).filter(|k| valid_key(k)).map(Secret::new)
}

/// Keys kept in memory for this session only: where no keychain works (headless Linux), and in
/// portable mode, where nothing may stay behind on the machine.
#[derive(Default)]
pub struct SessionKeys {
    keys: Mutex<Vec<(String, Secret)>>,
}

impl SecretStore for SessionKeys {
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        let keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(keys.iter().find(|(a, _)| a == account).map(|(_, k)| k.clone()))
    }

    fn set(&self, account: &str, key: &Secret) -> Result<(), SecretError> {
        if !valid_key(key.expose()) {
            return Err(SecretError::Invalid);
        }
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        keys.retain(|(a, _)| a != account);
        keys.push((account.to_owned(), key.clone()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner).retain(|(a, _)| a != account);
        Ok(())
    }
}

/// The system keychain: the login keychain on macOS (a generic password item named
/// [`KEYCHAIN_SERVICE`]). Other systems answer [`SecretError::Unavailable`] for now.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

impl Keychain {
    /// Whether this system has a keychain PdfCraft can use.
    pub fn available() -> bool {
        cfg!(target_os = "macos")
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use security_framework::passwords;

    use super::{KEYCHAIN_SERVICE, Secret, SecretError, valid_key};

    /// `errSecItemNotFound`: nothing stored under that name.
    const NOT_FOUND: i32 = -25300;

    fn store_error(e: &security_framework::base::Error) -> SecretError {
        SecretError::Store(e.message().unwrap_or_else(|| format!("error {}", e.code())))
    }

    pub(super) fn get(account: &str) -> Result<Option<Secret>, SecretError> {
        match passwords::get_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(bytes) => {
                let key = String::from_utf8(bytes).map_err(|_| SecretError::Invalid)?;
                if valid_key(&key) { Ok(Some(Secret::new(key))) } else { Err(SecretError::Invalid) }
            }
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(store_error(&e)),
        }
    }

    pub(super) fn set(account: &str, key: &Secret) -> Result<(), SecretError> {
        passwords::set_generic_password(KEYCHAIN_SERVICE, account, key.expose().as_bytes()).map_err(|e| store_error(&e))
    }

    pub(super) fn delete(account: &str) -> Result<(), SecretError> {
        match passwords::delete_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(store_error(&e)),
        }
    }
}

impl SecretStore for Keychain {
    #[cfg(target_os = "macos")]
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        macos::get(account)
    }
    #[cfg(target_os = "macos")]
    fn set(&self, account: &str, key: &Secret) -> Result<(), SecretError> {
        if !valid_key(key.expose()) {
            return Err(SecretError::Invalid);
        }
        macos::set(account, key)
    }
    #[cfg(target_os = "macos")]
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        macos::delete(account)
    }

    #[cfg(not(target_os = "macos"))]
    fn get(&self, _account: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable)
    }
    #[cfg(not(target_os = "macos"))]
    fn set(&self, _account: &str, _key: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
    #[cfg(not(target_os = "macos"))]
    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use transport::{HttpsTransport, MAX_RESPONSE_BYTES};

/// Claude Code as the way to Claude, on the user's own subscription (personal use).
#[cfg(not(target_arch = "wasm32"))]
pub mod claude_code;
#[cfg(not(target_arch = "wasm32"))]
pub use claude_code::ClaudeCode;

#[cfg(not(target_arch = "wasm32"))]
mod transport {
    use std::io::Read;
    use std::ops::ControlFlow;
    use std::time::Duration;

    use pdfcraft_ai::transport::{HttpRequest, Transport, TransportError, check_url, is_local};

    /// Largest answer read, decoded (16 MiB).
    pub const MAX_RESPONSE_BYTES: usize = 16 << 20;
    const CONNECT: Duration = Duration::from_secs(15);
    /// Until the answer's headers arrive.
    const FIRST_BYTE: Duration = Duration::from_secs(120);
    /// The whole streamed answer: long, since a careful answer takes minutes.
    const WHOLE_ANSWER: Duration = Duration::from_secs(15 * 60);
    const READ_PIECE: usize = 16 * 1024;

    /// Sends the assistant's requests over HTTPS. See the module docs for the policy.
    pub struct HttpsTransport {
        /// Through the proxy the environment names, if any.
        agent: ureq::Agent,
        /// Never through a proxy: for a model on this computer.
        direct: ureq::Agent,
    }

    impl HttpsTransport {
        /// `user_agent` names the product and version (`PdfCraft/0.18.0`) and nothing about the
        /// person using it.
        pub fn new(user_agent: &str) -> Self {
            let agent = |proxy: bool| {
                let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
                let mut config = ureq::Agent::config_builder()
                    .tls_config(tls)
                    .timeout_connect(Some(CONNECT))
                    .timeout_recv_response(Some(FIRST_BYTE))
                    .timeout_recv_body(Some(WHOLE_ANSWER))
                    .http_status_as_error(false)
                    .max_redirects(0)
                    .max_redirects_will_error(false)
                    .user_agent(user_agent);
                if !proxy {
                    config = config.proxy(None);
                }
                config.build().new_agent()
            };
            Self { agent: agent(true), direct: agent(false) }
        }
    }

    fn sending_error(e: ureq::Error) -> TransportError {
        match e {
            ureq::Error::Timeout(_) => TransportError::Timeout,
            ureq::Error::HostNotFound => TransportError::Connect("the provider's address wasn't found".into()),
            ureq::Error::ConnectionFailed => TransportError::Connect("the connection failed".into()),
            ureq::Error::Io(e) => reading_error(&e),
            other => TransportError::Io(other.to_string()),
        }
    }

    fn reading_error(e: &std::io::Error) -> TransportError {
        match e.kind() {
            std::io::ErrorKind::TimedOut => TransportError::Timeout,
            std::io::ErrorKind::ConnectionRefused => TransportError::Connect("the provider refused the connection".into()),
            _ => TransportError::Io(e.to_string()),
        }
    }

    impl Transport for HttpsTransport {
        fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> ControlFlow<()>) -> Result<u16, TransportError> {
            check_url(&req.url)?;
            let agent = if is_local(&req.url) { &self.direct } else { &self.agent };
            let mut request = agent.post(&req.url);
            for (name, value) in &req.headers {
                request = request.header(name.as_str(), value.expose());
            }
            let mut response = request.send(&req.body[..]).map_err(sending_error)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                return Err(TransportError::Refused("the provider answered with a redirect, which PdfCraft doesn't follow".into()));
            }
            // The cap is counted on the decoded bytes: ureq's own limit sits below gzip decoding.
            let mut reader = response.body_mut().with_config().reader();
            let mut buf = vec![0u8; READ_PIECE];
            let mut total = 0usize;
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(reading_error(&e)),
                };
                total = total.saturating_add(n);
                if total > MAX_RESPONSE_BYTES {
                    return Err(TransportError::TooLarge(MAX_RESPONSE_BYTES));
                }
                if on_bytes(buf.get(..n).unwrap_or_default()).is_break() {
                    return Err(TransportError::Cancelled);
                }
            }
            Ok(status)
        }
    }
}

#[cfg(test)]
mod tests;
