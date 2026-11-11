//! MCP server definition: the tools exposed to clients.

use rmcp::{
    Json, ServerHandler,
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::Serialize;

use crate::{
    config::{APP_KEY_ENV, BRIDGE_HOST_ENV, Config},
    hue::{BridgeInfo, HueClient, HueError},
};

#[derive(Debug, Clone, Default)]
pub struct HueServer {
    bridge: Option<Bridge>,
}

#[derive(Debug, Clone)]
struct Bridge {
    address: String,
    client: HueClient,
}

/// How the bridge address was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressSource {
    /// Set through the `HUE_BRIDGE_HOST` environment variable.
    Configured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BridgeDetails {
    pub name: String,
    pub bridge_id: String,
    pub model_id: String,
    pub software_version: String,
    pub api_version: String,
}

impl From<BridgeInfo> for BridgeDetails {
    fn from(info: BridgeInfo) -> Self {
        Self {
            name: info.name,
            bridge_id: info.bridge_id,
            model_id: info.model_id,
            software_version: info.software_version,
            api_version: info.api_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BridgeStatus {
    /// Address used to reach the bridge, if known.
    pub address: Option<String>,
    pub address_source: Option<AddressSource>,
    /// Whether the bridge answered on the network.
    pub reachable: bool,
    /// Whether an application key is configured on the server.
    pub key_configured: bool,
    /// Whether the bridge accepted the application key.
    pub authenticated: bool,
    /// Bridge identity, when it is reachable.
    pub bridge: Option<BridgeDetails>,
    /// Human-readable summary, including what to do when something is wrong.
    pub message: String,
}

impl HueServer {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let Some(host) = &config.bridge_host else {
            return Ok(Self::default());
        };
        let client =
            HueClient::for_host(host, config.app_key.clone(), config.bridge_id.as_deref())?;
        Ok(Self::with_client(host, client))
    }

    fn with_client(address: &str, client: HueClient) -> Self {
        Self {
            bridge: Some(Bridge {
                address: address.to_owned(),
                client,
            }),
        }
    }

    async fn status(&self) -> BridgeStatus {
        let Some(bridge) = &self.bridge else {
            return BridgeStatus {
                address: None,
                address_source: None,
                reachable: false,
                key_configured: false,
                authenticated: false,
                bridge: None,
                message: format!(
                    "The bridge address is not configured. Set {BRIDGE_HOST_ENV} to the bridge IP address or host name and restart the server."
                ),
            };
        };

        let mut status = BridgeStatus {
            address: Some(bridge.address.clone()),
            address_source: Some(AddressSource::Configured),
            reachable: false,
            key_configured: bridge.client.has_app_key(),
            authenticated: false,
            bridge: None,
            message: String::new(),
        };

        match bridge.client.bridge_info().await {
            Ok(info) => {
                status.reachable = true;
                status.bridge = Some(info.into());
            }
            Err(err) => {
                status.message =
                    format!("Cannot connect to the bridge at {}: {err}.", bridge.address);
                return status;
            }
        }

        status.message = match bridge.client.check_auth().await {
            Ok(()) => {
                status.authenticated = true;
                "The bridge is reachable and the application key is accepted.".to_owned()
            }
            Err(HueError::MissingKey) => format!(
                "The bridge is reachable but no application key is configured. Create one (see the README), set {APP_KEY_ENV} and restart the server."
            ),
            Err(HueError::KeyRejected) => format!(
                "The bridge is reachable but rejected the application key. Check {APP_KEY_ENV} or create a new key (see the README)."
            ),
            Err(err) => format!("The bridge is reachable but the key could not be checked: {err}."),
        };
        status
    }
}

#[tool_router]
impl HueServer {
    #[tool(description = "Check that the Hue MCP server is up. Returns \"pong\".")]
    fn ping(&self) -> String {
        "pong".to_owned()
    }

    #[tool(
        description = "Diagnose the connection to the Philips Hue bridge: whether it is reachable, whether the application key is accepted, and the bridge name, model, id and software version."
    )]
    async fn bridge_status(&self) -> Json<BridgeStatus> {
        Json(self.status().await)
    }
}

#[tool_handler]
impl ServerHandler for HueServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("Controls a Philips Hue bridge on the local network.")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        config::AppKey,
        hue::{
            ClientOptions,
            testing::{VALID_KEY, closed_port_url, fake_bridge},
        },
    };

    fn server(base_url: &str, key: Option<&str>) -> HueServer {
        let options = ClientOptions {
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_millis(1500),
        };
        let client = HueClient::new(base_url, key.map(AppKey::new), None, options).unwrap();
        HueServer::with_client("bridge.test", client)
    }

    #[test]
    fn exposes_tools() {
        let mut names: Vec<_> = HueServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["bridge_status", "ping"]);
    }

    #[test]
    fn ping_returns_pong() {
        assert_eq!(HueServer::default().ping(), "pong");
    }

    #[tokio::test]
    async fn status_with_valid_key() {
        let bridge = fake_bridge().await;
        let status = server(&bridge.uri(), Some(VALID_KEY)).status().await;

        assert!(status.reachable && status.key_configured && status.authenticated);
        assert_eq!(status.address.as_deref(), Some("bridge.test"));
        assert_eq!(status.address_source, Some(AddressSource::Configured));
        let details = status.bridge.unwrap();
        assert_eq!(details.name, "Hue Bridge");
        assert_eq!(details.model_id, "BSB002");
        assert_eq!(details.bridge_id, "ECB5FAFFFE13AF9C");
        assert_eq!(details.software_version, "1978545030");
    }

    #[tokio::test]
    async fn status_without_key() {
        let bridge = fake_bridge().await;
        let status = server(&bridge.uri(), None).status().await;

        assert!(status.reachable);
        assert!(!status.key_configured && !status.authenticated);
        assert!(status.bridge.is_some());
        assert!(status.message.contains(APP_KEY_ENV), "{}", status.message);
    }

    #[tokio::test]
    async fn status_with_rejected_key() {
        let bridge = fake_bridge().await;
        let status = server(&bridge.uri(), Some("wrong-key")).status().await;

        assert!(status.reachable && status.key_configured);
        assert!(!status.authenticated);
        assert!(status.message.contains("rejected"), "{}", status.message);
        assert!(!status.message.contains("wrong-key"));
    }

    #[tokio::test]
    async fn status_with_unreachable_bridge() {
        let status = server(&closed_port_url(), Some(VALID_KEY)).status().await;

        assert!(!status.reachable && !status.authenticated);
        assert!(status.bridge.is_none());
        assert!(
            status.message.contains("Cannot connect"),
            "{}",
            status.message
        );
    }

    #[tokio::test]
    async fn status_without_configured_address() {
        let status = HueServer::default().status().await;

        assert!(!status.reachable);
        assert_eq!(status.address, None);
        assert!(
            status.message.contains(BRIDGE_HOST_ENV),
            "{}",
            status.message
        );
    }

    #[tokio::test]
    async fn status_never_contains_the_key() {
        let bridge = fake_bridge().await;
        let status = server(&bridge.uri(), Some(VALID_KEY)).status().await;
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains(VALID_KEY), "{json}");
    }
}
