//! MCP server definition: the tools exposed to clients.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use rmcp::{
    Json, ServerHandler,
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::{
    config::{APP_KEY_ENV, AppKey, BRIDGE_HOST_ENV, BRIDGE_ID_ENV, Config},
    hue::{
        BridgeInfo, HueClient, HueError,
        discovery::{self, DiscoveredBridge, DiscoveryError},
    },
};

#[derive(Debug, Clone)]
pub struct HueServer {
    source: BridgeSource,
    /// Kept apart from `source`: a key can be configured while no bridge is known.
    key_configured: bool,
}

#[derive(Debug, Clone)]
enum BridgeSource {
    /// Address set through `HUE_BRIDGE_HOST`.
    Configured(Bridge),
    /// Address found through mDNS, shared by all sessions.
    Discovered(Arc<Discovery>),
}

#[derive(Debug, Clone)]
struct Bridge {
    address: String,
    client: HueClient,
    /// Announced bridge id, for a discovered bridge.
    bridge_id: Option<String>,
    /// Identifies the cache entry this bridge came from (0 when configured).
    generation: u64,
}

type FindFuture = Pin<Box<dyn Future<Output = Result<DiscoveredBridge, DiscoveryError>> + Send>>;
type FindFn = Arc<dyn Fn(Option<String>) -> FindFuture + Send + Sync>;
type ConnectFn =
    Arc<dyn Fn(&DiscoveredBridge, Option<AppKey>) -> anyhow::Result<HueClient> + Send + Sync>;

/// Finds the bridge on first use, then keeps its address until it stops answering.
struct Discovery {
    app_key: Option<AppKey>,
    /// `HUE_BRIDGE_ID`, if set.
    expected_bridge_id: Option<String>,
    find: FindFn,
    connect: ConnectFn,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    bridge: Option<Bridge>,
    /// Incremented each time a new bridge is remembered.
    generation: u64,
    /// Id of the bridge that accepted the application key. Kept for the life of
    /// the server, across failed searches: the key is never sent to another bridge.
    pinned_bridge_id: Option<String>,
}

impl fmt::Debug for Discovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Discovery")
            .field("app_key", &self.app_key)
            .field("expected_bridge_id", &self.expected_bridge_id)
            .finish_non_exhaustive()
    }
}

enum DiscoveryFailure {
    Search {
        error: DiscoveryError,
        /// Set when the search was restricted to the bridge that accepted the key,
        /// rather than to `HUE_BRIDGE_ID`.
        pinned_bridge_id: Option<String>,
    },
    Client(String),
}

impl Discovery {
    /// The remembered bridge, or the result of a new search if none is remembered.
    async fn bridge(&self) -> Result<Bridge, DiscoveryFailure> {
        let mut cache = self.cache.lock().await;
        if let Some(bridge) = &cache.bridge {
            return Ok(bridge.clone());
        }
        self.search(&mut cache).await
    }

    /// Replaces `stale`, which stopped answering. If another caller already
    /// replaced it, that replacement is reused instead of searching again.
    async fn refresh(&self, stale: &Bridge) -> Result<Bridge, DiscoveryFailure> {
        let mut cache = self.cache.lock().await;
        if let Some(bridge) = &cache.bridge
            && bridge.generation != stale.generation
        {
            return Ok(bridge.clone());
        }
        self.search(&mut cache).await
    }

    /// Remembers that `bridge` accepted the application key.
    async fn pin(&self, bridge: &Bridge) {
        let mut cache = self.cache.lock().await;
        if cache.pinned_bridge_id.is_none()
            && let Some(id) = &bridge.bridge_id
        {
            tracing::info!(bridge_id = %id, "Hue bridge accepted the key; later searches are restricted to it");
            cache.pinned_bridge_id = Some(id.clone());
        }
    }

    /// Searches the network. A failed search forgets the stale bridge but not the
    /// pinned bridge id.
    async fn search(&self, cache: &mut Cache) -> Result<Bridge, DiscoveryFailure> {
        cache.bridge = None;
        let pinned_bridge_id = match &self.expected_bridge_id {
            Some(_) => None,
            None => cache.pinned_bridge_id.clone(),
        };
        let expected = self.expected_bridge_id.clone().or(pinned_bridge_id.clone());
        let found = (self.find)(expected)
            .await
            .map_err(|error| DiscoveryFailure::Search {
                error,
                pinned_bridge_id,
            })?;
        tracing::info!(bridge_id = %found.bridge_id, host = %found.host, "Hue bridge discovered via mDNS");
        let client = (self.connect)(&found, self.app_key.clone())
            .map_err(|err| DiscoveryFailure::Client(format!("{err:#}")))?;
        cache.generation += 1;
        let bridge = Bridge {
            address: found.host,
            client,
            bridge_id: Some(found.bridge_id),
            generation: cache.generation,
        };
        cache.bridge = Some(bridge.clone());
        Ok(bridge)
    }
}

/// How the bridge address was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressSource {
    /// Set through the `HUE_BRIDGE_HOST` environment variable.
    Configured,
    /// Found on the local network through mDNS.
    Discovered,
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
        if let Some(host) = &config.bridge_host {
            let client =
                HueClient::for_host(host, config.app_key.clone(), config.bridge_id.as_deref())?;
            return Ok(Self::configured(host, client));
        }

        let find: FindFn = Arc::new(|expected_bridge_id: Option<String>| {
            Box::pin(async move {
                discovery::discover(expected_bridge_id.as_deref(), discovery::DISCOVERY_TIMEOUT)
                    .await
            })
        });
        // The certificate must match the announced bridge id (or the configured one,
        // which the search already required the announcement to match).
        let connect: ConnectFn = Arc::new(|found: &DiscoveredBridge, app_key| {
            HueClient::for_host(&found.host, app_key, Some(&found.bridge_id))
        });
        Ok(Self::discovered(config, find, connect))
    }

    fn configured(address: &str, client: HueClient) -> Self {
        Self {
            key_configured: client.has_app_key(),
            source: BridgeSource::Configured(Bridge {
                address: address.to_owned(),
                client,
                bridge_id: None,
                generation: 0,
            }),
        }
    }

    fn discovered(config: &Config, find: FindFn, connect: ConnectFn) -> Self {
        Self {
            key_configured: config.app_key.is_some(),
            source: BridgeSource::Discovered(Arc::new(Discovery {
                app_key: config.app_key.clone(),
                expected_bridge_id: config.bridge_id.clone(),
                find,
                connect,
                cache: Mutex::default(),
            })),
        }
    }

    async fn status(&self) -> BridgeStatus {
        let discovery = match &self.source {
            BridgeSource::Configured(bridge) => {
                return self.check(bridge, AddressSource::Configured).await;
            }
            BridgeSource::Discovered(discovery) => discovery,
        };

        let bridge = match discovery.bridge().await {
            Ok(bridge) => bridge,
            Err(failure) => return self.discovery_failed(failure),
        };
        let status = self.check_discovered(discovery, &bridge).await;
        if status.reachable {
            return status;
        }

        // The remembered address may be stale (new DHCP lease…): search again once.
        match discovery.refresh(&bridge).await {
            Ok(fresh) if fresh.address != bridge.address => {
                self.check_discovered(discovery, &fresh).await
            }
            Ok(_) => status,
            Err(failure) => {
                let mut status = status;
                status.message = format!(
                    "{} A new mDNS search failed too: {}",
                    status.message,
                    self.discovery_failed(failure).message
                );
                status
            }
        }
    }

    /// Checks a discovered bridge and, once it accepts the key, pins its id.
    async fn check_discovered(&self, discovery: &Discovery, bridge: &Bridge) -> BridgeStatus {
        let status = self.check(bridge, AddressSource::Discovered).await;
        if status.authenticated {
            discovery.pin(bridge).await;
        }
        status
    }

    fn discovery_failed(&self, failure: DiscoveryFailure) -> BridgeStatus {
        let message = match failure {
            DiscoveryFailure::Search {
                error: err @ DiscoveryError::NotFoundWithId { .. },
                pinned_bridge_id: Some(pinned),
            } => format!(
                "The bridge used so far ({pinned}) no longer answers: {err}. The server does not send the application key to another bridge. Check the bridge, or set {BRIDGE_ID_ENV} or {BRIDGE_HOST_ENV} and restart the server to use another one."
            ),
            DiscoveryFailure::Search {
                error: err @ DiscoveryError::NotFound,
                ..
            } => format!(
                "{BRIDGE_HOST_ENV} is not set and {err} through mDNS. Check that the server is on the same network as the bridge (a VPN or a virtual machine network can block multicast), or set {BRIDGE_HOST_ENV} to the bridge IP address."
            ),
            DiscoveryFailure::Search {
                error: err @ DiscoveryError::Ambiguous(_),
                ..
            } => format!(
                "{BRIDGE_HOST_ENV} is not set and {err}. Set {BRIDGE_ID_ENV} to pick one, or set {BRIDGE_HOST_ENV}."
            ),
            DiscoveryFailure::Search {
                error: err @ DiscoveryError::NotFoundWithId { .. },
                ..
            } => format!(
                "{BRIDGE_HOST_ENV} is not set and {err}. Check {BRIDGE_ID_ENV}, or set {BRIDGE_HOST_ENV}."
            ),
            DiscoveryFailure::Search {
                error: err @ DiscoveryError::Unavailable(_),
                ..
            } => format!(
                "{BRIDGE_HOST_ENV} is not set and {err}. Set {BRIDGE_HOST_ENV} to the bridge IP address."
            ),
            DiscoveryFailure::Client(err) => {
                format!("A bridge was discovered but the client could not be set up: {err}.")
            }
        };
        BridgeStatus {
            address: None,
            address_source: None,
            reachable: false,
            key_configured: self.key_configured,
            authenticated: false,
            bridge: None,
            message,
        }
    }

    async fn check(&self, bridge: &Bridge, source: AddressSource) -> BridgeStatus {
        let mut status = BridgeStatus {
            address: Some(bridge.address.clone()),
            address_source: Some(source),
            reachable: false,
            key_configured: self.key_configured,
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
        description = "Diagnose the connection to the Philips Hue bridge: whether it is reachable, how its address was found, whether the application key is accepted, and the bridge name, model, id and software version."
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
    use std::{
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;
    use crate::hue::{
        ClientOptions,
        testing::{VALID_KEY, closed_port_url, fake_bridge},
    };

    fn options() -> ClientOptions {
        ClientOptions {
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_millis(1500),
        }
    }

    fn server(base_url: &str, key: Option<&str>) -> HueServer {
        let client = HueClient::new(base_url, key.map(AppKey::new), None, options()).unwrap();
        HueServer::configured("bridge.test", client)
    }

    /// A discovery double: answers each search with the next result, records the
    /// expected ids it was asked for, and talks to found hosts over plain HTTP.
    struct FakeDiscovery {
        results: Arc<StdMutex<Vec<Result<DiscoveredBridge, DiscoveryError>>>>,
        searches: Arc<AtomicUsize>,
        asked_ids: Arc<StdMutex<Vec<Option<String>>>>,
    }

    impl FakeDiscovery {
        fn new(results: Vec<Result<DiscoveredBridge, DiscoveryError>>) -> Self {
            Self {
                results: Arc::new(StdMutex::new(results)),
                searches: Arc::default(),
                asked_ids: Arc::default(),
            }
        }

        fn server(&self, config: &Config) -> HueServer {
            let (results, searches, asked) = (
                self.results.clone(),
                self.searches.clone(),
                self.asked_ids.clone(),
            );
            let find: FindFn = Arc::new(move |expected: Option<String>| {
                searches.fetch_add(1, Ordering::SeqCst);
                asked.lock().unwrap().push(expected.clone());
                let mut results = results.lock().unwrap();
                let next = if results.len() > 1 {
                    results.remove(0)
                } else {
                    results[0].clone()
                };
                // Like the real search: an expected id filters what was found.
                let next =
                    next.and_then(|found| discovery::select(vec![found], expected.as_deref()));
                Box::pin(async move {
                    // Leaves time for concurrent callers to overlap.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    next
                })
            });
            let connect: ConnectFn = Arc::new(|found: &DiscoveredBridge, app_key| {
                HueClient::new(format!("http://{}", found.host), app_key, None, options())
            });
            HueServer::discovered(config, find, connect)
        }

        fn searches(&self) -> usize {
            self.searches.load(Ordering::SeqCst)
        }
    }

    fn found(host: &str) -> Result<DiscoveredBridge, DiscoveryError> {
        found_with_id("ecb5fafffe13af9c", host)
    }

    fn found_with_id(id: &str, host: &str) -> Result<DiscoveredBridge, DiscoveryError> {
        Ok(DiscoveredBridge {
            bridge_id: id.to_owned(),
            host: host.to_owned(),
        })
    }

    fn host_of(url: &str) -> &str {
        url.trim_start_matches("http://")
    }

    fn with_key() -> Config {
        Config {
            app_key: Some(AppKey::new(VALID_KEY)),
            ..Config::default()
        }
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
        assert_eq!(server("http://bridge.test", None).ping(), "pong");
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
    async fn status_never_contains_the_key() {
        let bridge = fake_bridge().await;
        let status = server(&bridge.uri(), Some(VALID_KEY)).status().await;
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains(VALID_KEY), "{json}");
    }

    #[tokio::test]
    async fn discovered_bridge_is_used_when_no_address_is_configured() {
        let bridge = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![found(host_of(&bridge.uri()))]);
        let status = discovery.server(&with_key()).status().await;

        assert!(
            status.reachable && status.authenticated,
            "{}",
            status.message
        );
        assert_eq!(status.address_source, Some(AddressSource::Discovered));
        assert_eq!(status.address.as_deref(), Some(host_of(&bridge.uri())));
    }

    #[tokio::test]
    async fn discovered_address_is_remembered_across_calls_and_clones() {
        let bridge = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![found(host_of(&bridge.uri()))]);
        let server = discovery.server(&with_key());

        assert!(server.status().await.reachable);
        // Each MCP session gets a clone of the server: they share the address.
        assert!(server.clone().status().await.reachable);
        assert_eq!(discovery.searches(), 1);
    }

    #[tokio::test]
    async fn searches_again_when_the_remembered_bridge_stops_answering() {
        let bridge = fake_bridge().await;
        let stale = closed_port_url();
        let discovery =
            FakeDiscovery::new(vec![found(host_of(&stale)), found(host_of(&bridge.uri()))]);
        let status = discovery.server(&with_key()).status().await;

        assert!(
            status.reachable && status.authenticated,
            "{}",
            status.message
        );
        assert_eq!(status.address.as_deref(), Some(host_of(&bridge.uri())));
        assert_eq!(discovery.searches(), 2);
    }

    #[tokio::test]
    async fn reports_when_no_bridge_is_found() {
        let discovery = FakeDiscovery::new(vec![Err(DiscoveryError::NotFound)]);
        let status = discovery.server(&with_key()).status().await;

        assert!(!status.reachable && !status.authenticated);
        assert!(status.key_configured);
        assert_eq!(status.address, None);
        assert!(
            status.message.contains(BRIDGE_HOST_ENV),
            "{}",
            status.message
        );
    }

    #[tokio::test]
    async fn failed_searches_are_not_remembered() {
        let bridge = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![
            Err(DiscoveryError::NotFound),
            found(host_of(&bridge.uri())),
        ]);
        let server = discovery.server(&with_key());

        assert!(!server.status().await.reachable);
        assert!(server.status().await.reachable);
    }

    #[tokio::test]
    async fn lists_bridges_when_several_are_found() {
        let bridges = vec![
            DiscoveredBridge {
                bridge_id: "abc".to_owned(),
                host: "192.168.1.7".to_owned(),
            },
            DiscoveredBridge {
                bridge_id: "def".to_owned(),
                host: "192.168.1.8".to_owned(),
            },
        ];
        let discovery = FakeDiscovery::new(vec![Err(DiscoveryError::Ambiguous(bridges))]);
        let status = discovery.server(&with_key()).status().await;

        assert!(!status.reachable);
        for expected in ["abc at 192.168.1.7", "def at 192.168.1.8", BRIDGE_ID_ENV] {
            assert!(status.message.contains(expected), "{}", status.message);
        }
    }

    #[tokio::test]
    async fn search_looks_for_the_configured_bridge_id() {
        let discovery = FakeDiscovery::new(vec![Err(DiscoveryError::NotFound)]);
        let config = Config {
            bridge_id: Some("ecb5fafffe13af9c".to_owned()),
            ..with_key()
        };
        discovery.server(&config).status().await;

        assert_eq!(
            *discovery.asked_ids.lock().unwrap(),
            [Some("ecb5fafffe13af9c".to_owned())]
        );
    }

    #[tokio::test]
    async fn never_switches_to_another_bridge_once_one_accepted_the_key() {
        let first = fake_bridge().await;
        let other = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![
            found_with_id("aaaa", host_of(&first.uri())),
            found_with_id("bbbb", host_of(&other.uri())),
        ]);
        let server = discovery.server(&with_key());
        assert!(server.status().await.authenticated);

        first.reset().await; // the bridge that accepted the key stops answering
        let status = server.status().await;

        assert!(!status.reachable && !status.authenticated);
        assert!(
            status.message.contains("(aaaa) no longer answers")
                && status.message.contains("does not send the application key"),
            "{}",
            status.message
        );
        assert_eq!(
            *discovery.asked_ids.lock().unwrap(),
            [None, Some("aaaa".to_owned())]
        );
        assert!(other.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn keeps_the_pinned_id_across_failed_searches() {
        let first = fake_bridge().await;
        let other = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![
            found_with_id("aaaa", host_of(&first.uri())),
            Err(DiscoveryError::NotFound),
            found_with_id("bbbb", host_of(&other.uri())),
        ]);
        let server = discovery.server(&with_key());
        assert!(server.status().await.authenticated);

        first.reset().await;
        assert!(!server.status().await.reachable); // search finds nothing
        assert!(!server.status().await.reachable); // search finds another bridge

        assert_eq!(
            discovery
                .asked_ids
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .as_deref(),
            Some("aaaa")
        );
        assert!(other.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn does_not_pin_a_bridge_that_rejected_the_key() {
        let first = fake_bridge().await;
        let other = fake_bridge().await;
        let discovery = FakeDiscovery::new(vec![
            found_with_id("aaaa", host_of(&first.uri())),
            found_with_id("bbbb", host_of(&other.uri())),
        ]);
        let config = Config {
            app_key: Some(AppKey::new("wrong-key")),
            ..Config::default()
        };
        let server = discovery.server(&config);
        assert!(!server.status().await.authenticated);

        first.reset().await;
        let status = server.status().await;

        assert!(status.reachable, "{}", status.message);
        assert_eq!(status.address.as_deref(), Some(host_of(&other.uri())));
        assert_eq!(*discovery.asked_ids.lock().unwrap(), [None, None]);
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_search() {
        let bridge = fake_bridge().await;
        let stale = closed_port_url();
        let discovery =
            FakeDiscovery::new(vec![found(host_of(&stale)), found(host_of(&bridge.uri()))]);
        let server = discovery.server(&with_key());
        let other_session = server.clone();

        let (first, second) = tokio::join!(server.status(), other_session.status());

        assert!(first.reachable && second.reachable);
        // One search for the first address, one to replace it: not one per caller.
        assert_eq!(discovery.searches(), 2);
    }
}
