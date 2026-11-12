//! Discovery of Hue bridges on the local network through mDNS.
//!
//! Bridges announce the `_hue._tcp.local.` service, with their bridge id in the
//! `bridgeid` TXT property. The id is later checked against the bridge
//! certificate, so a device that merely announces itself as a bridge is not
//! trusted.

use std::{
    fmt,
    net::IpAddr,
    time::{Duration, Instant},
};

use mdns_sd::{ServiceDaemon, ServiceEvent};

const HUE_SERVICE: &str = "_hue._tcp.local.";
const BRIDGE_ID_PROPERTY: &str = "bridgeid";
const HTTPS_PORT: u16 = 443;

/// How long to listen for announcements. Answers usually come within a second.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// A bridge found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredBridge {
    /// Lowercase bridge id, as announced.
    pub bridge_id: String,
    /// Address to reach the bridge: `ip` or `ip:port`.
    pub host: String,
}

impl fmt::Display for DiscoveredBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.bridge_id, self.host)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("mDNS discovery could not run: {0}")]
    Unavailable(String),
    #[error("no Hue bridge answered on the local network")]
    NotFound,
    #[error("no Hue bridge with id {expected} was found (found: {})", list(.found))]
    NotFoundWithId {
        expected: String,
        found: Vec<DiscoveredBridge>,
    },
    #[error("several Hue bridges were found: {}", list(.0))]
    Ambiguous(Vec<DiscoveredBridge>),
}

fn list(bridges: &[DiscoveredBridge]) -> String {
    bridges
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a bridge announcement carries, independent of the mDNS library.
#[derive(Debug, Clone)]
struct Announcement {
    bridge_id: Option<String>,
    addresses: Vec<IpAddr>,
    port: u16,
}

impl Announcement {
    /// The bridge behind this announcement, if it is usable.
    fn bridge(&self) -> Option<DiscoveredBridge> {
        let bridge_id = self.bridge_id.as_ref()?.trim().to_ascii_lowercase();
        if bridge_id.is_empty() {
            return None;
        }
        // IPv4 first; link-local IPv6 needs an interface scope we cannot express here.
        let address = self
            .addresses
            .iter()
            .find(|ip| ip.is_ipv4())
            .or_else(|| self.addresses.iter().find(|ip| is_routable_v6(ip)))?;
        let address = match address {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let host = if self.port == HTTPS_PORT || self.port == 0 {
            address
        } else {
            format!("{address}:{}", self.port)
        };
        Some(DiscoveredBridge { bridge_id, host })
    }
}

fn is_routable_v6(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80)
}

/// Keeps one entry per bridge id; a later announcement updates the address.
fn add(found: &mut Vec<DiscoveredBridge>, announcement: &Announcement) {
    let Some(bridge) = announcement.bridge() else {
        return;
    };
    match found.iter_mut().find(|b| b.bridge_id == bridge.bridge_id) {
        Some(existing) => *existing = bridge,
        None => found.push(bridge),
    }
}

/// Picks the bridge to use among those found.
pub fn select(
    found: Vec<DiscoveredBridge>,
    expected_bridge_id: Option<&str>,
) -> Result<DiscoveredBridge, DiscoveryError> {
    if let Some(expected) = expected_bridge_id {
        let expected = expected.trim().to_ascii_lowercase();
        return match found.iter().position(|b| b.bridge_id == expected) {
            Some(index) => Ok(found.into_iter().nth(index).expect("index is in range")),
            None => Err(DiscoveryError::NotFoundWithId { expected, found }),
        };
    }
    match found.len() {
        0 => Err(DiscoveryError::NotFound),
        1 => Ok(found.into_iter().next().expect("one bridge")),
        _ => Err(DiscoveryError::Ambiguous(found)),
    }
}

/// Listens for bridge announcements for up to `timeout`. Returns early once the
/// expected bridge is found; otherwise listens for the whole window, so that
/// several bridges can be detected.
pub async fn discover(
    expected_bridge_id: Option<&str>,
    timeout: Duration,
) -> Result<DiscoveredBridge, DiscoveryError> {
    let daemon =
        ServiceDaemon::new().map_err(|err| DiscoveryError::Unavailable(err.to_string()))?;
    let found = listen(&daemon, expected_bridge_id, timeout).await;
    // Stops the background thread; nothing useful to do if it fails.
    let _ = daemon.shutdown();
    select(found?, expected_bridge_id)
}

async fn listen(
    daemon: &ServiceDaemon,
    expected_bridge_id: Option<&str>,
    timeout: Duration,
) -> Result<Vec<DiscoveredBridge>, DiscoveryError> {
    let receiver = daemon
        .browse(HUE_SERVICE)
        .map_err(|err| DiscoveryError::Unavailable(err.to_string()))?;
    let expected = expected_bridge_id.map(|id| id.trim().to_ascii_lowercase());
    let deadline = Instant::now() + timeout;
    let mut found = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, receiver.recv_async()).await else {
            break; // listening window is over
        };
        let Ok(event) = event else {
            break; // the daemon stopped
        };
        if let ServiceEvent::ServiceResolved(service) = event {
            add(
                &mut found,
                &Announcement {
                    bridge_id: service
                        .get_property_val_str(BRIDGE_ID_PROPERTY)
                        .map(str::to_owned),
                    addresses: service
                        .get_addresses()
                        .iter()
                        .map(|ip| ip.to_ip_addr())
                        .collect(),
                    port: service.get_port(),
                },
            );
            if let Some(expected) = &expected
                && found.iter().any(|b| &b.bridge_id == expected)
            {
                break;
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement(id: Option<&str>, addresses: &[&str], port: u16) -> Announcement {
        Announcement {
            bridge_id: id.map(str::to_owned),
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
            port,
        }
    }

    fn bridge(id: &str, host: &str) -> DiscoveredBridge {
        DiscoveredBridge {
            bridge_id: id.to_owned(),
            host: host.to_owned(),
        }
    }

    #[test]
    fn announcement_prefers_ipv4_and_lowercases_the_id() {
        let a = announcement(
            Some("ECB5FAFFFE13AF9C"),
            &["fe80::1", "2a01:e0a::1", "192.168.1.7"],
            443,
        );
        assert_eq!(a.bridge(), Some(bridge("ecb5fafffe13af9c", "192.168.1.7")));
    }

    #[test]
    fn announcement_keeps_a_non_default_port() {
        let a = announcement(Some("abc"), &["192.168.1.7"], 8443);
        assert_eq!(a.bridge().unwrap().host, "192.168.1.7:8443");
    }

    #[test]
    fn announcement_falls_back_to_routable_ipv6_only() {
        let a = announcement(Some("abc"), &["fe80::1", "2a01:e0a::1"], 443);
        assert_eq!(a.bridge().unwrap().host, "[2a01:e0a::1]");

        let link_local_only = announcement(Some("abc"), &["fe80::1"], 443);
        assert_eq!(link_local_only.bridge(), None);
    }

    #[test]
    fn announcement_without_bridge_id_is_ignored() {
        assert_eq!(announcement(None, &["192.168.1.7"], 443).bridge(), None);
        assert_eq!(
            announcement(Some(" "), &["192.168.1.7"], 443).bridge(),
            None
        );
    }

    #[test]
    fn repeated_announcements_keep_one_entry_per_bridge() {
        let mut found = Vec::new();
        add(
            &mut found,
            &announcement(Some("abc"), &["192.168.1.7"], 443),
        );
        add(
            &mut found,
            &announcement(Some("ABC"), &["192.168.1.8"], 443),
        );
        add(&mut found, &announcement(None, &["192.168.1.9"], 443));
        assert_eq!(found, [bridge("abc", "192.168.1.8")]);
    }

    #[test]
    fn select_single_bridge() {
        let found = vec![bridge("abc", "192.168.1.7")];
        assert_eq!(select(found, None), Ok(bridge("abc", "192.168.1.7")));
    }

    #[test]
    fn select_reports_no_bridge() {
        assert_eq!(select(Vec::new(), None), Err(DiscoveryError::NotFound));
    }

    #[test]
    fn select_lists_bridges_when_ambiguous() {
        let found = vec![bridge("abc", "192.168.1.7"), bridge("def", "192.168.1.8")];
        let err = select(found.clone(), None).unwrap_err();
        assert_eq!(err, DiscoveryError::Ambiguous(found));
        let message = err.to_string();
        assert!(message.contains("abc at 192.168.1.7") && message.contains("def at 192.168.1.8"));
    }

    #[test]
    fn select_uses_the_expected_id_ignoring_case() {
        let found = vec![bridge("abc", "192.168.1.7"), bridge("def", "192.168.1.8")];
        assert_eq!(select(found, Some("DEF")), Ok(bridge("def", "192.168.1.8")));
    }

    #[test]
    fn select_reports_a_missing_expected_id() {
        let found = vec![bridge("abc", "192.168.1.7")];
        let err = select(found, Some("def")).unwrap_err();
        assert!(matches!(err, DiscoveryError::NotFoundWithId { .. }));
        assert!(err.to_string().contains("abc at 192.168.1.7"), "{err}");
    }

    /// Needs a Hue bridge on the local network: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a real Hue bridge on the local network"]
    async fn discovers_a_real_bridge() {
        let found = discover(None, DISCOVERY_TIMEOUT).await;
        println!("discovery result: {found:?}");
        found.unwrap();
    }
}
