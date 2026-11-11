//! HTTP client for the Hue bridge local API.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;

use super::tls;
use crate::config::AppKey;

const APP_KEY_HEADER: &str = "hue-application-key";

#[derive(Debug, thiserror::Error)]
pub enum HueError {
    #[error("nothing answers at this address ({0})")]
    Unreachable(String),
    #[error("the device at this address is not a trusted Hue bridge ({0})")]
    Untrusted(String),
    #[error("the bridge did not answer in time")]
    Timeout,
    #[error("the bridge rejected the application key")]
    KeyRejected,
    #[error("no application key is configured")]
    MissingKey,
    #[error("unexpected answer from the bridge: {0}")]
    Unexpected(String),
}

/// Public identity of a bridge, readable without an application key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeInfo {
    pub name: String,
    #[serde(rename = "bridgeid")]
    pub bridge_id: String,
    #[serde(rename = "modelid")]
    pub model_id: String,
    #[serde(rename = "swversion")]
    pub software_version: String,
    #[serde(rename = "apiversion")]
    pub api_version: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientOptions {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HueClient {
    http: reqwest::Client,
    base_url: String,
    app_key: Option<AppKey>,
}

impl HueClient {
    /// Client for the bridge at `host` (`host` or `host:port`), over verified HTTPS.
    pub fn for_host(
        host: &str,
        app_key: Option<AppKey>,
        expected_bridge_id: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::new(
            format!("https://{host}"),
            app_key,
            expected_bridge_id,
            ClientOptions::default(),
        )
    }

    pub fn new(
        base_url: impl Into<String>,
        app_key: Option<AppKey>,
        expected_bridge_id: Option<&str>,
        options: ClientOptions,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls::client_config(expected_bridge_id)?)
            // The bridge is a device on the local network: never go through a proxy.
            .no_proxy()
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            app_key,
        })
    }

    pub fn has_app_key(&self) -> bool {
        self.app_key.is_some()
    }

    /// Reads the bridge identity. Works without an application key.
    pub async fn bridge_info(&self) -> Result<BridgeInfo, HueError> {
        let response = self
            .http
            .get(format!("{}/api/0/config", self.base_url))
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            return Err(HueError::Unexpected(format!("HTTP {}", response.status())));
        }
        response.json().await.map_err(|err| {
            if err.is_timeout() {
                HueError::Timeout
            } else {
                HueError::Unexpected("the bridge description could not be read".to_owned())
            }
        })
    }

    /// Checks that the bridge accepts the configured application key.
    pub async fn check_auth(&self) -> Result<(), HueError> {
        let key = self.app_key.as_ref().ok_or(HueError::MissingKey)?;
        let response = self
            .http
            .get(format!("{}/clip/v2/resource/bridge", self.base_url))
            .header(APP_KEY_HEADER, key.expose())
            .send()
            .await
            .map_err(request_error)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(HueError::KeyRejected),
            status => Err(HueError::Unexpected(format!("HTTP {status}"))),
        }
    }
}

fn request_error(err: reqwest::Error) -> HueError {
    // A connection that fails or times out means nothing answers at that address;
    // `Timeout` is kept for a bridge that accepts the connection but stays silent.
    if let Some(tls_error) = invalid_certificate(&err) {
        HueError::Untrusted(tls_error)
    } else if err.is_connect() {
        HueError::Unreachable(root_cause(&err))
    } else if err.is_timeout() {
        HueError::Timeout
    } else {
        HueError::Unexpected(root_cause(&err))
    }
}

/// The certificate verification failure behind `err`, if that is what went wrong.
fn invalid_certificate(err: &reqwest::Error) -> Option<String> {
    match find_tls_error(err) {
        Some(tls @ rustls::Error::InvalidCertificate(_)) => Some(tls.to_string()),
        _ => None,
    }
}

/// Finds the TLS error in an error chain. TLS errors travel wrapped in
/// `io::Error`s, whose `source()` skips the wrapped error: look inside them too.
fn find_tls_error<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a rustls::Error> {
    if let Some(tls) = err.downcast_ref::<rustls::Error>() {
        return Some(tls);
    }
    if let Some(inner) = err
        .downcast_ref::<std::io::Error>()
        .and_then(|io| io.get_ref())
        && let Some(tls) = find_tls_error(inner)
    {
        return Some(tls);
    }
    err.source().and_then(find_tls_error)
}

/// The innermost error message: the actual reason (refused, timed out…).
fn root_cause(err: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = err;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}

#[cfg(test)]
mod tests {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    use super::*;
    use crate::hue::testing::{VALID_KEY as KEY, closed_port_url, fake_bridge};

    fn client(base_url: &str, key: Option<&str>) -> HueClient {
        let options = ClientOptions {
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_millis(1500),
        };
        HueClient::new(base_url, key.map(AppKey::new), None, options).unwrap()
    }

    #[tokio::test]
    async fn reads_bridge_info_without_key() {
        let bridge = fake_bridge().await;
        let info = client(&bridge.uri(), None).bridge_info().await.unwrap();
        assert_eq!(
            info,
            BridgeInfo {
                name: "Hue Bridge".to_owned(),
                bridge_id: "ECB5FAFFFE13AF9C".to_owned(),
                model_id: "BSB002".to_owned(),
                software_version: "1978545030".to_owned(),
                api_version: "1.78.0".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn accepts_valid_key() {
        let bridge = fake_bridge().await;
        client(&bridge.uri(), Some(KEY)).check_auth().await.unwrap();
    }

    #[tokio::test]
    async fn reports_rejected_key() {
        let bridge = fake_bridge().await;
        let err = client(&bridge.uri(), Some("wrong")).check_auth().await;
        assert!(matches!(err, Err(HueError::KeyRejected)), "{err:?}");
    }

    #[tokio::test]
    async fn reports_missing_key_without_calling_the_bridge() {
        let err = client("http://127.0.0.1:9", None).check_auth().await;
        assert!(matches!(err, Err(HueError::MissingKey)), "{err:?}");
    }

    #[tokio::test]
    async fn reports_unreachable_bridge() {
        let err = client(&closed_port_url(), Some(KEY)).bridge_info().await;
        assert!(matches!(err, Err(HueError::Unreachable(_))), "{err:?}");
    }

    #[tokio::test]
    async fn reports_timeout_within_the_configured_delay() {
        let bridge = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&bridge)
            .await;

        let started = std::time::Instant::now();
        let err = client(&bridge.uri(), Some(KEY)).bridge_info().await;
        assert!(matches!(err, Err(HueError::Timeout)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn reports_unexpected_answer() {
        let bridge = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&bridge)
            .await;
        let err = client(&bridge.uri(), None).bridge_info().await;
        assert!(matches!(err, Err(HueError::Unexpected(_))), "{err:?}");
    }

    #[tokio::test]
    async fn errors_never_contain_the_key() {
        let bridge = fake_bridge().await;
        let secret = "super-secret-key";
        let err = client(&bridge.uri(), Some(secret))
            .check_auth()
            .await
            .unwrap_err();
        assert!(!format!("{err} {err:?}").contains(secret));
    }
}
