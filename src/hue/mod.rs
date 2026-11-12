//! Access to the Philips Hue bridge.

mod client;
#[cfg(test)]
pub mod testing;
mod tls;

#[cfg(test)]
pub use client::ClientOptions;
pub use client::{BridgeInfo, HueClient, HueError};
